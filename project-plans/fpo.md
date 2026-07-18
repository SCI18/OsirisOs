# fpo.md — Findings for Systems Spec: Bridge + Ma'at Architecture Review

**Date:** 2026-07-17
**From:** Sitratis (CEO/Supervisor), via independent review with Claude
**To:** Systems Spec

## Instructions

1. Read `akernet/akernet-bridge/src/main.rs` and all of `maat/` (`lib.rs`, `daemon.rs`, `message.rs`, `registry.rs`, `error.rs`) in full before responding.
2. Do not assume the findings below are complete or correctly diagnosed — verify each one against the actual source before treating it as fact.
3. Propose a concrete architectural fix for each confirmed finding.
4. Write your full answer **in this same file**, appended below the findings, under a `## Systems Spec Response` heading. Cite exact file/line/function for every claim. Do not reference decisions, precedents, or prior fixes that are not verifiable in the current repo state.

---

## Findings (unverified — confirm or refute each)

### 1. No real restart-on-failure despite the file header claiming it
`akernet-bridge/src/main.rs`'s header comment states "Supervise all 42 — restart on failure." The orchestrator loop (`run_orchestrator`) only ever spawns daemons whose status is `Pending`. Once a daemon transitions to `Failed`, `Stopped`, or times out from `Starting`, nothing in the code moves it back to `Pending`. There appears to be no actual restart mechanism.

Note: `maat::daemon::DaemonInfo` already has a `restart_count: u32` field and `DaemonStatus::Restarting` variant defined — both appear unused anywhere in the codebase. This may mean a restart mechanism was planned but never wired up, rather than needing to be designed from scratch.

### 2. Spawned child processes are not retained
In `spawn_daemon()`, `tokio::process::Command::new(name).spawn()` returns a `Child`, which is bound to `_child` and immediately dropped. Dropping a `tokio::process::Child` does not kill the process, but it does mean Bridge retains no handle to stop, signal, or directly manage any daemon it spawns — only what daemons self-report over the registration socket.

### 3. No registration authentication
`handle_daemon_connection()` accepts whatever `name`/`pid`/`version` arrives in a `DaemonMessage::Register` and calls `mark_running(&name, pid)` with no verification that the actual connecting process's PID matches the claimed `pid`. Any local process that can open `/tmp/osiris-bridge.sock` can currently claim to be any daemon name and have Bridge treat its `Alert`/`StatusUpdate`/`Shutdown` messages as authoritative.

### 4. `spawn_daemon` uses bare daemon names, not paths
`Command::new(name)` relies on the daemon binary being resolvable via `PATH`. Given the actual build layout (`daemons/system_core/<name>/`, built into a shared `target/` via the workspace), it's unclear whether/how these binaries are expected to land on `PATH`. Confirm whether this is intentional (e.g. Harvester/OPIUM installs them there eventually) or an unfinished piece.

### 5. `BridgeState` duplicates `DaemonRegistry` instead of using it
`maat::registry::DaemonRegistry` already implements `ready_to_spawn()`, `is_healthy()`, `status_summary()`, `running_names()` — real, working methods. `BridgeState` in `akernet-bridge/src/main.rs` maintains its own separate `HashMap<String, DaemonInfo>` and reimplements overlapping logic inline instead of holding/using a `DaemonRegistry` directly. Confirm whether there's a reason for this duplication or whether Bridge should be refactored to use Ma'at's registry as the single source of truth.

### 6. Dependency graph references a non-daemon
Daemon #42 (`updated`) lists `"opium"` in `depends_on`. OPIUM is not in the 42-daemon registry — it's a separate package-manager binary, not something that ever registers with Bridge and appears in `running`. Under the current `is_ready_to_spawn()` logic (checks `depends_on` against the `running` list), `updated` may be structurally unable to ever become ready to spawn. Confirm and propose a fix (e.g. a separate non-daemon dependency check, or removing this dependency).

### 7. Who supervises Bridge itself?
Kha (PID 1) is Bridge's direct parent process. Kha's current draft implementation (`kha-main.rs`, not yet finalized) `wait()`s on Bridge and detects if it exits, but restart policy was left as an open question. Proposal under discussion: Kha becomes the sole restart authority for Bridge, with a backoff/retry-limit policy, since Kha already holds the real process handle via the parent-child relationship — no separate watchdog process, to avoid violating the "daemons are sensors, never controllers" principle elsewhere in the system. Evaluate this proposal specifically — agree, refine, or counter-propose.

### 8. Proposed authentication mechanism under discussion
Proposal: use `SO_PEERCRED` (kernel-level, non-spoofable real PID/UID/GID of the connecting process on a Unix socket) in Bridge's connection handler, compared against the `pid` claimed in `Register`, rejecting mismatches. Evaluate this proposal specifically — agree, refine, or counter-propose.

---

## Systems Spec Response

### Finding 1: No real restart-on-failure
**Status: CONFIRMED**

**Evidence:**
- `akernet-bridge/src/main.rs:8` header: "Supervise all 42 — restart on failure."
- `run_orchestrator` (lines 216-243) filters for `d.status == DaemonStatus::Pending` (line 229) only.
- `check_registration_timeout` (lines 286-295) sets `Failed` on timeout but nothing transitions `Failed`/`Stopped` back to `Pending`.
- `maat/src/daemon.rs:50` defines `DaemonStatus::Restarting` variant.
- `maat/src/daemon.rs:81` defines `restart_count: u32` field.
- Neither `Restarting` nor `restart_count` are referenced anywhere in `akernet-bridge/src/main.rs` or `maat/`.

**Proposed Fix:**
Add restart logic to `run_orchestrator` loop. After spawning ready daemons, scan for `Failed` or `Stopped` daemons where `restartable == true` and `restart_count < MAX_RESTARTS` (suggest `MAX_RESTARTS = 3`). Transition them to `Restarting`, increment `restart_count`, wait exponential backoff (e.g., `2^restart_count * 1000ms`), then set status to `Pending`. Use existing `restartable` (maat/src/daemon.rs:80) and `restart_count` (maat/src/daemon.rs:81) fields.

---

### Finding 2: Spawned child processes not retained
**Status: CONFIRMED**

**Evidence:**
- `akernet-bridge/src/main.rs:257-258`: `tokio::process::Command::new(name).spawn()` returns `Child` bound to `_child`, immediately dropped.
- `DaemonInfo` (maat/src/daemon.rs:72-82) has no field for process handle.
- Bridge cannot signal/stop/monitor daemons directly; relies entirely on self-reporting.

**Proposed Fix:**
Add `pub child: Option<tokio::process::Child>` to `DaemonInfo` in `maat/src/daemon.rs`. In `spawn_daemon`, store the `Child` instead of dropping it: `d.child = Some(child)`. Add helper methods on `BridgeState` for `signal_daemon(name, signal)` and `wait_daemon(name)` that operate on the retained handle. This enables Bridge to enforce `Stop`/`Restart` messages and detect unexpected exits without waiting for daemon self-report.

---

### Finding 3: No registration authentication
**Status: CONFIRMED**

**Evidence:**
- `akernet-bridge/src/main.rs:177-183`: `handle_daemon_connection` accepts `Register { name, pid, version }` and calls `mark_running(&name, pid)` with no PID verification.
- Any process with socket access can claim any daemon name.

**Proposed Fix:**
Implement `SO_PEERCRED` verification in `handle_daemon_connection`. On Linux, use `socket::get_peer_cred()` (or `libc::getsockopt` with `SO_PEERCRED`) to get kernel-attested PID/UID/GID of the connecting process. Compare `peer_pid` against claimed `pid` in `Register`; reject on mismatch. This is kernel-enforced, non-spoofable. Add to `maat/src/message.rs` a `RegistrationError` variant for auth failures, and return error frame to client on rejection.

---

### Finding 4: `spawn_daemon` uses bare daemon names
**Status: CONFIRMED — UNFINISHED PIECE**

**Evidence:**
- `akernet-bridge/src/main.rs:257`: `Command::new(name)` with no path.
- Build layout: `daemons/system_core/<name>/` → built to `target/<profile>/<name>` via workspace.
- No installation step places binaries on `PATH` in current repo state.
- Harvester/OPIUM may eventually install to `/usr/bin` or similar, but not yet implemented.

**Proposed Fix:**
Change `spawn_daemon` to resolve binary path explicitly. Options:
1. Pass `--daemon-bin-dir` flag to Bridge at startup (e.g., `/opt/osiris/bin`), construct full path: `Command::new(format!("{}/{}", bin_dir, name))`.
2. Use `std::env::current_exe()` to find Bridge's binary directory, assume daemons co-located.
3. Harvester/OPIUM install phase copies daemons to standard location (`/usr/lib/osiris/daemons/`) and Bridge reads from there.

Option 1 is simplest for current hosted/proot environment. Add config to `BridgeState` or env var `OSIRIS_DAEMON_BIN_DIR`.

---

### Finding 5: `BridgeState` duplicates `DaemonRegistry`
**Status: CONFIRMED**

**Evidence:**
- `akernet-bridge/src/main.rs:46-88`: `BridgeState` wraps `HashMap<String, DaemonInfo>` and reimplements `running_names()`, `status_summary()`, etc.
- `maat/src/registry.rs:302-339`: `DaemonRegistry` already has `ready_to_spawn()`, `is_healthy()`, `status_summary()`, `running_names()`, `by_domain()`, `get()` — all working.
- `BridgeState::new()` (lines 46-53) loads registry then copies into own map, discarding the registry instance.

**Proposed Fix:**
Make `BridgeState` hold `DaemonRegistry` directly:
```rust
pub struct BridgeState {
    pub registry: DaemonRegistry,
}
```
Use `registry.daemons` for name-based lookups (via `get()`), delegate `ready_to_spawn()`, `running_names()`, `status_summary()`, `is_healthy()` to registry. Remove duplicate logic. This makes registry the single source of truth.

---

### Finding 6: Dependency graph references non-daemon `opium`
**Status: CONFIRMED**

**Evidence:**
- `maat/src/registry.rs:282-286`: Daemon #42 `updated` has `depends_on: vec!["logd", "netd", "opium"]`.
- "opium" is not in the 42-daemon registry (no `DaemonInfo` entry for it).
- `DaemonInfo::is_ready_to_spawn` (maat/src/daemon.rs:104-106) checks `depends_on.iter().all(|dep| running.contains(dep))` against `running` names from `BridgeState::running_names()`, which only contains daemon names.
- Since "opium" never registers, `updated` can never become ready.

**Proposed Fix:**
Remove "opium" from `updated`'s `depends_on`. `updated` only needs `netd` for network checks and `logd` for logging. OPIUM is a package manager binary, not a supervised daemon.

---

### Finding 7: Who supervises Bridge?
**Status: CONFIRMED — OPEN QUESTION IN CODE**

**Evidence (from `kha/src/main.rs`):**
- `kha/src/main.rs:125-126`: `spawn_bridge()` returns `Child` handle retained in `bridge` variable (line 126).
- `kha/src/main.rs:148-156`: `bridge.wait()` resolves when Bridge exits; match arm logs error but **does not restart**.
- `kha/src/main.rs:157-163`: **Explicit TODO comment** (lines 157-163):
  ```rust
  // TODO: decide with Systems Spec — does Kha restart Bridge,
  // or is Bridge's death treated as fatal for the whole system?
  // Current behavior: log and continue watching for signals, but the 
  // system has no orchestrator running at this point since Bridge is gone. 
  // Not resolved here deliberately rather than guessing at recovery policy.
  ```
- `kha/src/main.rs:163`: Logs "No orchestrator running. System is in a degraded state." and continues signal loop.

**My Position: AGREE with Kha as restart authority, with defined policy.**

This is the correct architectural decision:
- Kha (PID 1) is Bridge's direct parent. Kernel gives Kha the real process handle via parent-child relationship.
- Kha already reaps zombies (`reap_zombies()` at lines 100-112) and forwards signals (`forward_signal_to_bridge` at lines 88-94).
- Adding restart with backoff keeps supervision hierarchy clean: Kernel → Kha → Bridge → 42 daemons. No separate watchdog daemon (avoids violating "daemons are sensors, never controllers" principle).

**Proposed Policy:**
- Exponential backoff: 1s, 2s, 4s, 8s, max 30s.
- Max retries: 5 attempts, then halt (kernel panic equivalent).
- Track `restart_count` in Kha's local state.
- On each Bridge exit, increment count, wait backoff, respawn via `spawn_bridge()`.
- If max retries exceeded: log fatal error and exit.

**Implementation in `kha/src/main.rs`:**
- Add `bridge_restart_count: u32 = 0` and `const MAX_BRIDGE_RESTARTS: u32 = 5` and `const MAX_BACKOFF_MS: u64 = 30000` before main loop.
- In the `bridge.wait()` match arm (lines 148-156), after logging, check `bridge_restart_count < MAX_BRIDGE_RESTARTS`. If yes: `bridge_restart_count += 1`, wait `std::cmp::min(1000 * 2^bridge_restart_count, MAX_BACKOFF_MS)`, call `spawn_bridge()` again, update `bridge_pid`, continue loop. If no: log fatal, exit.

---

### Finding 8: SO_PEERCRED authentication
**Status: CONFIRMED — AGREE**

**Evidence:**
- Finding 3 confirms no auth currently.
- `SO_PEERCRED` is Linux kernel feature returning `struct ucred { pid_t pid; uid_t uid; gid_t gid; }` via `getsockopt(SOL_SOCKET, SO_PEERCRED, ...)`.
- Non-spoofable: kernel fills creds, not user-space.

**My Position: AGREE — implement as mandatory for all registrations.**

**Implementation:**
- In `handle_daemon_connection`, after `listener.accept()`, call `stream.get_peer_cred()` (via `tokio::net::UnixStream` extension or raw `libc::getsockopt`).
- Extract `pid`, compare to `Register.pid`. Mismatch → send error frame, close connection.
- Also verify `uid == 0` (root) or specific `osiris` user if non-root daemons exist.
- Log rejection with peer creds for audit.

This is the minimal, correct fix for Finding 3. No alternative needed.
