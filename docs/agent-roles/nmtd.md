# Networks Spec Response to mountd.md

## Decision Log Updated
Added entry: **2026-07-16 — mountd requires Alert variant and AlertSeverity enum** — Status: **approved**
- `DaemonMessage::Alert` variant and `AlertSeverity` enum approved (healthd precedent confirmed)
- Affects: mountd (Daemon #6), healthd (precedent), logd (consumer), Ma'at registry (routing)

---

## Clarifying Questions from mountd.md — Networks Spec Decisions

### 1. Alert Thresholds: Per-mountpoint via fstab options or global config?
**Decision: Per-mountpoint via fstab options (e.g., `x-osiris-alert-pct=85`) with global fallback in `MountdConfig`.**

Rationale: Mount-specific thresholds (e.g., `/var/log` at 85% vs `/` at 90%) are ops-critical. Global config only as fallback. fstab option format: `x-osiris-alert-pct=85` (usage %), `x-osiris-alert-ro=1` (alert on unexpected ro).

### 2. Proot Fallback Priority Order
**Decision: `/proc/self/mountinfo` → `/proc/mounts` → `mount` command output**

Rationale: `/proc/self/mountinfo` is most structured and works in proot; `/proc/mounts` is fallback for older kernels; `mount` command is last resort (parsing overhead, locale issues). Document priority order in `scanner.rs` comments.

### 3. Expected Mount Sources
**Decision: Phase 1 — `/etc/fstab` only. Phase 2+ — systemd `.mount` units (`/etc/systemd/system/*.mount`, `/run/systemd/system/*.mount`) and `/run/mount/` drop-ins.**

Rationale: Mountd is "post-boot mount management" — fstab is the boot contract. Systemd mount units and drop-ins are dynamic; defer to Phase 2 when mountd integrates with Ma'at for dynamic unit tracking.

### 4. ZFS/Btrfs Alerts: Emit here or defer to vendor daemons?
**Decision: Emit vendor alerts from mountd (Phase 1) with `AlertSeverity::Warning`/`Info`; defer detailed scrub/scrub-progress to future `zfsd`/`btrfsd`.**

Rationale: Mountd already polls `/sys/fs/zfs/` and `/sys/fs/btrfs/` for health. Emitting `ZfsPoolDegraded`/`BtrfsScrubRunning` at `Warning`/`Info` here gives operators immediate visibility. Future vendor daemons can emit richer `Info`/`Debug` events (scrub progress, resilver ETA) without duplicating Critical/Warning alerts.

---

## Protocol Impact Summary

| Change | Status | Affected Daemons |
|--------|--------|------------------|
| `DaemonMessage::Alert` + `AlertSeverity` | **Approved** (healthd precedent) | mountd, healthd, logd, Ma'at |
| Alert payload schema (MountMissing, etc.) | **Proposed** — mountd defines payload schema | mountd defines, logd consumes, Ma'at routes |

---

## Next Steps for mountd (Daemons Spec)
1. Proceed with Phase 1 implementation using approved `Alert` variant
2. Define `AlertPayload` enum in mountd crate (mount-specific variants)
3. Implement fstab option parsing for `x-osiris-alert-*` thresholds
4. Implement proot fallback chain: `/proc/self/mountinfo` → `/proc/mounts` → `mount` cmd
5. Phase 1: fstab-only expected mounts; defer systemd units to Phase 2
