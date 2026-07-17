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
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 60000; // 60 seconds
const FLUSH_INTERVAL_MS: u64 = 5000;
const NTP_SERVER: &str = "pool.ntp.org";
const TIME_DRIFT_WARNING_MS: i64 = 1000; // 1 second
const TIME_DRIFT_CRITICAL_MS: i64 = 5000; // 5 seconds

/// Timed state
struct Timed {
    socket: Arc<Mutex<Option<UnixStream>>>,
    daemon_name: String,
    pid: u32,
    last_ntp_sync: Arc<Mutex<Option<SystemTime>>>,
    last_alerts: Arc<Mutex<std::collections::HashMap<String, (AlertSeverity, u64)>>>,
}

impl Timed {
    fn new() -> Result<Self> {
        Ok(Self {
            socket: Arc::new(Mutex::new(None)),
            daemon_name: "timed".to_string(),
            pid: std::process::id(),
            last_ntp_sync: Arc::new(Mutex::new(None)),
            last_alerts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    async fn connect_to_bridge(&self) -> Result<()> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let mut socket_guard = self.socket.lock().await;
        *socket_guard = Some(stream);
        info!("[timed] Connected to AkerNet Bridge at {}", SOCKET_PATH);
        Ok(())
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
                        debug!("[timed] Socket read error: {}", e);
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
        info!("[timed] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Query NTP server and check system time drift
    async fn check_time_sync(&self) -> Result<()> {
        let system_now = SystemTime::now();

        // Query NTP
        let start = SystemTime::now();
        let ntp_result = ntp::request(NTP_SERVER);
        let rtt = start.elapsed()?.as_millis() as u64;

        let (ntp_time, rtt) = match ntp_result {
            Ok(response) => {
                let ntp_secs = response.transmit_time.sec as i64;
                let ntp_frac = response.transmit_time.frac as f64 / u32::MAX as f64;
                let ntp_time = SystemTime::UNIX_EPOCH + Duration::from_secs_f64(ntp_secs as f64 + ntp_frac);
                (ntp_time, rtt)
            }
            Err(e) => {
                warn!("[timed] NTP query failed: {}", e);
                self.emit_alert("ntp_failed", AlertSeverity::Warning, json!({
                    "metric": "ntp_query",
                    "server": NTP_SERVER,
                    "error": e.to_string(),
                    "message": "Failed to query NTP server",
                })).await?;
                return Ok(());
            }
        };

        // Calculate drift
        let drift = ntp_time.duration_since(system_now).unwrap_or(Duration::from_secs(0));
        let drift_ms = drift.as_millis() as i64;
        let drift_abs = drift_ms.abs();

        // Update last sync time
        *self.last_ntp_sync.lock().await = Some(system_now);

        // Check drift thresholds
        if drift_abs >= TIME_DRIFT_CRITICAL_MS {
            self.emit_alert("time_drift_critical", AlertSeverity::Critical, json!({
                "metric": "time_drift",
                "drift_ms": drift_ms,
                "threshold_ms": TIME_DRIFT_CRITICAL_MS,
                "system_time": DateTime::<Utc>::from(system_now).to_rfc3339(),
                "ntp_time": DateTime::<Utc>::from(ntp_time).to_rfc3339(),
                "rtt_ms": rtt,
                "message": format!("System time drift critical: {}ms", drift_ms),
            })).await?;
        } else if drift_abs >= TIME_DRIFT_WARNING_MS {
            self.emit_alert("time_drift_warning", AlertSeverity::Warning, json!({
                "metric": "time_drift",
                "drift_ms": drift_ms,
                "threshold_ms": TIME_DRIFT_WARNING_MS,
                "system_time": DateTime::<Utc>::from(system_now).to_rfc3339(),
                "ntp_time": DateTime::<Utc>::from(ntp_time).to_rfc3339(),
                "rtt_ms": rtt,
                "message": format!("System time drift detected: {}ms", drift_ms),
            })).await?;
        } else {
            debug!("[timed] Time sync OK: drift={}ms, rtt={}ms", drift_ms, rtt);
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
    info!("[timed] Osiris Time Daemon v{} starting", VERSION);

    let timed = Timed::new()?;
    timed.connect_to_bridge().await?;
    timed.register().await?;

    // Wait for acknowledgment with timeout
    tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        let mut interval = interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if let Err(e) = timed.handle_bridge_messages().await {
                debug!("[timed] Bridge message handler error: {}", e);
            }
        }
    }).await.ok(); // Ignore timeout, continue if registered

    info!("[timed] Registration complete, entering time sync loop");

    // Main loop - check NTP and handle bridge messages
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
                if let Err(e) = timed.handle_bridge_messages().await {
                    debug!("[timed] Bridge message handler error: {}", e);
                }
            }
        }
    }
}