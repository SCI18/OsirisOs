 mountd — Assessor #6, SystemCore
// Filesystem mount management post-boot
// "Mounts are weighed. The unmounted are known."

use std::collections::HashMap;
use std::fs;
use std::ffi::CString;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const DEFAULT_FSTAB_PATH: &str = "/etc/fstab";
const DEFAULT_MOUNTINFO_PATH: &str = "/proc/self/mountinfo";
const DEFAULT_MOUNT_CMD: &str = "mount";

/// Configuration for mountd
#[derive(Debug, Clone, Deserialize, Serialize)]
struct MountdConfig {
    fstab_path: PathBuf,
    mountinfo_path: PathBuf,
    mount_cmd: PathBuf,
    poll_interval_ms: u64,
    flush_interval_ms: u64,
    registration_timeout_ms: u64,
    disk_usage_warning_pct: f32,
    disk_usage_critical_pct: f32,
    zfs_scrub_alert: bool,
    btrfs_scrub_alert: bool,
    #[serde(default = "default_true")]
    proot_fallback: bool,
}

fn default_true() -> bool { true }

impl Default for MountdConfig {
    fn default() -> Self {
        Self {
            fstab_path: PathBuf::from(DEFAULT_FSTAB_PATH),
            mountinfo_path: PathBuf::from(DEFAULT_MOUNTINFO_PATH),
            mount_cmd: PathBuf::from(DEFAULT_MOUNT_CMD),
            poll_interval_ms: 30000,
            flush_interval_ms: 5000,
            registration_timeout_ms: 5000,
            disk_usage_warning_pct: 85.0,
            disk_usage_critical_pct: 95.0,
            zfs_scrub_alert: true,
            btrfs_scrub_alert: true,
            proot_fallback: true,
        }
    }
}

/// Mount entry from /proc/self/mountinfo
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct MountEntry {
    mount_id: u32,
    parent_id: u32,
    major: u32,
    minor: u32,
    root: String,
    mount_point: String,
    fs_type: String,
    mount_options: Vec<String>,
    optional_fields: Vec<String>,
    propagation_flags: Vec<String>,
}

/// Fstab entry with x-osiris-alert options
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FstabEntry {
    device: String,
    mount_point: String,
    fs_type: String,
    options: Vec<String>,
    dump: u8,
    pass: u8,
    alert_severity: Option<AlertSeverity>,
    alert_on_missing: bool,
    alert_on_fs_mismatch: bool,
    alert_on_options_change: bool,
    /// Explicitly expected to be read-only (e.g. a boot partition). Suppresses
    /// the FilesystemReadOnly alert for this mount point since RO is intentional.
    expected_readonly: bool,
    disk_warning_pct: Option<f32>,
    disk_critical_pct: Option<f32>,
}

/// Disk usage info
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskUsage {
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    usage_pct: f32,
}

/// ZFS pool status
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ZfsPoolStatus {
    name: String,
    state: String,
    scan: String,
    scrub_in_progress: bool,
    scrub_errors: u64,
}

/// Btrfs filesystem status
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BtrfsStatus {
    mount_point: String,
    scrub_running: bool,
    scrub_progress_pct: Option<f32>,
    scrub_errors: u64,
}

/// Mountd alert types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
enum MountAlertType {
    MountMissing,
    MountDegraded,
    MountUnexpected,
    MountPropagationChanged,
    FilesystemFull,
    FilesystemReadOnly,
    ZfsPoolDegraded,
    BtrfsScrubRunning,
    ZfsScrubRunning,
    BtrfsScrubErrors,
    ZfsScrubErrors,
    DiskUsageWarning,
    DiskUsageCritical,
    FsTypeMismatch,
    OptionsMismatch,
}

impl MountAlertType {
    fn as_str(&self) -> &str {
        match self {
            MountAlertType::MountMissing => "MountMissing",
            MountAlertType::MountDegraded => "MountDegraded",
            MountAlertType::MountUnexpected => "MountUnexpected",
            MountAlertType::MountPropagationChanged => "MountPropagationChanged",
            MountAlertType::FilesystemFull => "FilesystemFull",
            MountAlertType::FilesystemReadOnly => "FilesystemReadOnly",
            MountAlertType::ZfsPoolDegraded => "ZfsPoolDegraded",
            MountAlertType::BtrfsScrubRunning => "BtrfsScrubRunning",
            MountAlertType::ZfsScrubRunning => "ZfsScrubRunning",
            MountAlertType::BtrfsScrubErrors => "BtrfsScrubErrors",
            MountAlertType::ZfsScrubErrors => "ZfsScrubErrors",
            MountAlertType::DiskUsageWarning => "DiskUsageWarning",
            MountAlertType::DiskUsageCritical => "DiskUsageCritical",
            MountAlertType::FsTypeMismatch => "FsTypeMismatch",
            MountAlertType::OptionsMismatch => "OptionsMismatch",
        }
    }
}

/// Mountd state
struct Mountd {
    socket: Arc<Mutex<Option<UnixStream>>>,
    daemon_name: String,
    pid: u32,
    config: MountdConfig,
    known_mounts: Arc<Mutex<HashMap<String, MountEntry>>>,
    expected_mounts: Arc<Mutex<HashMap<String, FstabEntry>>>,
    known_propagation: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Tracks which mount points we've already alerted as read-only, so we
    /// only alert on the *transition* into RO, not every poll cycle.
    known_readonly: Arc<Mutex<HashMap<String, bool>>>,
    last_alerts: Arc<Mutex<HashMap<String, (AlertSeverity, u64)>>>,
}

impl Mountd {
    fn new(config: MountdConfig) -> Result<Self> {
        let expected = Self::parse_fstab(&config)?;

        Ok(Self {
            socket: Arc::new(Mutex::new(None)),
            daemon_name: "mountd".to_string(),
            pid: std::process::id(),
            config,
            known_mounts: Arc::new(Mutex::new(HashMap::new())),
            expected_mounts: Arc::new(Mutex::new(expected)),
            known_propagation: Arc::new(Mutex::new(HashMap::new())),
            known_readonly: Arc::new(Mutex::new(HashMap::new())),
            last_alerts: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Parse /etc/fstab for expected mounts with x-osiris-alert options
    fn parse_fstab(config: &MountdConfig) -> Result<HashMap<String, FstabEntry>> {
        let mut expected = HashMap::new();
        let content = fs::read_to_string(&config.fstab_path).unwrap_or_default();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 6 {
                continue;
            }

            let device = parts[0].to_string();
            let mount_point = parts[1].to_string();
            let fs_type = parts[2].to_string();
            let options_str = parts[3];
            let dump = parts[4].parse::<u8>().unwrap_or(0);
            let pass = parts[5].parse::<u8>().unwrap_or(0);

            if matches!(fs_type.as_str(),
                "proc" | "sysfs" | "devpts" | "tmpfs" | "devtmpfs" |
                "cgroup" | "cgroup2" | "pstore" | "bpf" | "autofs" | "mqueue" |
                "configfs" | "debugfs" | "tracefs" | "securityfs" | "selinuxfs" |
                "efivarfs" | "hugetlbfs" | "fuse.gvfsd-fuse" | "fusectl") {
                continue;
            }

            let options: Vec<String> = options_str.split(',').map(|s| s.to_string()).collect();
            let mut alert_severity = None;
            let mut alert_on_missing = false;
            let mut alert_on_fs_mismatch = false;
            let mut alert_on_options_change = false;
            let mut disk_warning_pct = None;
            let mut disk_critical_pct = None;

            // NOTE: x-osiris-alert options previously conflated severity level
            // and boolean triggers under one key (documented gap — flagged
            // separately, not a bug fix in this pass). Kept as-is here.
            for opt in &options {
                if opt.starts_with("x-osiris-alert=") {
                    let val = &opt["x-osiris-alert=".len()..];
                    match val {
                        "critical" => alert_severity = Some(AlertSeverity::Critical),
                        "warning" => alert_severity = Some(AlertSeverity::Warning),
                        "info" => alert_severity = Some(AlertSeverity::Info),
                        "missing" => alert_on_missing = true,
                        "fsmismatch" => alert_on_fs_mismatch = true,
                        "optionschange" => alert_on_options_change = true,
                        _ => {}
                    }
                } else if opt.starts_with("x-osiris-disk-warning=") {
                    let val = &opt["x-osiris-disk-warning=".len()..];
                    disk_warning_pct = val.parse::<f32>().ok();
                } else if opt.starts_with("x-osiris-disk-critical=") {
                    let val = &opt["x-osiris-disk-critical=".len()..];
                    disk_critical_pct = val.parse::<f32>().ok();
                }
            }

            // A mount explicitly listed with "ro" in fstab options is expected
            // to be read-only — don't alert on it being read-only.
            let expected_readonly = options.iter().any(|o| o == "ro");

            let entry = FstabEntry {
                device,
                mount_point: mount_point.clone(),
                fs_type,
                options,
                dump,
                pass,
                alert_severity,
                alert_on_missing,
                alert_on_fs_mismatch,
                alert_on_options_change,
                expected_readonly,
                disk_warning_pct,
                disk_critical_pct,
            };

            expected.insert(mount_point, entry);
        }

        info!("[mountd] Parsed {} expected mounts from fstab", expected.len());
        Ok(expected)
    }

    /// Parse /proc/self/mountinfo for current mounts
    async fn parse_mountinfo(&self) -> Result<HashMap<String, MountEntry>> {
        let content = fs::read_to_string(&self.config.mountinfo_path)?;
        let mut mounts = HashMap::new();

        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 10 {
                continue;
            }

            let mount_id = parts[0].parse::<u32>().unwrap_or(0);
            let parent_id = parts[1].parse::<u32>().unwrap_or(0);

            let dev_parts: Vec<&str> = parts[2].split(':').collect();
            let major = dev_parts.first().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let minor = dev_parts.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);

            let root = parts[3].to_string();
            let mount_point = parts[4].to_string();

            let mut fs_type = String::new();
            let mut mount_options = Vec::new();
            let mut optional_fields = Vec::new();

            for (i, part) in parts.iter().enumerate().skip(5) {
                if *part == "-" {
                    if i + 1 < parts.len() {
                        fs_type = parts[i + 1].to_string();
                    }
                    break;
                }
                if part.starts_with("shared:") || part.starts_with("master:") || part.starts_with("propagate:") || part.starts_with("unbindable") {
                    optional_fields.push(part.to_string());
                } else {
                    mount_options = part.split(',').map(|s| s.to_string()).collect();
                }
            }

            let propagation_flags: Vec<String> = optional_fields.iter()
                .filter(|s| s.starts_with("shared:") || s.starts_with("master:") || s.starts_with("propagate:") || s.as_str() == "unbindable")
                .cloned()
                .collect();

            let entry = MountEntry {
                mount_id,
                parent_id,
                major,
                minor,
                root,
                mount_point: mount_point.clone(),
                fs_type,
                mount_options,
                optional_fields,
                propagation_flags,
            };

            mounts.insert(mount_point, entry);
        }

        Ok(mounts)
    }

    /// Fallback mount parsing using `mount` command (for proot environments).
    /// Runs the subprocess on a blocking thread since Command::output() is
    /// synchronous and would otherwise stall the tokio worker thread.
    async fn parse_mount_cmd(&self) -> Result<HashMap<String, MountEntry>> {
        let mount_cmd = self.config.mount_cmd.clone();

        let stdout = tokio::task::spawn_blocking(move || -> Result<String> {
            let output = Command::new(&mount_cmd)
                .output()
                .context("Failed to run mount command")?;
            if !output.status.success() {
                return Err(anyhow::anyhow!("mount command failed"));
            }
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }).await.context("mount command task panicked")??;

        let mut mounts = HashMap::new();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 6 || parts[1] != "on" || parts[3] != "type" {
                continue;
            }

            let mount_point = parts[2].to_string();
            let fs_type = parts[4].to_string();
            let options_str = parts[5].trim_start_matches('(').trim_end_matches(')');
            let mount_options = options_str.split(',').map(|s| s.to_string()).collect();

            let entry = MountEntry {
                mount_id: 0,
                parent_id: 0,
                major: 0,
                minor: 0,
                root: "/".to_string(),
                mount_point: mount_point.clone(),
                fs_type,
                mount_options,
                optional_fields: vec![],
                propagation_flags: vec![],
            };

            mounts.insert(mount_point, entry);
        }

        Ok(mounts)
    }

    /// Get current mounts (try mountinfo first, fallback to mount command)
    async fn get_current_mounts(&self) -> Result<HashMap<String, MountEntry>> {
        match self.parse_mountinfo().await {
            Ok(mounts) if !mounts.is_empty() => Ok(mounts),
            _ => {
                if self.config.proot_fallback {
                    warn!("[mountd] mountinfo empty/failed, falling back to mount command");
                    self.parse_mount_cmd().await
                } else {
                    self.parse_mountinfo().await
                }
            }
        }
    }

    /// Get disk usage for a mount point. statvfs is a fast local syscall,
    /// not a blocking network/IPC call, so this stays synchronous — it does
    /// not warrant spawn_blocking the way subprocess calls do.
    fn get_disk_usage(mount_point: &str) -> Result<DiskUsage> {
        let c_path = CString::new(mount_point)?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let result = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if result != 0 {
            return Err(anyhow::anyhow!("statvfs failed"));
        }

        let block_size = stat.f_frsize as u64;
        let total_blocks = stat.f_blocks as u64;
        let free_blocks = stat.f_bfree as u64;
        let available_blocks = stat.f_bavail as u64;

        let total_bytes = total_blocks * block_size;
        let free_bytes = free_blocks * block_size;
        let available_bytes = available_blocks * block_size;
        let used_bytes = total_bytes - free_bytes;
        let usage_pct = if total_bytes > 0 {
            (used_bytes as f32 / total_bytes as f32) * 100.0
        } else { 0.0 };

        Ok(DiskUsage {
            total_bytes,
            used_bytes,
            available_bytes,
            usage_pct,
        })
    }

    /// Check ZFS pool status. Runs zpool as a subprocess on a blocking thread.
    /// FIX: the previous version hardcoded state to "UNKNOWN" for every pool
    /// found and then compared "UNKNOWN" != "ONLINE" twice (a no-op duplicate
    /// condition), which meant every ZFS pool alerted as degraded on every
    /// single poll regardless of actual health. This version actually parses
    /// the state field from `zpool status -x` output.
    async fn check_zfs_pools(&self) -> Result<Vec<ZfsPoolStatus>> {
        let output = tokio::task::spawn_blocking(|| {
            Command::new("zpool").args(["status", "-x"]).output()
        }).await.context("zpool status task panicked")?;

        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();

                // "all pools are healthy" is zpool's exact message when there is
                // nothing to report — treat that as zero pools needing attention,
                // not as zero pools existing.
                if stdout.trim().eq_ignore_ascii_case("all pools are healthy") {
                    return Ok(vec![]);
                }

                let mut pools = Vec::new();
                let mut current: Option<ZfsPoolStatus> = None;

                for line in stdout.lines() {
                    let line = line.trim();
                    if let Some(rest) = line.strip_prefix("pool:") {
                        if let Some(p) = current.take() {
                            pools.push(p);
                        }
                        current = Some(ZfsPoolStatus {
                            name: rest.trim().to_string(),
                            state: "UNKNOWN".to_string(),
                            scan: String::new(),
                            scrub_in_progress: false,
                            scrub_errors: 0,
                        });
                    } else if let Some(rest) = line.strip_prefix("state:") {
                        if let Some(p) = current.as_mut() {
                            p.state = rest.trim().to_string();
                        }
                    } else if let Some(rest) = line.strip_prefix("scan:") {
                        if let Some(p) = current.as_mut() {
                            let rest = rest.trim();
                            p.scan = rest.to_string();
                            p.scrub_in_progress = rest.contains("scrub in progress");
                        }
                    } else if line.contains("errors:") {
                        if let Some(p) = current.as_mut() {
                            // e.g. "errors: No known data errors" or "errors: 3"
                            if let Some(count) = line.split(':').nth(1)
                                .and_then(|s| s.trim().parse::<u64>().ok())
                            {
                                p.scrub_errors = count;
                            }
                        }
                    }
                }
                if let Some(p) = current.take() {
                    pools.push(p);
                }

                Ok(pools)
            }
            Ok(_) => Ok(vec![]), // non-zero exit without stdout we can parse — treat as no pools
            Err(_) => Ok(vec![]), // zpool not installed
        }
    }

    /// Check Btrfs scrub status. Both findmnt and btrfs subprocess calls run
    /// on a blocking thread now.
    async fn check_btrfs_scrub(&self) -> Result<Vec<BtrfsStatus>> {
        let findmnt_out = tokio::task::spawn_blocking(|| {
            Command::new("findmnt").args(["-t", "btrfs", "-n", "-o", "TARGET"]).output()
        }).await.context("findmnt task panicked")?;

        let mut statuses = Vec::new();

        let Ok(out) = findmnt_out else { return Ok(statuses) };
        if !out.status.success() {
            return Ok(statuses);
        }

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let mount_points: Vec<String> = stdout.lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        for mp in mount_points {
            let mp_clone = mp.clone();
            let scrub_out = tokio::task::spawn_blocking(move || {
                Command::new("btrfs").args(["scrub", "status", &mp_clone]).output()
            }).await.context("btrfs scrub status task panicked")?;

            if let Ok(scrub) = scrub_out {
                if scrub.status.success() {
                    let scrub_stdout = String::from_utf8_lossy(&scrub.stdout);
                    let scrub_running = scrub_stdout.contains("running");
                    let mut errors = 0;
                    let mut progress_pct = None;

                    for line in scrub_stdout.lines() {
                        if line.contains("errors:") {
                            if let Some(e) = line.split_whitespace().nth(1) {
                                errors = e.parse().unwrap_or(0);
                            }
                        }
                        if line.contains("progress:") {
                            if let Some(p) = line.split(':').nth(1) {
                                progress_pct = p.trim().trim_end_matches('%').parse().ok();
                            }
                        }
                    }

                    statuses.push(BtrfsStatus {
                        mount_point: mp,
                        scrub_running,
                        scrub_progress_pct: progress_pct,
                        scrub_errors: errors,
                    });
                }
            }
        }

        Ok(statuses)
    }

    async fn connect_to_bridge(&self) -> Result<()> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let mut socket_guard = self.socket.lock().await;
        *socket_guard = Some(stream);
        info!("[mountd] Connected to AkerNet Bridge at {}", SOCKET_PATH);
        Ok(())
    }

    async fn register(&self) -> Result<()> {
        let msg = DaemonMessage::Register {
            name: self.daemon_name.clone(),
            pid: self.pid,
            version: VERSION.to_string(),
        };
        self.send_frame(msg).await?;
        debug!("[mountd] Registration sent");
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
                        debug!("[mountd] Socket read error: {}", e);
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
                info!("[mountd] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[mountd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[mountd] Received Reload from Bridge, re-parsing fstab");
                let expected = Self::parse_fstab(&self.config)?;
                *self.expected_mounts.lock().await = expected;
            }
            BridgeMessage::Restart => {
                info!("[mountd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[mountd] Shutting down gracefully");
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
        info!("[mountd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Check mount state and emit alerts
    async fn check_mounts(&self) -> Result<()> {
        let current = self.get_current_mounts().await?;
        let expected = self.expected_mounts.lock().await.clone();
        let known = self.known_mounts.lock().await.clone();
        let known_prop = self.known_propagation.lock().await.clone();
        let mut known_ro = self.known_readonly.lock().await.clone();

        for (mount_point, expected_entry) in &expected {
            if !current.contains_key(mount_point) {
                let severity = expected_entry.alert_severity.as_ref().cloned().unwrap_or(AlertSeverity::Critical);
                self.emit_alert(&format!("mount_missing_{}", mount_point.replace('/', "_")),
                    severity, json!({
                        "alert_type": MountAlertType::MountMissing.as_str(),
                        "metric": "mount_missing",
                        "mount_point": mount_point,
                        "expected_fs_type": expected_entry.fs_type,
                        "device": expected_entry.device,
                        "message": format!("Expected mount {} is missing", mount_point),
                    })).await?;
            } else {
                let current_entry = &current[mount_point];

                if current_entry.fs_type != expected_entry.fs_type && expected_entry.alert_on_fs_mismatch {
                    self.emit_alert(&format!("mount_fs_mismatch_{}", mount_point.replace('/', "_")),
                        AlertSeverity::Warning, json!({
                            "alert_type": MountAlertType::FsTypeMismatch.as_str(),
                            "metric": "mount_fs_type_mismatch",
                            "mount_point": mount_point,
                            "expected_fs_type": expected_entry.fs_type,
                            "actual_fs_type": current_entry.fs_type,
                            "message": format!("Filesystem type mismatch on {}: expected {}, got {}", mount_point, expected_entry.fs_type, current_entry.fs_type),
                        })).await?;
                }

                if expected_entry.alert_on_options_change && current_entry.mount_options != expected_entry.options {
                    self.emit_alert(&format!("mount_options_changed_{}", mount_point.replace('/', "_")),
                        AlertSeverity::Warning, json!({
                            "alert_type": MountAlertType::OptionsMismatch.as_str(),
                            "metric": "mount_options_mismatch",
                            "mount_point": mount_point,
                            "expected_options": expected_entry.options,
                            "actual_options": current_entry.mount_options,
                            "message": format!("Mount options changed on {}", mount_point),
                        })).await?;
                }

                if let Some(known_flags) = known_prop.get(mount_point) {
                    if known_flags != &current_entry.propagation_flags && !current_entry.propagation_flags.is_empty() {
                        self.emit_alert(&format!("mount_prop_changed_{}", mount_point.replace('/', "_")),
                            AlertSeverity::Info, json!({
                                "alert_type": MountAlertType::MountPropagationChanged.as_str(),
                                "metric": "mount_propagation_changed",
                                "mount_point": mount_point,
                                "previous_flags": known_flags,
                                "current_flags": current_entry.propagation_flags,
                                "message": format!("Mount propagation flags changed on {}", mount_point),
                            })).await?;
                    }
                }

                if let Ok(usage) = Self::get_disk_usage(mount_point) {
                    let warning_pct = expected_entry.disk_warning_pct.unwrap_or(self.config.disk_usage_warning_pct);
                    let critical_pct = expected_entry.disk_critical_pct.unwrap_or(self.config.disk_usage_critical_pct);

                    if usage.usage_pct >= critical_pct {
                        self.emit_alert(&format!("disk_critical_{}", mount_point.replace('/', "_")),
                            AlertSeverity::Critical, json!({
                                "alert_type": MountAlertType::DiskUsageCritical.as_str(),
                                "metric": "disk_usage_critical",
                                "mount_point": mount_point,
                                "usage_pct": usage.usage_pct,
                                "threshold_pct": critical_pct,
                                "total_bytes": usage.total_bytes,
                                "used_bytes": usage.used_bytes,
                                "available_bytes": usage.available_bytes,
                                "message": format!("Disk usage critical on {}: {:.1}%", mount_point, usage.usage_pct),
                            })).await?;
                    } else if usage.usage_pct >= warning_pct {
                        self.emit_alert(&format!("disk_warning_{}", mount_point.replace('/', "_")),
                            AlertSeverity::Warning, json!({
                                "alert_type": MountAlertType::DiskUsageWarning.as_str(),
                                "metric": "disk_usage_warning",
                                "mount_point": mount_point,
                                "usage_pct": usage.usage_pct,
                                "threshold_pct": warning_pct,
                                "total_bytes": usage.total_bytes,
                                "used_bytes": usage.used_bytes,
                                "available_bytes": usage.available_bytes,
                                "message": format!("Disk usage high on {}: {:.1}%", mount_point, usage.usage_pct),
                            })).await?;
                    }
                }

                // FIX: only alert on the *transition* into read-only, and never
                // for a mount that's expected to be read-only per fstab (e.g.
                // an intentional ro boot partition). Previously this fired
                // every poll cycle (~every 30s) forever for any ro mount.
                let is_ro_now = current_entry.mount_options.contains(&"ro".to_string());
                let was_ro_before = known_ro.get(mount_point).copied().unwrap_or(false);

                if is_ro_now && !was_ro_before && !expected_entry.expected_readonly {
                    self.emit_alert(&format!("fs_readonly_{}", mount_point.replace('/', "_")),
                        AlertSeverity::Warning, json!({
                            "alert_type": MountAlertType::FilesystemReadOnly.as_str(),
                            "metric": "filesystem_readonly",
                            "mount_point": mount_point,
                            "message": format!("Filesystem {} became read-only unexpectedly", mount_point),
                        })).await?;
                }
                known_ro.insert(mount_point.clone(), is_ro_now);
            }
        }

        for (mount_point, current_entry) in &current {
            if !expected.contains_key(mount_point) {
                if !matches!(current_entry.fs_type.as_str(),
                    "proc" | "sysfs" | "devpts" | "tmpfs" | "devtmpfs" | "cgroup" | "cgroup2" |
                    "pstore" | "bpf" | "autofs" | "mqueue" | "configfs" | "debugfs" | "tracefs" |
                    "securityfs" | "selinuxfs" | "efivarfs" | "hugetlbfs" | "fusectl" | "fuse.gvfsd-fuse" |
                    "overlay" | "squashfs" | "iso9660" | "udf") {
                    self.emit_alert(&format!("mount_unexpected_{}", mount_point.replace('/', "_")),
                        AlertSeverity::Warning, json!({
                            "alert_type": MountAlertType::MountUnexpected.as_str(),
                            "metric": "mount_unexpected",
                            "mount_point": mount_point,
                            "fs_type": current_entry.fs_type,
                            "device": format!("{}:{}", current_entry.major, current_entry.minor),
                            "message": format!("Unexpected mount detected: {} ({})", mount_point, current_entry.fs_type),
                        })).await?;
                }
            }
        }

        for (mount_point, _known_entry) in &known {
            if !current.contains_key(mount_point) && expected.contains_key(mount_point) {
                self.emit_alert(&format!("mount_disappeared_{}", mount_point.replace('/', "_")),
                    AlertSeverity::Warning, json!({
                        "alert_type": MountAlertType::MountDegraded.as_str(),
                        "metric": "mount_disappeared",
                        "mount_point": mount_point,
                        "message": format!("Mount {} disappeared", mount_point),
                    })).await?;
            }
        }

        // FIX: real state parsing means this now only fires for pools that are
        // genuinely not ONLINE, instead of every pool on every poll.
        if let Ok(pools) = self.check_zfs_pools().await {
            for pool in pools {
                if pool.state != "ONLINE" && pool.state != "UNKNOWN" {
                    self.emit_alert(&format!("zfs_pool_degraded_{}", pool.name),
                        AlertSeverity::Critical, json!({
                            "alert_type": MountAlertType::ZfsPoolDegraded.as_str(),
                            "metric": "zfs_pool_degraded",
                            "pool_name": pool.name,
                            "state": pool.state,
                            "message": format!("ZFS pool {} is in state: {}", pool.name, pool.state),
                        })).await?;
                }
                if pool.scrub_in_progress && self.config.zfs_scrub_alert {
                    self.emit_alert(&format!("zfs_scrub_running_{}", pool.name),
                        AlertSeverity::Info, json!({
                            "alert_type": MountAlertType::ZfsScrubRunning.as_str(),
                            "metric": "zfs_scrub_running",
                            "pool_name": pool.name,
                            "scan": pool.scan,
                            "message": format!("ZFS scrub running on pool {}", pool.name),
                        })).await?;
                }
                if pool.scrub_errors > 0 {
                    self.emit_alert(&format!("zfs_scrub_errors_{}", pool.name),
                        AlertSeverity::Critical, json!({
                            "alert_type": MountAlertType::ZfsScrubErrors.as_str(),
                            "metric": "zfs_scrub_errors",
                            "pool_name": pool.name,
                            "errors": pool.scrub_errors,
                            "message": format!("ZFS scrub found {} errors on pool {}", pool.scrub_errors, pool.name),
                        })).await?;
                }
            }
        }

        if let Ok(btrfs_statuses) = self.check_btrfs_scrub().await {
            for status in btrfs_statuses {
                if status.scrub_running && self.config.btrfs_scrub_alert {
                    self.emit_alert(&format!("btrfs_scrub_running_{}", status.mount_point.replace('/', "_")),
                        AlertSeverity::Info, json!({
                            "alert_type": MountAlertType::BtrfsScrubRunning.as_str(),
                            "metric": "btrfs_scrub_running",
                            "mount_point": status.mount_point,
                            "progress_pct": status.scrub_progress_pct,
                            "message": format!("Btrfs scrub running on {}", status.mount_point),
                        })).await?;
                }
                if status.scrub_errors > 0 {
                    self.emit_alert(&format!("btrfs_scrub_errors_{}", status.mount_point.replace('/', "_")),
                        AlertSeverity::Critical, json!({
                            "alert_type": MountAlertType::BtrfsScrubErrors.as_str(),
                            "metric": "btrfs_scrub_errors",
                            "mount_point": status.mount_point,
                            "errors": status.scrub_errors,
                            "message": format!("Btrfs scrub found {} errors on {}", status.scrub_errors, status.mount_point),
                        })).await?;
                }
            }
        }

        *self.known_mounts.lock().await = current.clone();
        let mut new_prop = HashMap::new();
        for (mp, entry) in &current {
            if !entry.propagation_flags.is_empty() {
                new_prop.insert(mp.clone(), entry.propagation_flags.clone());
            }
        }
        *self.known_propagation.lock().await = new_prop;
        *self.known_readonly.lock().await = known_ro;

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
    info!("[mountd] Osiris Mount Daemon v{} starting", VERSION);

    let config = MountdConfig::default();
    let mountd = Mountd::new(config)?;
    mountd.connect_to_bridge().await?;
    mountd.register().await?;

    tokio::time::timeout(std::time::Duration::from_millis(mountd.config.registration_timeout_ms), async {
        let mut interval = interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            if let Err(e) = mountd.handle_bridge_messages().await {
                debug!("[mountd] Bridge message handler error: {}", e);
            }
        }
    }).await.ok();

    info!("[mountd] Registration complete, entering monitoring loop");

    let poll_interval_ms = mountd.config.poll_interval_ms;
    let flush_interval_ms = mountd.config.flush_interval_ms;

    let mut poll_interval = interval(std::time::Duration::from_millis(poll_interval_ms));
    let mut flush_interval = interval(std::time::Duration::from_millis(flush_interval_ms));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = mountd.check_mounts().await {
                    error!("[mountd] Mount check error: {}", e);
                }
            }
            _ = flush_interval.tick() => {
                if let Err(e) = mountd.handle_bridge_messages().await {
                    debug!("[mountd] Bridge message handler error: {}", e);
                }
            }
        }
    }
}
