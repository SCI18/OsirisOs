// entropyd — Assessor #5, SystemCore
// Entropy pool management, RNG seeding
// "Randomness is measured. The seed is weighed."

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::{json, Value};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, AsyncReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 30000; // 30 seconds
const FLUSH_INTERVAL_MS: u64 = 5000;
const ENTROPY_AVAIL_PATH: &str = "/proc/sys/kernel/random/entropy_avail";
const POOLSIZE_PATH: &str = "/proc/sys/kernel/random/poolsize";
/// FIX: both load and save now consistently use /dev/urandom. The prior
/// version read from /dev/random in save_seed(), which blocks indefinitely
/// when the entropy pool is critically low — exactly the condition this
/// daemon exists to detect, meaning it could hang precisely when it most
/// needed to stay responsive. /dev/urandom never blocks on Linux once
/// initially seeded at boot, so it's the correct choice for periodic
/// seed-saving (as opposed to /dev/random's blocking guarantee, which
/// exists for a different use case entirely — never appropriate for a
/// background daemon's own upkeep).
const URANDOM_PATH: &str = "/dev/urandom";
const SEED_FILE_PATH: &str = "/var/lib/osiris/entropy.seed";

/// FIX: reconciled against the decision log and Systems Spec audit —
/// warning at 1024 bits, critical at 192 bits. (Log text at one point said
/// "1000/192"; code had 1024/256 originally, then 1024/192 after the
/// self-fix pass. This is the confirmed-current, intentional value —
/// documented explicitly here so a future reader doesn't have to
/// reverse-engineer which number is authoritative.)
const MIN_ENTROPY_WARNING: usize = 1024;
const MIN_ENTROPY_CRITICAL: usize = 192;

const SEED_SAVE_INTERVAL_SECS: u64 = 300; // 5 minutes

enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

/// Entropyd state
struct Entropyd {
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<std::collections::HashMap<String, (AlertSeverity, u64)>>>,
    last_seed_time: Arc<Mutex<Option<SystemTime>>>,
    /// FIX: track consecutive /proc read failures so we log at warning
    /// level with dedup rather than emitting a fresh `error!` on every
    /// single poll cycle when /proc/sys/kernel/random is inaccessible
    /// under some proot configuration — noisy and undiagnosed before.
    proc_read_failures: Arc<Mutex<u32>>,
}

impl Entropyd {
    async fn new() -> Result<Self> {
        fs::create_dir_all("/var/lib/osiris").await?;

        Ok(Self {
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "entropyd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(std::collections::HashMap::new())),
            last_seed_time: Arc::new(Mutex::new(None)),
            proc_read_failures: Arc::new(Mutex::new(0)),
        })
    }

    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[entropyd] Connected to AkerNet Bridge at {}", SOCKET_PATH);

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => { let _ = tx.send(ReaderEvent::Closed).await; break; }
                    Ok(_) => {
                        if let Ok(bridge_msg) = Frame::decode_bridge_message(&line) {
                            if tx.send(ReaderEvent::Bridge(bridge_msg)).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        debug!("[entropyd] Socket read error: {}", e);
                        let _ = tx.send(ReaderEvent::Closed).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }

    async fn register(&self) -> Result<()> {
        let msg = DaemonMessage::Register {
            name: self.daemon_name.clone(),
            pid: self.pid,
            version: VERSION.to_string(),
        };
        self.send_frame(msg).await?;
        debug!("[entropyd] Registration sent");
        Ok(())
    }

    async fn send_frame(&self, msg: DaemonMessage) -> Result<()> {
        let frame = Frame::encode(&msg).map_err(|e| anyhow::anyhow!(e))?;
        let mut guard = self.socket_write_half.lock().await;
        if let Some(writer) = guard.as_mut() {
            writer.write_all(frame.as_bytes()).await?;
            writer.flush().await?;
        }
        Ok(())
    }

    async fn handle_bridge_message(&self, msg: BridgeMessage) -> Result<()> {
        match msg {
            BridgeMessage::Acknowledged { name } => {
                info!("[entropyd] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[entropyd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[entropyd] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[entropyd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[entropyd] Shutting down gracefully, saving entropy seed");
        self.save_seed().await?;
        let msg = DaemonMessage::Shutdown {
            name: self.daemon_name.clone(),
            reason: "normal shutdown".to_string(),
        };
        self.send_frame(msg).await?;
        Ok(())
    }

    async fn emit_alert(&self, alert_key: &str, severity: AlertSeverity, payload: Value) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

        {
            let mut last_alerts = self.last_alerts.lock().await;
            if let Some((last_severity, last_time)) = last_alerts.get(alert_key) {
                if now - last_time < 60 {
                    let severity_order = match severity {
                        AlertSeverity::Info => 1,
                        AlertSeverity::Warning => 2,
                        AlertSeverity::Critical => 3,
                    };
                    let last_order = match last_severity {
                        AlertSeverity::Info => 1,
                        AlertSeverity::Warning => 2,
                        AlertSeverity::Critical => 3,
                    };
                    if severity_order <= last_order {
                        return Ok(());
                    }
                }
            }
            last_alerts.insert(alert_key.to_string(), (severity.clone(), now));
        }

        let timestamp = DateTime::from_timestamp(now as i64, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| now.to_string());

        let msg = DaemonMessage::Alert {
            name: self.daemon_name.clone(),
            severity: severity.clone(),
            payload,
            timestamp,
        };
        self.send_frame(msg).await?;
        info!("[entropyd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Read entropy available from /proc. FIX: uses tokio::fs (genuinely
    /// async) instead of std::fs (blocking) so this no longer stalls the
    /// worker thread, however briefly, on every poll.
    async fn read_entropy_avail() -> Result<usize> {
        let content = fs::read_to_string(ENTROPY_AVAIL_PATH).await?;
        Ok(content.trim().parse::<usize>().unwrap_or(0))
    }

    async fn read_pool_size() -> Result<usize> {
        let content = fs::read_to_string(POOLSIZE_PATH).await?;
        Ok(content.trim().parse::<usize>().unwrap_or(4096))
    }

    /// Load seed from file and feed to kernel via /dev/urandom.
    async fn load_seed(&self) -> Result<()> {
        let exists = fs::metadata(SEED_FILE_PATH).await.is_ok();
        if exists {
            let mut seed = Vec::new();
            fs::File::open(SEED_FILE_PATH).await?.read_to_end(&mut seed).await?;
            if !seed.is_empty() {
                let mut urandom = fs::OpenOptions::new().write(true).open(URANDOM_PATH).await?;
                urandom.write_all(&seed).await?;
                urandom.flush().await?;
                info!("[entropyd] Loaded {} bytes of entropy seed", seed.len());
            }
        }
        Ok(())
    }

    /// Save current entropy to seed file for next boot, reading from
    /// /dev/urandom (non-blocking, safe to call from an async context even
    /// under low-entropy conditions).
    async fn save_seed(&self) -> Result<()> {
        let mut seed = vec![0u8; 512];
        let mut urandom = fs::OpenOptions::new().read(true).open(URANDOM_PATH).await?;
        urandom.read_exact(&mut seed).await?;

        fs::write(SEED_FILE_PATH, &seed).await?;
        *self.last_seed_time.lock().await = Some(SystemTime::now());
        info!("[entropyd] Saved entropy seed ({} bytes)", seed.len());
        Ok(())
    }

    /// Check entropy levels and reseed if needed
    async fn check_entropy(&self) -> Result<()> {
        let entropy_avail = match Self::read_entropy_avail().await {
            Ok(v) => {
                *self.proc_read_failures.lock().await = 0;
                v
            }
            Err(e) => {
                let mut failures = self.proc_read_failures.lock().await;
                *failures += 1;
                // Only warn (not error-spam) and only once per several
                // consecutive failures, since a single transient miss under
                // proot isn't necessarily meaningful.
                if *failures == 1 || *failures % 10 == 0 {
                    warn!("[entropyd] Failed to read {} ({} consecutive failures): {}",
                        ENTROPY_AVAIL_PATH, *failures, e);
                }
                return Ok(()); // degrade gracefully rather than propagating an error every cycle
            }
        };
        let pool_size = Self::read_pool_size().await.unwrap_or(4096);

        let entropy_pct = if pool_size > 0 {
            (entropy_avail as f32 / pool_size as f32) * 100.0
        } else { 0.0 };

        if entropy_avail <= MIN_ENTROPY_CRITICAL {
            self.emit_alert("entropy_critical", AlertSeverity::Critical, json!({
                "metric": "entropy_available", "value": entropy_avail, "pool_size": pool_size,
                "percentage": entropy_pct, "threshold": MIN_ENTROPY_CRITICAL, "unit": "bits",
                "message": format!("Entropy critically low: {} bits ({:.1}% of pool)", entropy_avail, entropy_pct),
            })).await?;
        } else if entropy_avail <= MIN_ENTROPY_WARNING {
            self.emit_alert("entropy_warning", AlertSeverity::Warning, json!({
                "metric": "entropy_available", "value": entropy_avail, "pool_size": pool_size,
                "percentage": entropy_pct, "threshold": MIN_ENTROPY_WARNING, "unit": "bits",
                "message": format!("Entropy low: {} bits ({:.1}% of pool)", entropy_avail, entropy_pct),
            })).await?;
        }

        let should_save = {
            let last_seed = self.last_seed_time.lock().await;
            last_seed.map_or(true, |t| t.elapsed().unwrap_or(Duration::MAX).as_secs() > SEED_SAVE_INTERVAL_SECS)
        };

        if should_save {
            self.save_seed().await?;
        }

        let status = DaemonMessage::StatusUpdate {
            name: self.daemon_name.clone(),
            status: DaemonStatus::Running,
        };
        self.send_frame(status).await?;

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("[entropyd] Osiris Entropy Daemon v{} starting", VERSION);

    let entropyd = Entropyd::new().await?;

    entropyd.load_seed().await?;

    let mut reader_rx = entropyd.connect_to_bridge().await?;
    entropyd.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = entropyd.handle_bridge_message(msg).await {
                        debug!("[entropyd] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[entropyd] Registration complete, entering entropy monitoring loop");

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = entropyd.check_entropy().await {
                    error!("[entropyd] Entropy check error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                // retained for symmetry; no buffered writes to flush here
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = entropyd.handle_bridge_message(msg).await {
                            debug!("[entropyd] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[entropyd] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
