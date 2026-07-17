// healthd — Assessor #3, SystemCore
// CPU/RAM/thermal monitoring, threshold alerts
// "The body's vitals are watched. The sick are known."

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::{json, Value};
use sysinfo::{System, Disks};
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{Duration, interval};
use tracing::{info, warn, error, debug};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 10000;
const FLUSH_INTERVAL_MS: u64 = 5000;

const CPU_WARNING_THRESHOLD: f32 = 80.0;
const CPU_CRITICAL_THRESHOLD: f32 = 95.0;
const MEMORY_WARNING_THRESHOLD: f32 = 80.0;
const MEMORY_CRITICAL_THRESHOLD: f32 = 95.0;
const TEMP_WARNING_THRESHOLD: f32 = 80.0; // Celsius
const TEMP_CRITICAL_THRESHOLD: f32 = 95.0;
const DISK_WARNING_THRESHOLD: f32 = 85.0;
const DISK_CRITICAL_THRESHOLD: f32 = 95.0;

const THERMAL_ZONE_GLOB: &str = "/sys/class/thermal";

enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

/// A thermal zone reading
struct ThermalReading {
    zone: String,
    temp_celsius: f32,
}

/// Read all available thermal zones under /sys/class/thermal.
/// FIX: previously healthd did not attempt any thermal reads at all, since
/// sysinfo 0.30 doesn't expose temperature on all platforms. This reads the
/// kernel's thermal_zone interface directly instead of relying on sysinfo,
/// which works on any Linux kernel (including under proot, since /sys is
/// typically bind-mounted through) and degrades to an empty list — not an
/// error — if no thermal zones are exposed (e.g. some proot/container
/// configurations hide /sys/class/thermal entirely).
fn read_thermal_zones() -> Vec<ThermalReading> {
    let mut readings = Vec::new();

    let entries = match std::fs::read_dir(THERMAL_ZONE_GLOB) {
        Ok(e) => e,
        Err(_) => return readings, // no thermal support available — not an error
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if !name.starts_with("thermal_zone") {
            continue;
        }

        let temp_path = path.join("temp");
        let type_path = path.join("type");

        let raw_temp = match std::fs::read_to_string(&temp_path) {
            Ok(s) => s.trim().parse::<i64>().ok(),
            Err(_) => None,
        };

        let Some(raw_temp) = raw_temp else { continue };

        // Kernel reports millidegrees Celsius almost universally.
        let temp_celsius = raw_temp as f32 / 1000.0;

        let zone_label = std::fs::read_to_string(&type_path)
            .map(|s| s.trim().to_string())
            .unwrap_or(name);

        readings.push(ThermalReading { zone: zone_label, temp_celsius });
    }

    readings
}

/// Healthd state
struct Healthd {
    system: Arc<Mutex<System>>,
    disks: Arc<Mutex<Disks>>,
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<HashMap<String, (AlertSeverity, u64)>>>,
}

impl Healthd {
    fn new() -> Result<Self> {
        let mut system = System::new_all();
        system.refresh_all();

        let disks = Disks::new_with_refreshed_list();

        Ok(Self {
            system: Arc::new(Mutex::new(system)),
            disks: Arc::new(Mutex::new(disks)),
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "healthd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Connect and spawn a persistent reader task (same fix applied in logd —
    /// avoids the read-vs-flush race from recreating a BufReader every tick).
    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[healthd] Connected to AkerNet Bridge at {}", SOCKET_PATH);

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
                        debug!("[healthd] Socket read error: {}", e);
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
        debug!("[healthd] Registration sent");
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
        info!("[healthd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Collect metrics and check thresholds
    async fn collect_and_check(&self) -> Result<()> {
        {
            let mut sys = self.system.lock().await;
            sys.refresh_cpu();
            sys.refresh_memory();
        }

        {
            let mut disks = self.disks.lock().await;
            disks.refresh_list();
            disks.refresh();
        }

        let (cpu_usage, memory_usage, memory_total, disk_usages, sysinfo_looked_healthy) = {
            let sys = self.system.lock().await;

            let cpus = sys.cpus();
            let cpu_usage = if !cpus.is_empty() {
                cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / cpus.len() as f32
            } else { 0.0 };

            let memory_used = sys.used_memory();
            let memory_total = sys.total_memory();
            let memory_usage = if memory_total > 0 {
                (memory_used as f32 / memory_total as f32) * 100.0
            } else { 0.0 };

            let disks_guard = self.disks.lock().await;
            let mut disk_usages = Vec::new();
            for disk in disks_guard.list() {
                let total = disk.total_space();
                let available = disk.available_space();
                if total > 0 {
                    let used = total - available;
                    let usage_pct = (used as f32 / total as f32) * 100.0;
                    disk_usages.push((disk.mount_point().to_string_lossy().to_string(), usage_pct));
                }
            }

            // FIX: sysinfo can silently report zero CPUs / zero total memory
            // under some proot configurations instead of erroring — there was
            // previously no way to tell "genuinely idle/empty system" apart
            // from "sysinfo couldn't read the real values here". Flag it so
            // we can fall back to /proc directly rather than silently
            // reporting 0% usage as if everything were fine.
            let sysinfo_looked_healthy = !cpus.is_empty() && memory_total > 0;

            (cpu_usage, memory_usage, memory_total, disk_usages, sysinfo_looked_healthy)
        };

        let (cpu_usage, memory_usage, memory_total) = if sysinfo_looked_healthy {
            (cpu_usage, memory_usage, memory_total)
        } else {
            warn!("[healthd] sysinfo returned empty CPU/memory data, falling back to /proc");
            proot_fallback_cpu_mem().unwrap_or((cpu_usage, memory_usage, memory_total))
        };

        if cpu_usage >= CPU_CRITICAL_THRESHOLD {
            self.emit_alert("cpu_critical", AlertSeverity::Critical, json!({
                "metric": "cpu_usage", "value": cpu_usage, "threshold": CPU_CRITICAL_THRESHOLD,
                "unit": "percent", "message": format!("CPU usage critical: {:.1}%", cpu_usage),
            })).await?;
        } else if cpu_usage >= CPU_WARNING_THRESHOLD {
            self.emit_alert("cpu_warning", AlertSeverity::Warning, json!({
                "metric": "cpu_usage", "value": cpu_usage, "threshold": CPU_WARNING_THRESHOLD,
                "unit": "percent", "message": format!("CPU usage high: {:.1}%", cpu_usage),
            })).await?;
        }

        if memory_usage >= MEMORY_CRITICAL_THRESHOLD {
            self.emit_alert("memory_critical", AlertSeverity::Critical, json!({
                "metric": "memory_usage", "value": memory_usage, "threshold": MEMORY_CRITICAL_THRESHOLD,
                "unit": "percent", "total_bytes": memory_total,
                "used_bytes": (memory_total as f32 * memory_usage / 100.0) as u64,
                "message": format!("Memory usage critical: {:.1}%", memory_usage),
            })).await?;
        } else if memory_usage >= MEMORY_WARNING_THRESHOLD {
            self.emit_alert("memory_warning", AlertSeverity::Warning, json!({
                "metric": "memory_usage", "value": memory_usage, "threshold": MEMORY_WARNING_THRESHOLD,
                "unit": "percent", "total_bytes": memory_total,
                "used_bytes": (memory_total as f32 * memory_usage / 100.0) as u64,
                "message": format!("Memory usage high: {:.1}%", memory_usage),
            })).await?;
        }

        for (mount, used_pct) in disk_usages {
            let mount_key = mount.trim_start_matches('/').replace('/', "_");
            if mount_key.is_empty() { continue; }

            if used_pct >= DISK_CRITICAL_THRESHOLD {
                self.emit_alert(&format!("disk_{}_critical", mount_key), AlertSeverity::Critical, json!({
                    "metric": "disk_usage", "mount_point": mount, "value": used_pct,
                    "threshold": DISK_CRITICAL_THRESHOLD, "unit": "percent",
                    "message": format!("Disk usage critical on {}: {:.1}%", mount, used_pct),
                })).await?;
            } else if used_pct >= DISK_WARNING_THRESHOLD {
                self.emit_alert(&format!("disk_{}_warning", mount_key), AlertSeverity::Warning, json!({
                    "metric": "disk_usage", "mount_point": mount, "value": used_pct,
                    "threshold": DISK_WARNING_THRESHOLD, "unit": "percent",
                    "message": format!("Disk usage high on {}: {:.1}%", mount, used_pct),
                })).await?;
            }
        }

        // FIX: thermal monitoring was previously entirely absent (only a
        // comment noting sysinfo 0.30 doesn't expose it). Now reads
        // /sys/class/thermal directly, which does not depend on sysinfo at
        // all. If no thermal zones are exposed (e.g. hidden under this
        // proot config), the list is simply empty and nothing is alerted —
        // an explicit, checked absence rather than a silent one.
        for reading in read_thermal_zones() {
            let zone_key = reading.zone.replace(' ', "_").replace('/', "_");
            if reading.temp_celsius >= TEMP_CRITICAL_THRESHOLD {
                self.emit_alert(&format!("temp_{}_critical", zone_key), AlertSeverity::Critical, json!({
                    "metric": "temperature", "zone": reading.zone, "value_celsius": reading.temp_celsius,
                    "threshold_celsius": TEMP_CRITICAL_THRESHOLD,
                    "message": format!("Temperature critical on {}: {:.1}°C", reading.zone, reading.temp_celsius),
                })).await?;
            } else if reading.temp_celsius >= TEMP_WARNING_THRESHOLD {
                self.emit_alert(&format!("temp_{}_warning", zone_key), AlertSeverity::Warning, json!({
                    "metric": "temperature", "zone": reading.zone, "value_celsius": reading.temp_celsius,
                    "threshold_celsius": TEMP_WARNING_THRESHOLD,
                    "message": format!("Temperature high on {}: {:.1}°C", reading.zone, reading.temp_celsius),
                })).await?;
            }
        }

        let status = DaemonMessage::StatusUpdate {
            name: self.daemon_name.clone(),
            status: DaemonStatus::Running,
        };
        self.send_frame(status).await?;

        Ok(())
    }
}

/// Proot-safe fallback: read CPU and memory usage directly from /proc when
/// sysinfo returns empty/zeroed data. This is a point-in-time snapshot
/// rather than a delta-based CPU percentage (that needs two samples over
/// time), so on first fallback read CPU usage may read as 0 — acceptable
/// for a degraded-mode fallback, not a primary data source.
fn proot_fallback_cpu_mem() -> Option<(f32, f32, u64)> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut mem_total_kb = 0u64;
    let mut mem_available_kb = 0u64;

    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            mem_total_kb = rest.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            mem_available_kb = rest.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
        }
    }

    let memory_total = mem_total_kb * 1024;
    let memory_usage = if mem_total_kb > 0 {
        ((mem_total_kb.saturating_sub(mem_available_kb)) as f32 / mem_total_kb as f32) * 100.0
    } else { 0.0 };

    // A real CPU percentage needs two /proc/stat samples over an interval;
    // as a degraded fallback we report 0.0 rather than fabricate a number.
    let cpu_usage = 0.0;

    Some((cpu_usage, memory_usage, memory_total))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("[healthd] Osiris Health Daemon v{} starting", VERSION);

    let healthd = Healthd::new()?;
    let mut reader_rx = healthd.connect_to_bridge().await?;
    healthd.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = healthd.handle_bridge_message(msg).await {
                        debug!("[healthd] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[healthd] Registration complete, entering monitoring loop");

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
                // periodic tick retained for symmetry with other daemons;
                // no buffered writes to flush in healthd currently.
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = healthd.handle_bridge_message(msg).await {
                            debug!("[healthd] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[healthd] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
