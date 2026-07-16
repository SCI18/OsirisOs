// healthd — Assessor #3, SystemCore
// CPU/RAM/thermal monitoring, threshold alerts
// "The body's vitals are watched. The sick are known."

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::{json, Value};
use sysinfo::System;
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{info, error, debug};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 10000; // 10 seconds
const FLUSH_INTERVAL_MS: u64 = 5000;

// Thresholds (configurable in future)
const CPU_WARNING_THRESHOLD: f32 = 80.0;
const CPU_CRITICAL_THRESHOLD: f32 = 95.0;
const MEMORY_WARNING_THRESHOLD: f32 = 80.0;
const MEMORY_CRITICAL_THRESHOLD: f32 = 95.0;
const TEMP_WARNING_THRESHOLD: f32 = 80.0; // Celsius
const TEMP_CRITICAL_THRESHOLD: f32 = 95.0;
const DISK_WARNING_THRESHOLD: f32 = 85.0;
const DISK_CRITICAL_THRESHOLD: f32 = 95.0;

/// Healthd state
struct Healthd {
    system: Arc<Mutex<System>>,
    socket: Arc<Mutex<Option<UnixStream>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<HashMap<String, (AlertSeverity, u64)>>>, // key -> (severity, timestamp)
}

impl Healthd {
    fn new() -> Result<Self> {
        let mut system = System::new_all();
        system.refresh_all();

        Ok(Self {
            system: Arc::new(Mutex::new(system)),
            socket: Arc::new(Mutex::new(None)),
            daemon_name: "healthd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn connect_to_bridge(&self) -> Result<()> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let mut socket_guard = self.socket.lock().await;
        *socket_guard = Some(stream);
        info!("[healthd] Connected to AkerNet Bridge at {}", SOCKET_PATH);
        Ok(())
    }

    async fn register(&self) -> Result<()> {
        let msg = DaemonMessage::Register {
            name: self.daemon_name.clone(),
            pid: self.pid,
            version: VERSION.to_string(),
        };
        self.send_frame(msg).await?;
        debug!("[healthd] Registration sent");
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
        // Take the stream out of the mutex to read without holding lock
        let mut stream_opt = self.socket.lock().await.take();
        
        if let Some(mut stream) = stream_opt.take() {
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            loop {
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        if let Ok(bridge_msg) = Frame::decode_bridge_message(&line) {
                            self.handle_bridge_message(bridge_msg).await?;
                        }
                        line.clear();
                    }
                    Err(e) => {
                        debug!("[healthd] Socket read error: {}", e);
                        break;
                    }
                }
            }
            // Put stream back
            *self.socket.lock().await = Some(stream);
        }
        Ok(())
    }

    async fn handle_bridge_message(&self, msg: BridgeMessage) -> Result<()> {
        match msg {
            BridgeMessage::Acknowledged { name } => {
                info!("[healthd] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[healthd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[healthd] Received Reload from Bridge");
                let mut sys = self.system.lock().await;
                sys.refresh_all();
            }
            BridgeMessage::Restart => {
                info!("[healthd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[healthd] Shutting down gracefully");
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
                    // Check if severity increased
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
        info!("[healthd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Collect metrics and check thresholds
    async fn collect_and_check(&self) -> Result<()> {
        // Refresh system info
        {
            let mut sys = self.system.lock().await;
            sys.refresh_cpu_all();
            sys.refresh_memory();
            sys.refresh_disks_list();
            sys.refresh_disks();
        }

        // Collect metrics under lock
        let (cpu_usage, memory_usage, memory_total, disk_usages) = {
            let sys = self.system.lock().await;
            
            // CPU - global usage
            let cpu_usage = sys.global_cpu_usage();
            
            // Memory
            let memory_used = sys.used_memory();
            let memory_total = sys.total_memory();
            let memory_usage = if memory_total > 0 {
                (memory_used as f32 / memory_total as f32) * 100.0
            } else { 0.0 };
            
            // Disks
            let mut disk_usages = Vec::new();
            for disk in sys.disks() {
                let total = disk.total_space();
                let available = disk.available_space();
                if total > 0 {
                    let used = total - available;
                    let usage_pct = (used as f32 / total as f32) * 100.0;
                    disk_usages.push((disk.mount_point().to_string_lossy().to_string(), usage_pct));
                }
            }
            
            (cpu_usage, memory_usage, memory_total, disk_usages)
        };

        // Check CPU thresholds
        if cpu_usage >= CPU_CRITICAL_THRESHOLD {
            self.emit_alert("cpu_critical", AlertSeverity::Critical, json!({
                "metric": "cpu_usage",
                "value": cpu_usage,
                "threshold": CPU_CRITICAL_THRESHOLD,
                "unit": "percent",
                "message": format!("CPU usage critical: {:.1}%", cpu_usage),
            })).await?;
        } else if cpu_usage >= CPU_WARNING_THRESHOLD {
            self.emit_alert("cpu_warning", AlertSeverity::Warning, json!({
                "metric": "cpu_usage",
                "value": cpu_usage,
                "threshold": CPU_WARNING_THRESHOLD,
                "unit": "percent",
                "message": format!("CPU usage high: {:.1}%", cpu_usage),
            })).await?;
        }

        // Check memory thresholds
        if memory_usage >= MEMORY_CRITICAL_THRESHOLD {
            self.emit_alert("memory_critical", AlertSeverity::Critical, json!({
                "metric": "memory_usage",
                "value": memory_usage,
                "threshold": MEMORY_CRITICAL_THRESHOLD,
                "unit": "percent",
                "total_bytes": memory_total,
                "used_bytes": (memory_total as f32 * memory_usage / 100.0) as u64,
                "message": format!("Memory usage critical: {:.1}%", memory_usage),
            })).await?;
        } else if memory_usage >= MEMORY_WARNING_THRESHOLD {
            self.emit_alert("memory_warning", AlertSeverity::Warning, json!({
                "metric": "memory_usage",
                "value": memory_usage,
                "threshold": MEMORY_WARNING_THRESHOLD,
                "unit": "percent",
                "total_bytes": memory_total,
                "used_bytes": (memory_total as f32 * memory_usage / 100.0) as u64,
                "message": format!("Memory usage high: {:.1}%", memory_usage),
            })).await?;
        }

        // Check disk thresholds
        for (mount, used_pct) in disk_usages {
            let mount_key = mount.trim_start_matches('/').replace('/', "_");
            if mount_key.is_empty() { continue; }
            
            if used_pct >= DISK_CRITICAL_THRESHOLD {
                self.emit_alert(&format!("disk_{}_critical", mount_key), AlertSeverity::Critical, json!({
                    "metric": "disk_usage",
                    "mount_point": mount,
                    "value": used_pct,
                    "threshold": DISK_CRITICAL_THRESHOLD,
                    "unit": "percent",
                    "message": format!("Disk usage critical on {}: {:.1}%", mount, used_pct),
                })).await?;
            } else if used_pct >= DISK_WARNING_THRESHOLD {
                self.emit_alert(&format!("disk_{}_warning", mount_key), AlertSeverity::Warning, json!({
                    "metric": "disk_usage",
                    "mount_point": mount,
                    "value": used_pct,
                    "threshold": DISK_WARNING_THRESHOLD,
                    "unit": "percent",
                    "message": format!("Disk usage high on {}: {:.1}%", mount, used_pct),
                })).await?;
            }
        }

        // Note: Temperature sensors not available in sysinfo 0.30 on all platforms
        // Could be added later via platform-specific code

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
    info!("[healthd] Osiris Health Daemon v{} starting", VERSION);

    let healthd = Healthd::new()?;
    healthd.connect_to_bridge().await?;
    healthd.register().await?;

    // Wait for acknowledgment with timeout
    tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        let mut interval = interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            if let Err(e) = healthd.handle_bridge_messages().await {
                debug!("[healthd] Bridge message handler error: {}", e);
            }
        }
    }).await.ok(); // Ignore timeout, continue if registered

    info!("[healthd] Registration complete, entering monitoring loop");

    // Main monitoring loop
    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut flush_interval = interval(Duration::from_millis(FLUSH_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = healthd.collect_and_check().await {
                    error!("[healthd] Collection error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                if let Err(e) = healthd.handle_bridge_messages().await {
                    debug!("[healthd] Bridge message handler error: {}", e);
                }
            }
        }
    }
}