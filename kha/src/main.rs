// Kha — Osiris Init System
// PID 1. The life force. Minimal by design.
// "What breathes life into the system, breathes it quietly."
//
// Responsibilities (and nothing more):
//   1. Mount essential filesystems
//   2. Reap orphaned child processes (zombie reaping)
//   3. Forward signals to children
//   4. Spawn AkerNet Bridge
//   5. Stay alive — if Kha dies, the kernel panics

use std::process::{Command, Child};
use std::time::Duration;
use std::thread;

const VERSION: &str    = "0.1.0-alpha";
const BRIDGE_BIN: &str = "akernet-bridge";

fn main() {
    println!("[kha] Osiris Init v{}", VERSION);
    println!("[kha] The life force awakens.");

    // Mount essential virtual filesystems
    if let Err(e) = mount_essentials() {
        eprintln!("[kha] Warning: mount step incomplete: {}", e);
        // Non-fatal in dev/proot environments — continue
    }

    // Spawn AkerNet Bridge — the daemon orchestrator
    let mut bridge = match spawn_bridge() {
        Ok(child) => {
            println!("[kha] AkerNet Bridge spawned (pid {})", child.id());
            child
        }
        Err(e) => {
            eprintln!("[kha] FATAL: Could not spawn AkerNet Bridge: {}", e);
            eprintln!("[kha] System cannot continue without the Bridge.");
            std::process::exit(1);
        }
    };

    println!("[kha] System is alive. Entering supervision loop.");

    // Main supervision loop
    // Kha's only job from here: reap zombies, watch the bridge
    loop {
        // Reap any orphaned child processes
        reap_zombies();

        // Check if Bridge is still alive
        match bridge.try_wait() {
            Ok(Some(status)) => {
                eprintln!("[kha] AkerNet Bridge exited with: {}", status);
                eprintln!("[kha] Attempting restart...");
                match spawn_bridge() {
                    Ok(child) => {
                        println!("[kha] AkerNet Bridge restarted (pid {})", child.id());
                        bridge = child;
                    }
                    Err(e) => {
                        eprintln!("[kha] FATAL: Could not restart Bridge: {}", e);
                        eprintln!("[kha] Halting.");
                        std::process::exit(1);
                    }
                }
            }
            Ok(None) => {
                // Bridge is running — all good
            }
            Err(e) => {
                eprintln!("[kha] Error checking Bridge status: {}", e);
            }
        }

        // Sleep before next supervision tick
        // Short enough to catch failures fast, long enough not to burn CPU
        thread::sleep(Duration::from_millis(500));
    }
}

/// Mount essential virtual filesystems
/// Safe to call in proot — mounts will fail silently if already present
fn mount_essentials() -> Result<(), String> {
    let mounts = [
        ("proc",     "/proc",     "proc"),
        ("sysfs",    "/sys",      "sysfs"),
        ("devtmpfs", "/dev",      "devtmpfs"),
        ("devpts",   "/dev/pts",  "devpts"),
        ("tmpfs",    "/run",      "tmpfs"),
    ];

    for (fstype, target, name) in &mounts {
        let status = Command::new("mount")
            .args(&["-t", fstype, name, target])
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("[kha] Mounted: {}", target);
            }
            _ => {
                // In proot/dev environments, mounts often already exist
                // Not fatal — log and continue
                println!("[kha] Skipped (already mounted or unavailable): {}", target);
            }
        }
    }

    Ok(())
}

/// Spawn the AkerNet Bridge process
fn spawn_bridge() -> Result<Child, String> {
    println!("[kha] Spawning AkerNet Bridge...");
    Command::new(BRIDGE_BIN)
        .spawn()
        .map_err(|e| format!("Failed to spawn {}: {}", BRIDGE_BIN, e))
}

/// Reap zombie (orphaned) child processes
/// PID 1 must do this or zombies accumulate indefinitely
fn reap_zombies() {
    loop {
        // waitpid with WNOHANG — non-blocking, returns immediately
        // if no children have exited
        let result = libc_waitpid(-1, std::ptr::null_mut(), libc_wnohang());

        if result <= 0 {
            break; // No more zombies to reap
        }

        println!("[kha] Reaped zombie process: pid {}", result);
    }
}

// Minimal libc bindings for waitpid
// Avoids pulling in the full libc crate for PID 1
extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

fn libc_waitpid(pid: i32, status: *mut i32, options: i32) -> i32 {
    unsafe { waitpid(pid, status, options) }
}

fn libc_wnohang() -> i32 {
    1 // WNOHANG constant — don't block if no child has exited
}
