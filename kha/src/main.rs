// kha — PID 1, the life-force / spirit double
// "Everything ground up. Every name earns its place."
//
// SCOPE (per kha.md): mount essentials, spawn AkerNet Bridge as sole child,
// reap zombies, forward signals to Bridge, stay alive unconditionally.
//
// Kha deliberately has NO Ma'at/IPC surface. It does not register with
// itself, does not speak DaemonMessage/BridgeMessage. The only channel into
// this process is OS signals — see the signal-handling section below for
// exactly which signals are treated as meaningful and why.
//
// OPEN QUESTION FOR SYSTEMS SPEC (flagged, not resolved here): under a real
// kernel, PID 1 has special signal semantics — it silently ignores any
// signal it hasn't explicitly installed a handler for. Under proot, PID 1
// is simulated, and it is not yet confirmed whether that protection is
// actually enforced or merely cosmetic. Until confirmed, this skeleton
// installs explicit handlers for the signals it cares about and treats
// everything else as unhandled by default (Rust/tokio's default signal
// behavior), rather than assuming proot replicates kernel PID-1 immunity.

use std::process::ExitStatus;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, warn, error};

/// Filesystems Kha mounts at boot, before Bridge is spawned. Order matters:
/// /proc and /sys are needed by almost everything else, /dev before /run.
const ESSENTIAL_MOUNTS: &[(&str, &str, &str)] = &[
    // (source, target, fstype)
    ("proc", "/proc", "proc"),
    ("sysfs", "/sys", "sysfs"),
    ("devtmpfs", "/dev", "devtmpfs"),
    ("tmpfs", "/run", "tmpfs"),
];

const BRIDGE_BINARY_PATH: &str = "/usr/local/bin/akernet-bridge"; // TODO confirm actual install path with Systems Spec

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
fn forward_signal_to_bridge(bridge_pid: u32, sig: libc::c_int) {
    let result = unsafe { libc::kill(bridge_pid as i32, sig) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        warn!("[kha] Failed to forward signal {} to Bridge (pid={}): {}", sig, bridge_pid, err);
    }
}

/// Reap any zombie children. Called on SIGCHLD. Uses waitpid with WNOHANG
/// in a loop to reap all currently-zombied children in one pass, not just
/// one — multiple children can become zombies between signal deliveries
/// since signals don't queue by default.
fn reap_zombies() -> u32 {
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
    reaped
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

    mount_essentials();

    let mut bridge = spawn_bridge().await?;
    let bridge_pid = bridge.id().context("Bridge has no PID immediately after spawn")?;

    // Explicit handlers only for the signals Kha is designed to care about.
    // Anything not listed here falls through to tokio/OS default behavior —
    // deliberately, per the open question at the top of this file, rather
    // than assuming proot grants PID-1 immunity to everything else.
    let mut sigchld = signal(SignalKind::child())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    info!("[kha] Entering main loop, watching Bridge (pid={})", bridge_pid);

    let mut bridge_pid = bridge_pid;
    let mut bridge_restart_count: u32 = 0;

    loop {
        tokio::select! {
            _ = sigchld.recv() => {
                let reaped = reap_zombies();
                if reaped > 0 {
                    info!("[kha] Reaped {} zombie(s)", reaped);
                }
                // If Bridge itself was among the reaped, its wait() below
                // will resolve on the next select iteration.
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

                // FIX (fpo.md Finding 7, Systems Spec-approved policy):
                // Kha is Bridge's sole restart authority. Exponential
                // backoff (1s, 2s, 4s, 8s, 16s, capped at 30s), max 5
                // attempts. Beyond that, treated as fatal — no orchestrator
                // running is a degraded state we don't crash-loop forever
                // trying to recover from.
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
                forward_signal_to_bridge(bridge_pid, libc::SIGTERM);
                let _ = bridge.wait().await;
                break;
            }
            _ = sigint.recv() => {
                info!("[kha] Received SIGINT, forwarding to Bridge and shutting down");
                forward_signal_to_bridge(bridge_pid, libc::SIGINT);
                let _ = bridge.wait().await;
                break;
            }
        }
    }
    info!("[kha] Shutting down");
    Ok(())
})
}
