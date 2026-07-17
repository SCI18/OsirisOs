// timed — Assessor #4, SystemCore
// System time, NTP synchronization
// "Time is measured. The moment is weighed."

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::{DateTime, Utc};
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 60000; // 60 seconds
const FLUSH_INTERVAL_MS: u64 = 5000;

/// FIX: previously a single hardcoded NTP server was a single point of
/// failure — if pool.ntp.org was unreachable (DNS/firewall/offline), timed
/// would just log a warning and do nothing further until the next poll.
/// Now tries each server in order until one responds.
const NTP_SERVERS: &[&str] = &[
    "pool.ntp.org",
    "time.google.com",
    "time.cloudflare.com",
];

const TIME_DRIFT_WARNING_MS: i64 = 1000; // 1 second
const TIME_DRIFT_CRITICAL_MS: i64 = 5000; // 5 seconds
/// If no successful sync has happened in this long, alert regardless of
/// per-attempt warnings — catches the case where every attempt fails
/// individually (each logged as a one-off warning) but the daemon has
/// silently been unable to sync for an extended period.
const SYNC_STALE_THRESHOLD_SECS: u64 = 600; // 10 minutes

enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

/// Timed state
struct Timed {
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_ntp_sync: Arc<Mutex<Option<SystemTime>>>,
    last_alerts: Arc<Mutex<std::collections::HashMap<String, (AlertSeverity, u64)>>>,
}

impl Timed {
    fn new() -> Result<Self> {
        Ok(Self {
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "timed".to_string(),
            pid: std::process::id(),
            last_ntp_sync: Arc::new(Mutex::new(None)),
            last_alerts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[timed] Connected to AkerNet Bridge at {}", SOCKET_PATH);

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
                        debug!("[timed] Socket read error: {}", e);
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
        debug!("[timed] Registration sent");
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
                info!("[timed] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[timed] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[timed] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[timed] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[timed] Shutting down gracefully");
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
        info!("[timed] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Query a single NTP server on a blocking thread.
    /// FIX: `ntp::request()` is synchronous network I/O. Calling it directly
    /// inside an async fn blocks the entire tokio worker thread for the
    /// round-trip duration, during which timed cannot process Bridge
    /// messages (Stop/Reload/etc). Wrapped in spawn_blocking so the async
    /// runtime stays responsive regardless of how long the NTP query takes
    /// or hangs.
    async fn query_ntp_server(server: &'static str) -> Result<(SystemTime, u64)> {
        let start = SystemTime::now();
        let result = tokio::task::spawn_blocking(move || ntp::request(server))
            .await
            .map_err(|e| anyhow::anyhow!("NTP query task panicked: {}", e))?;
        let rtt = start.elapsed()?.as_millis() as u64;

        let response = result.map_err(|e| anyhow::anyhow!("NTP query to {} failed: {}", server, e))?;
        let ntp_secs = response.transmit_time.sec as i64;
        let ntp_frac = response.transmit_time.frac as f64 / u32::MAX as f64;
        let ntp_time = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(ntp_secs as f64 + ntp_frac);

        Ok((ntp_time, rtt))
    }

    /// Query NTP servers in order (with fallback) and check system time drift
    async fn check_time_sync(&self) -> Result<()> {
        let system_now = SystemTime::now();

        let mut result: Option<(&str, SystemTime, u64)> = None;
        let mut last_err: Option<String> = None;

        for &server in NTP_SERVERS {
            match Self::query_ntp_server(server).await {
                Ok((ntp_time, rtt)) => {
                    result = Some((server, ntp_time, rtt));
                    break;
                }
                Err(e) => {
                    warn!("[timed] NTP query to {} failed: {}", server, e);
                    last_err = Some(e.to_string());
                }
            }
        }

        let Some((server_used, ntp_time, rtt)) = result else {
            self.emit_alert("ntp_failed", AlertSeverity::Warning, json!({
                "metric": "ntp_query",
                "servers_tried": NTP_SERVERS,
                "error": last_err.unwrap_or_else(|| "all servers failed".to_string()),
                "message": "Failed to query any configured NTP server",
            })).await?;

            // FIX: last_ntp_sync was previously written but never read
            // anywhere. Now actually used: if it's been too long since the
            // last *successful* sync, escalate beyond the per-attempt
            // warning above, since repeated individual failures over a long
            // window is a more serious condition than one blip.
            let stale = {
                let last_sync = self.last_ntp_sync.lock().await;
                match *last_sync {
                    Some(t) => t.elapsed().unwrap_or(Duration::MAX).as_secs() > SYNC_STALE_THRESHOLD_SECS,
                    None => false, // never synced yet this run — not "stale", just not-yet-synced
                }
            };
            if stale {
                self.emit_alert("ntp_sync_stale", AlertSeverity::Critical, json!({
                    "metric": "ntp_sync_stale",
                    "threshold_secs": SYNC_STALE_THRESHOLD_SECS,
                    "message": format!("No successful NTP sync in over {}s", SYNC_STALE_THRESHOLD_SECS),
                })).await?;
            }

            let status = DaemonMessage::StatusUpdate {
                name: self.daemon_name.clone(),
                status: DaemonStatus::Running,
            };
            self.send_frame(status).await?;
            return Ok(());
        };

        *self.last_ntp_sync.lock().await = Some(system_now);

        let drift = ntp_time.duration_since(system_now).unwrap_or(Duration::from_secs(0));
        let drift_ms = drift.as_millis() as i64;
        let drift_abs = drift_ms.abs();

        if drift_abs >= TIME_DRIFT_CRITICAL_MS {
            self.emit_alert("time_drift_critical", AlertSeverity::Critical, json!({
                "metric": "time_drift", "drift_ms": drift_ms, "threshold_ms": TIME_DRIFT_CRITICAL_MS,
                "server": server_used,
                "system_time": DateTime::<Utc>::from(system_now).to_rfc3339(),
                "ntp_time": DateTime::<Utc>::from(ntp_time).to_rfc3339(),
                "rtt_ms": rtt,
                "message": format!("System time drift critical: {}ms (via {})", drift_ms, server_used),
            })).await?;
        } else if drift_abs >= TIME_DRIFT_WARNING_MS {
            self.emit_alert("time_drift_warning", AlertSeverity::Warning, json!({
                "metric": "time_drift", "drift_ms": drift_ms, "threshold_ms": TIME_DRIFT_WARNING_MS,
                "server": server_used,
                "system_time": DateTime::<Utc>::from(system_now).to_rfc3339(),
                "ntp_time": DateTime::<Utc>::from(ntp_time).to_rfc3339(),
                "rtt_ms": rtt,
                "message": format!("System time drift detected: {}ms (via {})", drift_ms, server_used),
            })).await?;
        } else {
            debug!("[timed] Time sync OK via {}: drift={}ms, rtt={}ms", server_used, drift_ms, rtt);
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
    info!("[timed] Osiris Time Daemon v{} starting", VERSION);

    let timed = Timed::new()?;
    let mut reader_rx = timed.connect_to_bridge().await?;
    timed.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = timed.handle_bridge_message(msg).await {
                        debug!("[timed] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[timed] Registration complete, entering time sync loop");

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = timed.check_time_sync().await {
                    error!("[timed] Time sync error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                // retained for symmetry; no buffered writes to flush here
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = timed.handle_bridge_message(msg).await {
                            debug!("[timed] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[timed] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
