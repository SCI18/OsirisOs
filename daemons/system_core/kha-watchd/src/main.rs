// kha-watchd — Assessor #1, SystemCore
// Monitors Kha (PID 1) itself, system heartbeat
// "The guardian is watched. The watcher is known."

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::DateTime;
use maat::{DaemonMessage, BridgeMessage, Frame, DaemonStatus, AlertSeverity};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{Duration, interval};
use tracing::{info, error, debug, warn};

const VERSION: &str = "0.1.0-alpha";
const SOCKET_PATH: &str = "/tmp/osiris-bridge.sock";
const REGISTRATION_TIMEOUT_MS: u64 = 5000;
const POLL_INTERVAL_MS: u64 = 10000; // 10 seconds
const HEARTBEAT_LOG_INTERVAL_MS: u64 = 300_000; // 5 minutes, wall-clock
const KHA_PID: i32 = 1;
/// If zombie children of Kha persist across this many consecutive polls,
/// something is wrong with Kha's reaping loop — alert.
const ZOMBIE_PERSIST_POLL_THRESHOLD: u32 = 3;

enum ReaderEvent {
    Bridge(BridgeMessage),
    Closed,
}

/// Kha process state, parsed from field 2 of /proc/1/stat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KhaState {
    Running,
    Sleeping,
    UninterruptibleSleep, // 'D' — often indicates I/O or kernel-level hang
    Zombie,               // 'Z' — should be structurally impossible for PID 1
    Stopped,
    Other(char),
}

impl KhaState {
    fn from_char(c: char) -> Self {
        match c {
            'R' => KhaState::Running,
            'S' => KhaState::Sleeping,
            'D' => KhaState::UninterruptibleSleep,
            'Z' => KhaState::Zombie,
            'T' | 't' => KhaState::Stopped,
            other => KhaState::Other(other),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            KhaState::Running => "running",
            KhaState::Sleeping => "sleeping",
            KhaState::UninterruptibleSleep => "uninterruptible_sleep",
            KhaState::Zombie => "zombie",
            KhaState::Stopped => "stopped",
            KhaState::Other(_) => "other",
        }
    }

    fn is_concerning(&self) -> bool {
        matches!(self, KhaState::Zombie | KhaState::UninterruptibleSleep | KhaState::Stopped)
    }
}

/// Kha-watchd state
struct KhaWatchd {
    socket_write_half: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    daemon_name: String,
    pid: u32,
    last_alerts: Arc<Mutex<std::collections::HashMap<String, (AlertSeverity, u64)>>>,
    kha_uptime_at_start: u64,
    /// Consecutive polls where a zombie child of Kha was observed.
    zombie_streak: Arc<Mutex<u32>>,
}

impl KhaWatchd {
    fn new() -> Result<Self> {
        let kha_uptime_at_start = Self::get_kha_uptime().unwrap_or(0);

        Ok(Self {
            socket_write_half: Arc::new(Mutex::new(None)),
            daemon_name: "kha-watchd".to_string(),
            pid: std::process::id(),
            last_alerts: Arc::new(Mutex::new(std::collections::HashMap::new())),
            kha_uptime_at_start,
            zombie_streak: Arc::new(Mutex::new(0)),
        })
    }

    fn get_kha_uptime() -> Result<u64> {
        let content = std::fs::read_to_string("/proc/1/stat")?;
        let parts: Vec<&str> = content.split_whitespace().collect();
        if parts.len() < 22 {
            return Err(anyhow::anyhow!("Invalid /proc/1/stat format"));
        }
        let starttime = parts[21].parse::<u64>()?;
        let uptime = std::fs::read_to_string("/proc/uptime")?;
        let uptime_secs = uptime.split_whitespace().next().unwrap_or("0").parse::<f64>().unwrap_or(0.0) as u64;
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        let start_time_secs = starttime / clk_tck.max(1);
        Ok(uptime_secs.saturating_sub(start_time_secs))
    }

    fn is_kha_alive() -> bool {
        unsafe { libc::kill(KHA_PID, 0) == 0 }
    }

    /// Get Kha stats: utime, stime, num_threads, and process state.
    /// FIX: previously only utime/stime/num_threads were parsed. Field 2
    /// (state) is now also read — this is the field kha.md explicitly
    /// required for detecting Kha stuck in zombie (Z) or uninterruptible
    /// sleep (D), both of which should be structurally near-impossible or
    /// deeply abnormal for PID 1 and worth alerting on immediately.
    fn get_kha_stats() -> Result<(u64, u64, u64, KhaState)> {
        let content = std::fs::read_to_string("/proc/1/stat")?;

        // Field 2 (comm) is the process name in parentheses and may itself
        // contain spaces, so we locate it by the last ')' rather than naive
        // whitespace splitting for the fields before it.
        let last_paren = content.rfind(')').ok_or_else(|| anyhow::anyhow!("malformed /proc/1/stat"))?;
        let after_comm = &content[last_paren + 1..];
        let rest: Vec<&str> = after_comm.split_whitespace().collect();
        // rest[0] is field 3 (state) since field 1 (pid) and field 2 (comm)
        // were consumed by the parenthesized-name split above.
        if rest.is_empty() {
            return Err(anyhow::anyhow!("Invalid /proc/1/stat format after comm field"));
        }
        let state_char = rest[0].chars().next().unwrap_or('?');
        let state = KhaState::from_char(state_char);

        // utime is field 14, stime field 15, num_threads field 20 in the
        // full /proc/pid/stat layout; relative to `rest` (which starts at
        // field 3) those are indices 11, 12, 17.
        let utime = rest.get(11).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let stime = rest.get(12).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let num_threads = rest.get(17).and_then(|s| s.parse::<u64>().ok()).unwrap_or(1);

        Ok((utime, stime, num_threads, state))
    }

    /// Count current zombie children of PID 1 by scanning /proc/*/stat for
    /// processes whose PPID (field 4, or field-after-comm index 2) is 1 and
    /// whose state is 'Z'.
    ///
    /// HONEST LIMITATION: this is an approximation of "is Kha behind on
    /// reaping right now", not a running total of zombies Kha has ever
    /// reaped. kha-watchd has no way to observe Kha's internal SIGCHLD
    /// handling count from outside the process — a true "reaping delta"
    /// counter would require Kha itself to expose that number (e.g. via a
    /// status field over the Ma'at socket). That's a Systems Spec /
    /// Kha-implementation decision, not something kha-watchd can fabricate
    /// by watching from the outside. What's implemented here is the closest
    /// externally-observable proxy: are zombies currently piling up under
    /// PID 1, which only happens if reaping isn't keeping up.
    fn count_kha_zombie_children() -> u32 {
        let mut count = 0u32;

        let Ok(entries) = std::fs::read_dir("/proc") else { return 0 };

        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else { continue };
            if !name_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }

            let stat_path = entry.path().join("stat");
            let Ok(content) = std::fs::read_to_string(&stat_path) else { continue };
            let Some(last_paren) = content.rfind(')') else { continue };
            let after_comm = &content[last_paren + 1..];
            let rest: Vec<&str> = after_comm.split_whitespace().collect();
            if rest.len() < 2 { continue; }

            let state_char = rest[0].chars().next().unwrap_or('?');
            let ppid: i32 = rest[1].parse().unwrap_or(-1);

            if state_char == 'Z' && ppid == KHA_PID {
                count += 1;
            }
        }

        count
    }

    async fn connect_to_bridge(&self) -> Result<mpsc::Receiver<ReaderEvent>> {
        let stream = UnixStream::connect(SOCKET_PATH).await?;
        let (read_half, write_half) = stream.into_split();
        *self.socket_write_half.lock().await = Some(write_half);
        info!("[kha-watchd] Connected to AkerNet Bridge at {}", SOCKET_PATH);

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
                        debug!("[kha-watchd] Socket read error: {}", e);
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
        debug!("[kha-watchd] Registration sent");
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
                info!("[kha-watchd] Registration acknowledged by Bridge: {}", name);
            }
            BridgeMessage::StatusRequest => {
                let status = DaemonMessage::StatusUpdate {
                    name: self.daemon_name.clone(),
                    status: DaemonStatus::Running,
                };
                self.send_frame(status).await?;
            }
            BridgeMessage::Stop => {
                info!("[kha-watchd] Received Stop from Bridge, shutting down");
                self.shutdown().await?;
                std::process::exit(0);
            }
            BridgeMessage::Reload => {
                info!("[kha-watchd] Received Reload from Bridge");
            }
            BridgeMessage::Restart => {
                info!("[kha-watchd] Received Restart from Bridge");
                self.shutdown().await?;
                std::process::exit(0);
            }
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("[kha-watchd] Shutting down gracefully");
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
        info!("[kha-watchd] Alert emitted: {} [{:?}]", alert_key, severity);
        Ok(())
    }

    /// Main check loop - monitors Kha health
    async fn check_kha(&self) -> Result<()> {
        let kha_alive = Self::is_kha_alive();

        if !kha_alive {
            self.emit_alert("kha_dead", AlertSeverity::Critical, json!({
                "metric": "kha_liveness",
                "status": "dead",
                "message": "Kha (PID 1) is not responding to signal(0)",
            })).await?;
            return Ok(());
        }

        let (_utime, _stime, num_threads, state) = Self::get_kha_stats().unwrap_or((0, 0, 1, KhaState::Other('?')));
        let current_uptime = Self::get_kha_uptime().unwrap_or(0);

        // FIX: alert on concerning process state (Z/D/T) — kha.md's core
        // requirement that was previously entirely unimplemented.
        if state.is_concerning() {
            self.emit_alert("kha_state_abnormal", AlertSeverity::Critical, json!({
                "metric": "kha_process_state",
                "state": state.as_str(),
                "message": format!("Kha (PID 1) is in abnormal state: {}", state.as_str()),
            })).await?;
        }

        // FIX: zombie-children-of-Kha tracking, as the closest externally
        // observable proxy for "is Kha's reaping loop keeping up". See the
        // honest-limitation note on count_kha_zombie_children().
        let zombie_count = Self::count_kha_zombie_children();
        {
            let mut streak = self.zombie_streak.lock().await;
            if zombie_count > 0 {
                *streak += 1;
            } else {
                *streak = 0;
            }

            if *streak >= ZOMBIE_PERSIST_POLL_THRESHOLD {
                self.emit_alert("kha_zombies_persisting", AlertSeverity::Warning, json!({
                    "metric": "kha_zombie_children",
                    "zombie_count": zombie_count,
                    "consecutive_polls": *streak,
                    "message": format!("{} zombie child(ren) of Kha have persisted for {} consecutive polls — reaping may be stalled", zombie_count, *streak),
                })).await?;
            }
        }

        let status = DaemonMessage::StatusUpdate {
            name: self.daemon_name.clone(),
            status: DaemonStatus::Running,
        };
        self.send_frame(status).await?;

        // FIX: heartbeat log now triggers on a wall-clock interval (see
        // main()'s separate `heartbeat_log_interval` timer) instead of
        // `kha_uptime % 300 == 0`, which only logged if Kha's uptime in
        // whole seconds happened to land exactly on a multiple of 300 at
        // the moment of a poll tick — since polls run every 10s, that
        // condition could be missed entirely depending on drift, or never
        // fire if uptime reporting has any jitter.
        debug!("[kha-watchd] Poll: Kha uptime={}s, threads={}, state={}, zombies={}",
            current_uptime, num_threads, state.as_str(), zombie_count);

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("[kha-watchd] Osiris Kha Watchdog Daemon v{} starting", VERSION);

    let kha_watchd = KhaWatchd::new()?;
    let mut reader_rx = kha_watchd.connect_to_bridge().await?;
    kha_watchd.register().await?;

    let _ = tokio::time::timeout(Duration::from_millis(REGISTRATION_TIMEOUT_MS), async {
        while let Some(event) = reader_rx.recv().await {
            match event {
                ReaderEvent::Bridge(msg) => {
                    let is_ack = matches!(msg, BridgeMessage::Acknowledged { .. });
                    if let Err(e) = kha_watchd.handle_bridge_message(msg).await {
                        debug!("[kha-watchd] Bridge message handler error: {}", e);
                    }
                    if is_ack { break; }
                }
                ReaderEvent::Closed => break,
            }
        }
    }).await;

    info!("[kha-watchd] Registration complete, entering monitoring loop");

    let mut poll_interval = interval(Duration::from_millis(POLL_INTERVAL_MS));
    let mut heartbeat_log_interval = interval(Duration::from_millis(HEARTBEAT_LOG_INTERVAL_MS));

    loop {
        tokio::select! {
            _ = poll_interval.tick() => {
                if let Err(e) = kha_watchd.check_kha().await {
                    error!("[kha-watchd] Kha check error: {}", e);
                }
            }
            _ = heartbeat_log_interval.tick() => {
                info!("[kha-watchd] Heartbeat: still monitoring Kha (PID 1)");
            }
            event = reader_rx.recv() => {
                match event {
                    Some(ReaderEvent::Bridge(msg)) => {
                        if let Err(e) = kha_watchd.handle_bridge_message(msg).await {
                            debug!("[kha-watchd] Bridge message handler error: {}", e);
                        }
                    }
                    Some(ReaderEvent::Closed) | None => {
                        warn!("[kha-watchd] Bridge connection closed, exiting");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
