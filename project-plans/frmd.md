# Project Next Steps / Milestones — Osiris OS

*Generated from full workspace analysis — 2026-07-16*

---

## Current State Summary

| Component | Status |
|-----------|--------|
| **Workspace** | ✅ Cargo workspace with 13 crates |
| **Kha (PID 1)** | ✅ Implemented — spawns akernet-bridge, reaps zombies, mounts essentials |
| **AkerNet Bridge** | ✅ Implemented — Unix socket listener, HTTP surface, orchestrator, daemon registry |
| **Ma'at** | ✅ Implemented — IPC protocol (`DaemonMessage`, `BridgeMessage`, `Frame`), daemon registry |
| **Harvester** | ✅ Implemented — harvest/install/remove/list for `.osr` packages |
| **OPIUM** | ✅ Implemented — CLI package manager delegating to Harvester |
| **Ra** | ⚠️ Stub — Wayland rewrite needed (external `~/projects/ra` exists) |
| **Anubis/Thoth** | ⚠️ Workspace members, status unknown |
| **healthd/logd/mountd/timed/entropyd/kha-watchd** | ❌ Not implemented (Daemons #1-6 of SystemCore) |

---

## Immediate Next Milestones (Priority Order)

### **Milestone 0: Hosted Osiris Core** *(per osiris-rm.json)*
**Goal:** Runnable Osiris core inside Termux/proot
- [ ] `cargo build --workspace` succeeds (currently passes `cargo check`)
- [ ] Kha runs as hosted process, supervises akernet-bridge
- [ ] Bridge exposes `/` and `/health` endpoints
- [ ] Harvester installs/removes/lists a simple `.osr` package
- [ ] OPIUM calls Harvester and maintains package metadata

### **Stage 1: Foundation — Core Abyss Daemons (SystemCore #1-6)**
**Build Order (per daemons-spec.md):** `timed → entropyd → mountd → kha-watchd`  
*(healthd and logd are reference implementations — but don't exist in code yet)*

| Daemon | ID | Responsibility | Blockers |
|--------|-----|----------------|----------|
| **logd** | #2 | Unified logging, ring buffer | **Needs `DaemonMessage::Alert` + `AlertSeverity` added to Ma'at** |
| **healthd** | #3 | CPU/RAM/thermal monitoring | Same protocol blocker |
| **timed** | #4 | System time, NTP sync | Needs protocol + Ma'at registration pattern |
| **entropyd** | #5 | Entropy pool, RNG seeding | Same |
| **mountd** | #6 | Filesystem mount monitoring (sensor-only) | **Same blocker + decisions in nmtd.md** |
| **kha-watchd** | #1 | Monitors Kha itself, heartbeat | Last in order (depends on Kha running) |

---

## Critical Path Blockers (Must Resolve First)

| Blocker | Owner | Status |
|---------|-------|--------|
| **`DaemonMessage::Alert` variant + `AlertSeverity` enum missing from Ma'at** | Networks Spec → Ma'at | **Approved in decision log** — needs implementation in `maat/src/message.rs` |
| **Alert payload schema for mountd/healthd** | Daemons Spec | Defined in `nmtd.md` — mountd crate owns it |
| **healthd & logd reference implementations don't exist** | Daemons Spec | Must be built first as templates |

---

## Recommended Execution Order

### **Step 1: Protocol Fix (Networks Spec → Ma'at)**
Add to `maat/src/message.rs`:
```rust
// In DaemonMessage enum
Alert {
    name: String,
    severity: AlertSeverity,
    payload: serde_json::Value,  // daemon-specific
    timestamp: String,
}

// New enum
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertSeverity { Info, Warning, Critical }
```

### **Step 2: Reference Daemons (Daemons Spec)**
Build **logd** and **healthd** as templates — they establish:
- Ma'at registration pattern
- Unix socket communication
- Alert emission pattern
- Proot fallbacks (`/proc` parsing)

### **Step 3: mountd (Daemons Spec)**
Follow `mountd.md` + `nmtd.md` decisions:
- Per-mountpoint thresholds via `x-osiris-alert-pct=85` in fstab
- Proot fallback: `/proc/self/mountinfo` → `/proc/mounts` → `mount` cmd
- Phase 1: `/etc/fstab` only; Phase 2: systemd `.mount` units
- ZFS/Btrfs alerts emitted here at Warning/Info; detailed events deferred to `zfsd`/`btrfsd`

### **Step 4: timed, entropyd, kha-watchd**
Standard sensor daemons following healthd/logd pattern.

### **Step 5: Remaining Stage 1**
- AkerNet Bridge fully integrated (already close)
- Harvester/OPIUM package flow end-to-end
- Remaining 36 Abyss daemons (Hardware, Input, Display, Network, Audio, UserSession, Security, Services)

---

## Questions Before Proceeding

1. **healthd/logd existence**: Are there external implementations in `~/projects/` that should be imported, or build from scratch?
2. **Proot environment constraints**: Any mount/namespace limitations that affect daemon development (e.g., `/proc` visibility)?
3. **Ra integration timeline**: Stage 3 depends on Wayland rewrite — should daemon work pause at Stage 2, or proceed in parallel?
4. **External source reconciliation**: `~/projects/ra` and `~/projects/netjeru` — import now or later?

---

## Suggested First Action
**Start with Step 1** — add `Alert` variant + `AlertSeverity` to `maat/src/message.rs`. This unblocks all daemon work and matches the approved Networks Spec decision.
