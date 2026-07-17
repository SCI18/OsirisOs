// entropyd — Assessor #5, SystemCore
// Entropy pool management, RNG seeding
// "Randomness is measured. The seed is weighed."

use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 30000; // 30 seconds
const FLUSH_INTERVAL_MS: u64 = 5000;
const ENTROPY_AVAIL_PATH: &str = "/proc/sys/kernel/random/entropy_avail";
const POOLSIZE_PATH: &str = "/proc/sys/kernel/random/poolsize";
const URANDOM_PATH: &str = "/dev/urandom";
const RANDOM_PATH: &str = "/dev/random";
const MIN_ENTROPY_WARNING: usize = 1024; // bits
const MIN_ENTROPY_CRITICAL: usize = 256; // bits
const SEED_FILE_PATH: &str = "/var/lib/osiris/entropy_seed";

/// Entropyd state
struct Entropyd {
    socket: Arc<Mutex<Option<UnixStream>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<std::collections::HashMap<String, (AlertSeverity, u64)>>>,
    last_seed_time: Arc<Mutex<Option<SystemTime>>>,
}

impl Entropyd {
    fn new() -> Result<Self> {
        // Ensure seed directory exists
        fs::create_dir_all("/var/lib/osiris")?;
        
        Ok(Self {
            socket: Arc::new(Mutex::new(None)),
            daemon_name: "entropyd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(std::collections::HashMap::new())),
            last_seed_time: Arc::new(Mutex::new(None)),
        })
    }

    async fn connect_to_bridge(&self) -> Result<()> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let mut socket_guard = self.socket.lock().await;
        *socket_guard = Some(stream);
        info!("[entropyd] Connected to AkerNet Bridge at {}", SOCKET_PATH);
        Ok(())
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
        let mut socket_guard = self.socket.lock().await;
        if let Some(stream) = socket_guard.as_mut() {
            stream.write_all(frame.as_bytes()).await?;
            stream.flush().await?;
        }
        Ok(())
    }

    async fn handle_bridge_messages(&self) -> Result<()> {
        let mut stream_opt = self.socket.lock().await.take();

        if let Some(mut stream) = stream_opt.take() {
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            loop {
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Ok(bridge_msg) = Frame::decode_bridge_message(&line) {
                            self.handle_bridge_message(bridge_msg).await?;
                        }
                        line.clear();
                    }
                    Err(e) => {
                        debug!("[entropyd] Socket read error: {}", e);
                        break;
                    }
                }
            }
            *self.socket.lock().await = Some(stream);
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

    /// Emit an alert via DaemonMessage::Alert with deduplication
    async fn emit_alert(&self, alert_key: &str, severity: AlertSeverity, payload: Value) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        
        // Check deduplication
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
                        return Ok(()); // Suppress duplicate/lower severity
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

    /// Read entropy available from /proc
    fn read_entropy_avail() -> Result<usize> {
        let content = fs::read_to_string(ENTROPY_AVAIL_PATH)?;
        Ok(content.trim().parse::<usize>().unwrap_or(0))
    }

    /// Read pool size from /proc
    fn read_pool_size() -> Result<usize> {
        let content = fs::read_to_string(POOLSIZE_PATH)?;
        Ok(content.trim().parse::<usize>().unwrap_or(4096))
    }

    /// Load seed from file and feed to kernel
    async fn load_seed(&self) -> Result<()> {
        if Path::new(SEED_FILE_PATH).exists() {
            let mut seed = Vec::new();
            fs::File::open(SEED_FILE_PATH)?.read_to_end(&mut seed)?;
            if !seed.is_empty() {
                // Write to /dev/urandom to credit entropy
                let mut urandom = fs::OpenOptions::new().write(true).open(URANDOM_PATH)?;
                urandom.write_all(&seed)?;
                urandom.flush()?;
                info!("[entropyd] Loaded {} bytes of entropy seed", seed.len());
            }
        }
        Ok(())
    }

    /// Save current entropy to seed file for next boot
    async fn save_seed(&self) -> Result<()> {
        let mut seed = vec![0u8; 512];
        let mut random = fs::OpenOptions::new().read(true).open(RANDOM_PATH)?;
        random.read_exact(&mut seed)?;
        
        fs::write(SEED_FILE_PATH, &seed)?;
        *self.last_seed_time.lock().await = Some(SystemTime::now());
        info!("[entropyd] Saved entropy seed ({} bytes)", seed.len());
        Ok(())
    }

    /// Check entropy levels and reseed if needed
    async fn check_entropy(&self) -> Result<()> {
        let entropy_avail = Self::read_entropy_avail()?;
        let pool_size = Self::read_pool_size()?;
        
        let entropy_pct = if pool_size > 0 {
            (entropy_avail as f32 / pool_size as f32) * 100.0
        } else { 0.0 };

        // Check thresholds
        if entropy_avail <= MIN_ENTROPY_CRITICAL {
            self.emit_alert("entropy_critical", AlertSeverity::Critical, json!({
                "metric": "entropy_available",
                "value": entropy_avail,
                "pool_size": pool_size,
                "percentage": entropy_pct,
                "threshold": MIN_ENTROPY_CRITICAL,
                "unit": "bits",
                "message": format!("Entropy critically low: {} bits ({:.1}% of pool)", entropy_avail, entropy_pct),
            })).await?;
        } else if entropy_avail <= MIN_ENTROPY_WARNING {
            self.emit_alert("entropy_warning", AlertSeverity::Warning, json!({
                "metric": "entropy_available",
                "value": entropy_avail,
                "pool_size": pool_size,
                "percentage": entropy_pct,
                "threshold": MIN_ENTROPY_WARNING,
                "unit": "bits",
                "message": format!("Entropy low: {} bits ({:.1}% of pool)", entropy_avail, entropy_pct),
            })).await?;
        }

        // Save seed periodically (every 5 minutes)
        let should_save = {
            let last_seed = self.last_seed_time.lock().await;
            last_seed.map_or(true, |t| t.elapsed().unwrap_or(Duration::MAX) > Duration::from_secs(300))
        };
        
        if should_save {
            self.save_seed().await?;
        }

        // Periodic status update
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

    let entropyd = Entropyd::new()?;
    
    // Load seed on startup
    entropyd.load_seed().await?;
    
    entropyd.connect_to_bridge().await?;
    entropyd.register().await?;

    // Wait for acknowledgment with timeout
    tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        let mut interval = interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if let Err(e) = entropyd.handle_bridge_messages().await {
                debug!("[entropyd] Bridge message handler error: {}", e);
            }
        }
    }).await.ok(); // Ignore timeout, continue if registered

    info!("[entropyd] Registration complete, entering entropy monitoring loop");

    // Main monitoring loop
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
                if let Err(e) = entropyd.handle_bridge_messages().await {
                    debug!("[entropyd] Bridge message handler error: {}", e);
                }
            }
        }
    }
}