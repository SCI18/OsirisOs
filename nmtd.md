# nmtd — Networks Daemon (Networks Spec Decisions)

## Networks Spec Decision Log Summary (from `docs/decisions/networks-spec-log.md`)

**Decision:** Approved `DaemonMessage::Alert` variant + `AlertSeverity` enum for mountd/healthd alerts.

**Precedent:** healthd already emits `DaemonMessage::Alert`; mountd reuses same variant.

---

## Mountd Clarifying Questions — Resolved

| Question | Decision |
|----------|----------|
| **Alert thresholds** | Per-mountpoint via fstab options (`x-osiris-alert-pct=85`, `x-osiris-alert-ro=1`) with global config fallback |
| **Proot fallback order** | `/proc/self/mountinfo` → `/proc/mounts` → `mount` command |
| **Expected mount sources (Phase 1)** | `/etc/fstab` only |
| **Expected mount sources (Phase 2+)** | systemd `.mount` units + `/run/mount/` drop-ins |
| **ZFS/Btrfs alerts (Phase 1)** | Emit `Warning`/`Info` alerts from mountd; defer detailed scrub/resilver events to future `zfsd`/`btrfsd` |

---

## Alert Payload Schema (for mountd → logd/Ma'at)

Defined in mountd crate (`mountd/src/alerts.rs`):

```rust
pub enum MountAlert {
    MountMissing { mountpoint: PathBuf, expected_fs: String },
    MountDegraded { mountpoint: PathBuf, reason: DegradedReason },
    MountReadOnly { mountpoint: PathBuf, unexpected: bool },
    UsageWarning { mountpoint: PathBuf, used_pct: u8, threshold_pct: u8 },
    UsageCritical { mountpoint: PathBuf, used_pct: u8, threshold_pct: u8 },
    FsError { mountpoint: PathBuf, error: String },
    ZfsScrubWarning { pool: String, message: String },
    BtrfsScrubWarning { fs: String, message: String },
}

pub enum DegradedReason {
    ReadOnlyUnexpected,
    UsageAboveThreshold,
    IoErrors,
    CorruptionSuspected,
}

pub enum AlertSeverity { Info, Warning, Critical }
```

Alert payload is wrapped in `DaemonMessage::Alert { severity, payload: MountAlert, timestamp }` and routed via Ma'at to logd/alertd.

---

## Phase 1 Scope (Mountd)

- Parse `/etc/fstab` for expected mounts (with `x-osiris-alert-*` options)
- Read `/proc/self/mountinfo` → `/proc/mounts` → `mount` fallback
- Emit `DaemonMessage::Alert` on: missing mount, unexpected RO, usage threshold breach, FS errors
- Emit `Warning`/`Info` for ZFS/Btrfs scrub/resilver (defer detailed events to zfsd/btrfsd)
- Route alerts via Ma'at to logd + alertd

---

## Phase 2+ Scope

- systemd `.mount` unit discovery via D-Bus
- `/run/mount/` drop-in support
- Dedicated `zfsd`/`btrfsd` daemons for detailed pool/fs events

---

## Files

- Spec decisions: `docs/decisions/networks-spec-log.md`
- Agent role detail: `docs/agent-roles/nmtd.md`
- Mountd implementation: `mountd/src/alerts.rs`, `mountd/src/fstab.rs`, `mountd/src/mountinfo.rs`
