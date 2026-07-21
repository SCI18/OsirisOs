// proxyd — Daemon #27, NetworkAkernet
// Traffic routing, privacy proxy
// "The path is hidden. The traffic is weighed."

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::Value;
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tokio::time::interval;
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 10000;
const FLUSH_INTERVAL_MS: u64 = 5000;

enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

struct Proxyd {
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<std::collections::HashMap<String, (AlertSeverity, u64)>>>,
}

impl Proxyd {
    async fn new() -> Result<Self> {
        Ok(Self {
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "proxyd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
    }

    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[proxyd] Connected to AkerNet Bridge at {}", SOCKET_PATH);

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
                            if tx.send(ReaderEvent::Bridge(bridge_msg)).await.is_err() { break; }
                        }
                    }
                    Err(e) => {
                        debug!("[proxyd] Socket read error: {}", e);
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
        debug!("[proxyd] Registration sent");
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
                info!("[proxyd] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::RegistrationRejected { name, reason } => {
                error!("[proxyd] Registration rejected: {} — {}", name, reason);
                std::process::exit(1);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[proxyd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[proxyd] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[proxyd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Forward(_) => {
                debug!("[proxyd] Received unexpected Forward message, ignoring");
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[proxyd] Shutting down gracefully");
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
                    if severity_order <= last_order { return Ok(()); }
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
        info!("[proxyd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    async fn collect_and_check(&self) -> Result<()> {
        // TODO: Implement proxy monitoring
        // - Track proxy process health (sing-box, v2ray, etc.)
        // - Monitor inbound/outbound connection counts and throughput
        // - Alert on proxy failures, connection leaks, circuit breaks
        // - Manage proxy profiles (Tor, Shadowsocks, WireGuard obfs, etc.)
        // - Coordinate with vpnd for split-tunneling, kill switch integration
        // - Rotate proxy chains, handle upstream failures gracefully

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
    info!("[proxyd] Osiris Proxy Daemon v{} starting", VERSION);

    let proxyd = Proxyd::new().await?;
    let mut reader_rx = proxyd.connect_to_bridge().await?;
    proxyd.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = proxyd.handle_bridge_message(msg).await {
                        debug!("[proxyd] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[proxyd] Registration complete, entering monitoring loop");

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = proxyd.collect_and_check().await {
                    error!("[proxyd] Collection error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                // retained for symmetry
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = proxyd.handle_bridge_message(msg).await {
                            debug!("[proxyd] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[proxyd] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}