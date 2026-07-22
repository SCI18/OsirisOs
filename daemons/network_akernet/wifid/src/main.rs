// wifid — Daemon #23, NetworkAkernet
// WiFi scanning, connection, profiles
// "The air is scanned. The signal is known."

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
const POLL_INTERVAL_MS: u64 = 30000;
const FLUSH_INTERVAL_MS: u64 = 5000;

enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

#[derive(Debug, Clone, Default)]
struct WifiNetwork {
    ssid: String,
    bssid: String,
    frequency: u32,
    signal_dbm: i32,
    flags: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct WifiInterface {
    name: String,
    connected: bool,
    current_ssid: Option<String>,
    current_bssid: Option<String>,
    signal_dbm: Option<i32>,
    bitrate_mbps: Option<u32>,
    known_networks: Vec<WifiNetwork>,
}

struct Wifid {
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<HashMap<String, (AlertSeverity, u64)>>>,
    known_interfaces: Arc<Mutex<HashMap<String, WifiInterface>>>,
}

impl Wifid {
    async fn new() -> Result<Self> {
        Ok(Self {
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "wifid".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(HashMap::new())),
            known_interfaces: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[wifid] Connected to AkerNet Bridge at {}", SOCKET_PATH);

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
                        debug!("[wifid] Socket read error: {}", e);
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
        debug!("[wifid] Registration sent");
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
                info!("[wifid] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::RegistrationRejected { name, reason } => {
                error!("[wifid] Registration rejected: {} — {}", name, reason);
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
                info!("[wifid] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[wifid] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[wifid] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Forward(_) => {
                debug!("[wifid] Received unexpected Forward message, ignoring");
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[wifid] Shutting down gracefully");
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
        info!("[wifid] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Read WiFi interface info from /proc/net/wireless
    async fn read_proc_net_wireless(&self) -> HashMap<String, WifiInterface> {
        let mut interfaces = HashMap::new();
        
        if let Ok(content) = tokio::fs::read_to_string("/proc/net/wireless").await {
            for line in content.lines().skip(2) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 10 {
                    continue;
                }
                
                let name = parts[0].trim_end_matches(':').to_string();
                if name == "lo" {
                    continue;
                }
                
                let status = parts[1].parse::<u32>().unwrap_or(0);
                let link_quality = parts[2].parse::<u32>().unwrap_or(0);
                let signal_level = parts[3].parse::<i32>().unwrap_or(0);
                let noise_level = parts[4].parse::<i32>().unwrap_or(0);
                
                // Calculate approximate signal in dBm (signal_level is typically in dBm * -1)
                let signal_dbm = if signal_level < 0 { signal_level } else { -signal_level };
                
                let mut iface = WifiInterface {
                    name: name.clone(),
                    connected: status & 0x1 != 0, // IEEE80211_CONNECTED
                    signal_dbm: Some(signal_dbm),
                    bitrate_mbps: None,
                    known_networks: Vec::new(),
                    current_ssid: None,
                    current_bssid: None,
                };
                
                // Try to get current connection info from /sys/class/net/<iface>/wireless
                if let Ok(conn_content) = tokio::fs::read_to_string(format!("/sys/class/net/{}/wireless", name)).await {
                    for line in conn_content.lines() {
                        if let Some(ssid) = line.strip_prefix("ssid=") {
                            iface.current_ssid = Some(ssid.to_string());
                        } else if let Some(bssid) = line.strip_prefix("bssid=") {
                            iface.current_bssid = Some(bssid.to_string());
                        }
                    }
                }
                
                interfaces.insert(name, iface);
            }
        }
        
        interfaces
    }

    /// Scan for available networks using iw command (if available)
    async fn scan_networks(&self, iface: &str) -> Vec<WifiNetwork> {
        let mut networks = Vec::new();
        
        if let Ok(output) = tokio::process::Command::new("iw")
            .args(["dev", iface, "scan"])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut current: Option<WifiNetwork> = None;
                
                for line in stdout.lines() {
                    let line = line.trim();
                    if line.starts_with("BSS ") {
                        if let Some(n) = current.take() {
                            networks.push(n);
                        }
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        current = Some(WifiNetwork {
                            bssid: parts.get(1).unwrap_or(&"").to_string(),
                            ..Default::default()
                        });
                    } else if let Some(ref mut n) = current {
                        if let Some(ssid) = line.strip_prefix("SSID: ") {
                            n.ssid = ssid.to_string();
                        } else if let Some(freq) = line.strip_prefix("freq: ") {
                            n.frequency = freq.parse().unwrap_or(0);
                        } else if let Some(signal) = line.strip_prefix("signal: ") {
                            n.signal_dbm = signal.split('.').next().unwrap_or("0").parse().unwrap_or(0);
                        } else if line.contains("capability:") {
                            n.flags.push(line.to_string());
                        }
                    }
                }
                if let Some(n) = current {
                    networks.push(n);
                }
            }
        }
        
        networks
    }

    async fn collect_and_check(&self) -> Result<()> {
        let current_wireless = self.read_proc_net_wireless().await;
        let mut known = self.known_interfaces.lock().await;

        // Collect keys to avoid borrowing issues
        let current_names: Vec<String> = current_wireless.keys().cloned().collect();
        
        for name in current_names {
            let mut current = current_wireless.get(&name).unwrap().clone();
            
            // Scan for available networks (expensive, do less frequently)
            let networks = self.scan_networks(&name).await;
            current.known_networks = networks;

            if !known.contains_key(&name) {
                // New WiFi interface
                self.emit_alert(&format!("wifi_interface_new_{}", name), AlertSeverity::Info, serde_json::json!({
                    "alert_type": "WiFiInterfaceAppeared",
                    "interface": name,
                    "connected": current.connected,
                    "signal_dbm": current.signal_dbm,
                    "message": format!("New WiFi interface detected: {}", name),
                })).await?;
            } else {
                let previous = &known[&name];
                
                // Check connection state change
                if previous.connected != current.connected {
                    let severity = if current.connected { AlertSeverity::Info } else { AlertSeverity::Warning };
                    self.emit_alert(&format!("wifi_connection_change_{}", name), severity, serde_json::json!({
                        "alert_type": "WiFiConnectionChange",
                        "interface": name,
                        "connected": current.connected,
                        "ssid": current.current_ssid,
                        "bssid": current.current_bssid,
                        "message": format!("WiFi interface {} {}", name, if current.connected { "connected" } else { "disconnected" }),
                    })).await?;
                }

                // Check signal strength degradation
                if let (Some(prev_sig), Some(curr_sig)) = (previous.signal_dbm, current.signal_dbm) {
                    if prev_sig - curr_sig > 10 { // Signal dropped by more than 10 dBm
                        self.emit_alert(&format!("wifi_signal_drop_{}", name), AlertSeverity::Warning, serde_json::json!({
                            "alert_type": "WiFiSignalDegraded",
                            "interface": name,
                            "previous_signal_dbm": prev_sig,
                            "current_signal_dbm": curr_sig,
                            "drop_dbm": prev_sig - curr_sig,
                            "message": format!("WiFi signal degraded on {}: {} dBm drop", name, prev_sig - curr_sig),
                        })).await?;
                    }
                }

                // Check for roaming (BSSID change while connected)
                if current.connected && previous.current_bssid != current.current_bssid {
                    if let (Some(prev), Some(curr)) = (&previous.current_bssid, &current.current_bssid) {
                        if prev != curr {
                            self.emit_alert(&format!("wifi_roam_{}", name), AlertSeverity::Info, serde_json::json!({
                                "alert_type": "WiFiRoam",
                                "interface": name,
                                "previous_bssid": prev,
                                "current_bssid": curr,
                                "ssid": current.current_ssid,
                                "message": format!("WiFi roamed on {}: {} -> {}", name, prev, curr),
                            })).await?;
                        }
                    }
                }

                // Check for authentication failures (disconnected with no clean roam)
                if previous.connected && !current.connected && previous.current_bssid == current.current_bssid {
                    self.emit_alert(&format!("wifi_auth_fail_{}", name), AlertSeverity::Warning, serde_json::json!({
                        "alert_type": "WiFiAuthFailure",
                        "interface": name,
                        "ssid": previous.current_ssid,
                        "bssid": previous.current_bssid,
                        "message": format!("WiFi authentication failure or link loss on {}", name),
                    })).await?;
                }
            }

            known.insert(name.clone(), current.clone());
        }

        // Check for disappeared WiFi interfaces
        let current_names: std::collections::HashSet<_> = current_wireless.keys().cloned().collect();
        let known_names: Vec<String> = known.keys().cloned().collect();
        for name in known_names {
            if !current_names.contains(&name) {
                self.emit_alert(&format!("wifi_interface_gone_{}", name), AlertSeverity::Warning, serde_json::json!({
                    "alert_type": "WiFiInterfaceDisappeared",
                    "interface": name,
                    "message": format!("WiFi interface {} disappeared", name),
                })).await?;
            }
        }

        *known = current_wireless.clone();

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
    info!("[wifid] Osiris WiFi Daemon v{} starting", VERSION);

    let wifid = Wifid::new().await?;
    let mut reader_rx = wifid.connect_to_bridge().await?;
    wifid.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = wifid.handle_bridge_message(msg).await {
                        debug!("[wifid] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[wifid] Registration complete, entering monitoring loop");

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = wifid.collect_and_check().await {
                    error!("[wifid] Collection error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                // retained for symmetry
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = wifid.handle_bridge_message(msg).await {
                            debug!("[wifid] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[wifid] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}