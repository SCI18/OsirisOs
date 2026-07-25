// kha — PID 1, the life-force / spirit double
// "Everything ground up. Every name earns its place."
//
// SCOPE (per kha.md): mount essentials, spawn AkerNet Bridge as sole child,
// reap zombies, forward signals to Bridge, stay alive unconditionally.
//
// Kha deliberately has NO Ma'at/IPC surface. It does not register with
// itself, does not speak DaemonMessage/BridgeMessage. The only channel into
// this process is OS signals.
//
// METRICS EXPOSURE: Kha exposes internal metrics (zombie reap count, signal
// forwarding count) via a one-way Unix datagram socket (SOCK_DGRAM) at
// /run/osiris/kha-metrics.sock. No commands accepted, no state machine,
// kernel-buffered, SO_PEERCRED-verifiable by consumers (kha-watchd).

use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UnixDatagram;
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Mutex;
use tokio::time::interval;
use tracing::{info, warn, error, debug};

/// Filesystems Kha mounts at boot, before Bridge is spawned. Order matters:
/// /proc and /sys are needed by almost everything else, /dev before /run.
const ESSENTIAL_MOUNTS: &[(&str, &str, &str)] = &[
    // (source, target, fstype)
    ("proc", "/proc", "proc"),
    ("sysfs", "/sys", "sysfs"),
    ("devtmpfs", "/dev", "devtmpfs"),
    ("tmpfs", "/run", "tmpfs"),
];

const BRIDGE_BINARY_PATH: &str = "/usr/local/bin/akernet-bridge";

/// Metrics socket path — created in /run/osiris (tmpfs, recreated on boot)
const KHA_METRICS_SOCK: &str = "/run/osiris/kha-metrics.sock";

/// Metrics emission interval
const METRICS_EMIT_INTERVAL_MS: u64 = 5000;

/// Kha internal metrics, updated atomically from signal handlers
struct KhaMetrics {
    /// Total zombies reaped since boot
    zombies_reaped: AtomicU64,
    /// Total signals forwarded to Bridge since boot
    signals_forwarded: AtomicU64,
    /// Last reap timestamp (unix secs)
    last_reap_ts: AtomicU64,
    /// Last signal forward timestamp (unix secs)
    last_forward_ts: AtomicU64,
}

impl KhaMetrics {
    fn new() -> Self {
        Self {
            zombies_reaped: AtomicU64::new(0),
            signals_forwarded: AtomicU64::new(0),
            last_reap_ts: AtomicU64::new(0),
            last_forward_ts: AtomicU64::new(0),
        }
    }

    fn record_reap(&self, count: u32) {
        self.zombies_reaped.fetch_add(count as u64, Ordering::Relaxed);
        self.last_reap_ts.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Ordering::Relaxed,
        );
    }

    fn record_signal_forward(&self) {
        self.signals_forwarded.fetch_add(1, Ordering::Relaxed);
        self.last_forward_ts.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Ordering::Relaxed,
        );
    }

    fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.zombies_reaped.load(Ordering::Relaxed),
            self.signals_forwarded.load(Ordering::Relaxed),
            self.last_reap_ts.load(Ordering::Relaxed),
            self.last_forward_ts.load(Ordering::Relaxed),
        )
    }
}

/// Binary metrics frame sent over SOCK_DGRAM
/// Layout (little-endian):
///   u64 zombies_reaped
///   u64 signals_forwarded
///   u64 last_reap_ts
///   u64 last_forward_ts
/// Total: 32 bytes — fits in single datagram, no fragmentation
fn encode_metrics(zombies: u64, signals: u64, last_reap: u64, last_forward: u64) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..8].copy_from_slice(&zombies.to_le_bytes());
    buf[8..16].copy_from_slice(&signals.to_le_bytes());
    buf[16..24].copy_from_slice(&last_reap.to_le_bytes());
    buf[24..32].copy_from_slice(&last_forward.to_le_bytes());
    buf
}

// FIX (fpo.md Finding 7, Systems Spec-approved policy): Kha is the sole
// restart authority for Bridge, using exponential backoff with a hard
// ceiling. Beyond the ceiling, Kha treats this as fatal rather than
// crash-looping forever.
const MAX_BRIDGE_RESTARTS: u32 = 5;
const BRIDGE_RESTART_BASE_MS: u64 = 1000;
const MAX_BRIDGE_BACKOFF_MS: u64 = 30_000;

/// Mount essential filesystems. Each mount is attempted independently —
/// under proot, some of these may already be mounted or may fail due to
/// namespace restrictions; a failure here is logged but not fatal, since
/// Kha staying alive matters more than any single mount succeeding (proot
/// environments commonly have /proc and /sys already bind-mounted through
/// from the host before Kha ever runs).
fn mount_essentials() {
    for (source, target, fstype) in ESSENTIAL_MOUNTS {
        match mount_one(source, target, fstype) {
            Ok(()) => info!("[kha] Mounted {} at {}", fstype, target),
            Err(e) => warn!("[kha] Failed to mount {} at {} ({}) — continuing", fstype, target, e),
        }
    }
}

fn mount_one(source: &str, target: &str, fstype: &str) -> Result<()> {
    std::fs::create_dir_all(target).ok(); // best-effort, target may already exist
    let source_c = std::ffi::CString::new(source)?;
    let target_c = std::ffi::CString::new(target)?;
    let fstype_c = std::ffi::CString::new(fstype)?;

    let result = unsafe {
        libc::mount(
            source_c.as_ptr(),
            target_c.as_ptr(),
            fstype_c.as_ptr(),
            0,
            std::ptr::null(),
        )
    };

    if result != 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow::anyhow!("mount({}, {}, {}) failed: {}", source, target, fstype, err));
    }
    Ok(())
}

/// Spawn AkerNet Bridge as Kha's sole child.
async fn spawn_bridge() -> Result<Child> {
    let child = Command::new(BRIDGE_BINARY_PATH)
        .spawn()
        .context("Failed to spawn AkerNet Bridge")?;
    info!("[kha] Spawned AkerNet Bridge, pid={:?}", child.id());
    Ok(child)
}

/// Forward a received signal to Bridge, if it's still alive.
fn forward_signal_to_bridge(bridge_pid: u32, sig: libc::c_int, metrics: &KhaMetrics) {
    let result = unsafe { libc::kill(bridge_pid as i32, sig) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        warn!("[kha] Failed to forward signal {} to Bridge (pid={}): {}", sig, bridge_pid, err);
    } else {
        metrics.record_signal_forward();
        debug!("[kha] Forwarded signal {} to Bridge (pid={})", sig, bridge_pid);
    }
}

/// Reap any zombie children. Called on SIGCHLD. Uses waitpid with WNOHANG
/// in a loop to reap all currently-zombied children in one pass, not just
/// one — multiple children can become zombies between signal deliveries
/// since signals don't queue by default.
fn reap_zombies(metrics: &KhaMetrics) -> u32 {
    let mut reaped = 0u32;
    loop {
        let mut status: i32 = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break; // no more zombies to reap right now
        }
        reaped += 1;
        info!("[kha] Reaped zombie pid={}", pid);
    }
    if reaped > 0 {
        metrics.record_reap(reaped);
    }
    reaped
}

/// Metrics emission task — runs independently, sends datagrams to SOCK_DGRAM
async fn metrics_emitter(metrics: Arc<KhaMetrics>) {
    // Ensure socket directory exists
    if let Err(e) = std::fs::create_dir_all("/run/osiris") {
        warn!("[kha] Failed to create /run/osiris: {}", e);
    }

    // Remove stale socket
    let _ = std::fs::remove_file(KHA_METRICS_SOCK);

    // Create SOCK_DGRAM socket (connectionless, one-way)
    let sock = match UnixDatagram::bind(KHA_METRICS_SOCK) {
        Ok(s) => {
            info!("[kha] Metrics socket bound at {}", KHA_METRICS_SOCK);
            s
        }
        Err(e) => {
            error!("[kha] Failed to bind metrics socket: {}", e);
            return;
        }
    };

    let mut interval = interval(Duration::from_millis(METRICS_EMIT_INTERVAL_MS));

    loop {
        interval.tick().await;
        let (zombies, signals, last_reap, last_forward) = metrics.snapshot();
        let frame = encode_metrics(zombies, signals, last_reap, last_forward);
        
        // Send to self (kernel loops back) — any listener on the socket receives it
        if let Err(e) = sock.send_to(&frame, KHA_METRICS_SOCK).await {
            debug!("[kha] Metrics send failed (no listener?): {}", e);
        }
    }
}

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create Tokio runtime")?
        .block_on(async {
            tracing_subscriber::fmt::init();
            info!("[kha] Osiris init starting (PID {})", std::process::id());

            if std::process::id() != 1 {
                warn!("[kha] Not running as PID 1 — expected under proot/dev environments, but confirm this is intentional before relying on PID-1 semantics.");
            }

            let metrics = Arc::new(KhaMetrics::new());

            // Spawn metrics emitter task
            tokio::spawn(metrics_emitter(Arc::clone(&metrics)));

            mount_essentials();

            let mut bridge = spawn_bridge().await?;
            let bridge_pid = bridge.id().context("Bridge has no PID immediately after spawn")?;

            // Explicit handlers only for the signals Kha is designed to care about.
            let mut sigchld = signal(SignalKind::child())?;
            let mut sigterm = signal(SignalKind::terminate())?;
            let mut sigint = signal(SignalKind::interrupt())?;

            info!("[kha] Entering main loop, watching Bridge (pid={})", bridge_pid);

            let mut bridge_pid = bridge_pid;
            let mut bridge_restart_count: u32 = 0;

            loop {
                tokio::select! {
                    _ = sigchld.recv() => {
                        let reaped = reap_zombies(&metrics);
                        if reaped > 0 {
                            info!("[kha] Reaped {} zombie(s) (total: {})", 
                                reaped, metrics.zombies_reaped.load(Ordering::Relaxed));
                        }
                    }
                    status = bridge.wait() => {
                        match status {
                            Ok(exit_status) => {
                                error!("[kha] AkerNet Bridge exited unexpectedly: {:?}", exit_status);
                            }
                            Err(e) => {
                                error!("[kha] Error waiting on Bridge: {}", e);
                            }
                        }

                        if bridge_restart_count >= MAX_BRIDGE_RESTARTS {
                            error!("[kha] Bridge has failed {} times — exceeding restart ceiling. Treating as fatal.", bridge_restart_count);
                            error!("[kha] System cannot continue without an orchestrator. Exiting.");
                            std::process::exit(1);
                        }

                        let backoff_ms = (BRIDGE_RESTART_BASE_MS * 2u64.pow(bridge_restart_count))
                            .min(MAX_BRIDGE_BACKOFF_MS);
                        bridge_restart_count += 1;
                        warn!(
                            "[kha] Restarting Bridge (attempt {}/{}) after {}ms backoff",
                            bridge_restart_count, MAX_BRIDGE_RESTARTS, backoff_ms
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;

                        match spawn_bridge().await {
                            Ok(new_bridge) => {
                                bridge = new_bridge;
                                bridge_pid = match bridge.id() {
                                    Some(pid) => pid,
                                    None => {
                                        error!("[kha] Restarted Bridge has no PID — treating as fatal.");
                                        std::process::exit(1);
                                    }
                                };
                                info!("[kha] Bridge restarted successfully, pid={}", bridge_pid);
                            }
                            Err(e) => {
                                error!("[kha] Failed to respawn Bridge: {} — will retry on next iteration if under ceiling", e);
                            }
                        }
                    }
                    _ = sigterm.recv() => {
                        info!("[kha] Received SIGTERM, forwarding to Bridge and shutting down");
                        forward_signal_to_bridge(bridge_pid, libc::SIGTERM, &metrics);
                        let _ = bridge.wait().await;
                        break;
                    }
                    _ = sigint.recv() => {
                        info!("[kha] Received SIGINT, forwarding to Bridge and shutting down");
                        forward_signal_to_bridge(bridge_pid, libc::SIGINT, &metrics);
                        let _ = bridge.wait().await;
                        break;
                    }
                }
            }
            info!("[kha] Shutting down");
            Ok(())
        })
    }