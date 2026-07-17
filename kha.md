# Kha & kha-watchd — Systems Spec Analysis

## Summary

**Only one daemon exists in the spec: `kha-watchd` (Daemon #1, SystemCore).**  
There is no separate `watchd` daemon in the 42-daemon Abyss Network. The user's reference to both "kha-watchd" and "watchd" appears to be a conflation — this document clarifies the actual architecture.

---

## Kha (PID 1 Init)

**Role:** The life-force/spirit double. Minimal Rust binary (~300 lines).  
**Responsibilities:**
- Mount essential filesystems (`/proc`, `/sys`, `/dev`, `/run`)
- Reap zombie processes (SIGCHLD handler)
- Forward signals to children
- Spawn AkerNet Bridge (its **only** child)
- Stay alive — if Kha dies, the kernel panics

**What Kha does NOT do:**
- Service supervision (that's AkerNet Bridge)
- Daemon management (that's AkerNet Bridge)
- Logging, networking, time, entropy, mounts (those are SystemCore daemons)

**Dependency:** None. Kha is the root.

---

## kha-watchd (Daemon #1, SystemCore)

**Role:** Monitors Kha itself + system heartbeat.  
**Responsibility (per osiris-rm.json):** "Monitors Kha itself, system heartbeat"

**Proposed scope:**
1. **Kha liveness** — verify Kha (PID 1) is responsive via signal(0) or /proc/1/stat
2. **Heartbeat emission** — periodic `DaemonMessage::StatusUpdate` with `DaemonStatus::Running` to AkerNet Bridge
3. **Kha metric collection** — uptime, child reaping count, signal forwarding stats
4. **Alert on anomaly** — if Kha stops reaping zombies, or heartbeat missing > threshold → `DaemonMessage::Alert` (Critical)

**What kha-watchd does NOT do:**
- Manage Kha (cannot restart PID 1)
- Spawn/supervise other daemons (that's AkerNet Bridge)
- Mount filesystems, manage time, entropy, logs (other SystemCore daemons)

**Dependency chain:**
```
Kha (PID 1) → spawns → AkerNet Bridge → orchestrates → kha-watchd (via Ma'at registry)
```
kha-watchd registers with Ma'at *after* AkerNet Bridge is running. It is the **last SystemCore daemon to start** (build order: `timed → entropyd → mountd → kha-watchd`).

---

## fwatchd (Daemon #14, Hardware) — Not to Be Confused

Separate daemon: `fwatchd` — "Firmware/hardware fault monitoring" (Hardware domain). Monitors vendor firmware logs, EDAC, MCE, hardware error injection. Unrelated to Kha.

---

## Systems Spec Position

- **Kha** → `kha/` crate (workspace member, PID 1 binary)
- **kha-watchd** → `daemons/system_core/kha-watchd/` (follows new domain structure)
- **No `watchd` crate** — does not exist in spec

**Cross-domain impact:**  
- Networks Spec: kha-watchd uses `DaemonMessage::Alert` + `AlertSeverity` (already approved for healthd/mountd precedent)  
- Daemons Spec: Implements kha-watchd per build order  
- No other domains affected