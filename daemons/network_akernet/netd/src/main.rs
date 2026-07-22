// netd — Daemon #22, NetworkAkernet
// Core network management, interfaces
// "The path is mapped. The traffic flows."

use std::collections::HashMap;
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

#[derive(Debug, Clone, Default)]
struct InterfaceStats {
    name: String,
    rx_bytes: u64,
    tx_bytes: u64,
    rx_packets: u64,
    tx_packets: u64,
    rx_errors: u64,
    tx_errors: u64,
    rx_dropped: u64,
    tx_dropped: u64,
    carrier_up: bool,
    ipv4_addrs: Vec<String>,
    ipv6_addrs: Vec<String>,
}

struct Netd {
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<HashMap<String, (AlertSeverity, u64)>>>,
    known_interfaces: Arc<Mutex<HashMap<String, InterfaceStats>>>,
}

impl Netd {
    async fn new() -> Result<Self> {
        Ok(Self {
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "netd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(HashMap::new())),
            known_interfaces: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[netd] Connected to AkerNet Bridge at {}", SOCKET_PATH);

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
                        debug!("[netd] Socket read error: {}", e);
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
        debug!("[netd] Registration sent");
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
                info!("[netd] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::RegistrationRejected { name, reason } => {
                error!("[netd] Registration rejected: {} — {}", name, reason);
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
                info!("[netd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[netd] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[netd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Forward(_) => {
                debug!("[netd] Received unexpected Forward message, ignoring");
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[netd] Shutting down gracefully");
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
        info!("[netd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Read interface statistics from /proc/net/dev
    async fn read_proc_net_dev(&self) -> Result<HashMap<String, InterfaceStats>> {
        let content = tokio::fs::read_to_string("/proc/net/dev").await?;
        let mut interfaces = HashMap::new();

        for line in content.lines().skip(2) { // Skip header lines
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 17 {
                continue;
            }

            let name = parts[0].trim_end_matches(':').to_string();
            // Skip loopback and virtual interfaces we don't manage
            if name == "lo" || name.starts_with("docker") || name.starts_with("veth") || name.starts_with("br-") {
                continue;
            }

            let stats = InterfaceStats {
                name: name.clone(),
                rx_bytes: parts[1].parse().unwrap_or(0),
                rx_packets: parts[2].parse().unwrap_or(0),
                rx_errors: parts[3].parse().unwrap_or(0),
                rx_dropped: parts[4].parse().unwrap_or(0),
                tx_bytes: parts[9].parse().unwrap_or(0),
                tx_packets: parts[10].parse().unwrap_or(0),
                tx_errors: parts[11].parse().unwrap_or(0),
                tx_dropped: parts[12].parse().unwrap_or(0),
                carrier_up: false, // Will be filled from /sys/class/net
                ipv4_addrs: Vec::new(),
                ipv6_addrs: Vec::new(),
            };
            interfaces.insert(name, stats);
        }

        Ok(interfaces)
    }

    /// Read carrier state from /sys/class/net/<iface>/carrier
    async fn read_carrier_state(&self, iface: &str) -> bool {
        let path = format!("/sys/class/net/{}/carrier", iface);
        tokio::fs::read_to_string(&path).await
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }

    /// Read IPv4 addresses from /proc/net/fib_trie (simplified)
    async fn read_ipv4_addrs(&self, iface: &str) -> Vec<String> {
        let mut addrs = Vec::new();
        if let Ok(content) = tokio::fs::read_to_string("/proc/net/fib_trie").await {
            for line in content.lines() {
                if line.contains(iface) && line.contains("local") {
                    if let Some(addr) = line.split_whitespace().nth(1) {
                        if !addr.starts_with("127.") {
                            addrs.push(addr.to_string());
                        }
                    }
                }
            }
        }
        addrs
    }

    /// Read IPv6 addresses from /proc/net/if_inet6
    async fn read_ipv6_addrs(&self, iface: &str) -> Vec<String> {
        let mut addrs = Vec::new();
        if let Ok(content) = tokio::fs::read_to_string("/proc/net/if_inet6").await {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 6 && parts[5] == iface {
                    let addr = parts[0];
                    if addr.len() == 32 {
                        // Format IPv6 address
                        let formatted = format!(
                            "{}:{}:{}:{}:{}:{}:{}:{}",
                            &addr[0..4], &addr[4..8], &addr[8..12], &addr[12..16],
                            &addr[16..20], &addr[20..24], &addr[24..28], &addr[28..32]
                        );
                        addrs.push(formatted);
                    }
                }
            }
        }
        addrs
    }

    async fn collect_and_check(&self) -> Result<()> {
        // Read current interface stats
        let current_stats = self.read_proc_net_dev().await?;
        let mut known = self.known_interfaces.lock().await;

        // Check each current interface
        for (name, current) in &current_stats {
            let name_owned = name.clone();
            let mut current = current.clone();
            
            // Read IP addresses
            current.ipv4_addrs = self.read_ipv4_addrs(name).await;
            current.ipv6_addrs = self.read_ipv6_addrs(name).await;

            // Check if this is a new interface
            if !known.contains_key(&name_owned) {
                self.emit_alert(&format!("interface_new_{}", name_owned), AlertSeverity::Info, serde_json::json!({
                    "alert_type": "InterfaceAppeared",
                    "interface": name_owned,
                    "carrier_up": current.carrier_up,
                    "ipv4_addrs": current.ipv4_addrs,
                    "ipv6_addrs": current.ipv6_addrs,
                    "message": format!("New network interface detected: {}", name_owned),
                })).await?;
            } else {
                let previous = known.get(&name_owned).unwrap();
                
                // Check carrier change
                if previous.carrier_up != current.carrier_up {
                    let severity = if current.carrier_up { AlertSeverity::Info } else { AlertSeverity::Warning };
                    self.emit_alert(&format!("carrier_change_{}", name_owned), severity, serde_json::json!({
                        "alert_type": "CarrierChange",
                        "interface": name_owned,
                        "carrier_up": current.carrier_up,
                        "message": format!("Interface {} carrier {}", name_owned, if current.carrier_up { "up" } else { "down" }),
                    })).await?;
                }

                // Check IP address changes
                if previous.ipv4_addrs != current.ipv4_addrs || previous.ipv6_addrs != current.ipv6_addrs {
                    self.emit_alert(&format!("ip_change_{}", name_owned), AlertSeverity::Info, serde_json::json!({
                        "alert_type": "IPAddressChange",
                        "interface": name_owned,
                        "ipv4_addrs": current.ipv4_addrs,
                        "ipv6_addrs": current.ipv6_addrs,
                        "previous_ipv4": previous.ipv4_addrs,
                        "previous_ipv6": previous.ipv6_addrs,
                        "message": format!("IP addresses changed on interface {}", name_owned),
                    })).await?;
                }

                // Check for high error rates
                let rx_error_rate = if current.rx_packets > previous.rx_packets {
                    (current.rx_errors - previous.rx_errors) as f64 / (current.rx_packets - previous.rx_packets) as f64
                } else { 0.0 };
                
                let tx_error_rate = if current.tx_packets > previous.tx_packets {
                    (current.tx_errors - previous.tx_errors) as f64 / (current.tx_packets - previous.tx_packets) as f64
                } else { 0.0 };

                if rx_error_rate > 0.01 || tx_error_rate > 0.01 {
                    self.emit_alert(&format!("high_errors_{}", name_owned), AlertSeverity::Warning, serde_json::json!({
                        "alert_type": "HighErrorRate",
                        "interface": name_owned,
                        "rx_error_rate": rx_error_rate,
                        "tx_error_rate": tx_error_rate,
                        "message": format!("High error rate on interface {}: rx={:.2}% tx={:.2}%", name_owned, rx_error_rate*100.0, tx_error_rate*100.0),
                    })).await?;
                }
            }

            known.insert(name_owned.clone(), current.clone());
        }

        // Check for disappeared interfaces
        let current_names: std::collections::HashSet<_> = current_stats.keys().cloned().collect();
        for name in known.keys() {
            if !current_names.contains(name) {
                self.emit_alert(&format!("interface_gone_{}", name), AlertSeverity::Warning, serde_json::json!({
                    "alert_type": "InterfaceDisappeared",
                    "interface": name,
                    "message": format!("Network interface {} disappeared", name),
                })).await?;
            }
        }

        // Update known interfaces
        *known = current_stats.clone();

        // Send status update
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
    info!("[netd] Osiris Network Daemon v{} starting", VERSION);

    let netd = Netd::new().await?;
    let mut reader_rx = netd.connect_to_bridge().await?;
    netd.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = netd.handle_bridge_message(msg).await {
                        debug!("[netd] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[netd] Registration complete, entering monitoring loop");

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = netd.collect_and_check().await {
                    error!("[netd] Collection error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                // retained for symmetry; no buffered writes to flush currently
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = netd.handle_bridge_message(msg).await {
                            debug!("[netd] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[netd] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}