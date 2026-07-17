# Mountd Daemon Implementation Plan

## Job
**mountd** (Daemon #6, SystemCore) — "Filesystem mount management post-boot"
**Sensor only** — monitors, reports, alerts. Never mounts/unmounts.

## Monitors

| Source | What | Frequency |
|--------|------|-----------|
| `/proc/self/mountinfo` | All mounts (device, path, fstype, options, propagation) | 10-30s poll |
| `/proc/mounts` | Fallback for proot/container | Same |
| `/etc/fstab` | Expected mounts at boot | Startup |
| `/sys/fs/` | Vendor-specific health (zfs, btrfs) | 60s poll |
| `inotify` on `/etc/fstab.d/` or `/run/mount/` | Dynamic mount units | Event-driven |

**Proot fallback**: Parse `mount` command output.

---

## Alerts (via `DaemonMessage::Alert`)

| Alert | Trigger | Severity |
|-------|---------|----------|
| `MountMissing` | Expected mount (fstab) absent | `Critical` |
| `MountDegraded` | Options mismatch (ro/rw, noexec, etc.) | `Warning` |
| `MountUnexpected` | New mount not in fstab | `Info` |
| `MountPropagationChanged` | Propagation flags changed | `Warning` |
| `FilesystemFull` | Usage > 90% (configurable) | `Critical` |
| `FilesystemReadOnly` | Unexpected ro remount | `Critical` |
| `ZfsPoolDegraded` / `BtrfsScrubRunning` | Vendor health signals | `Warning`/`Info` |

---

## DaemonMessage Variants

Current enum has: `Register`, `StatusUpdate`, `Error`, `Shutdown`
**healthd precedent**: Added `Alert` + `AlertSeverity` enum
**mountd needs**: Same `Alert` variant — **no new variants** beyond healthd's.

**Action**: Flag Networks Spec to confirm `Alert` variant is approved/implemented.

---

## Implementation Phases

1. **Protocol** — Confirm `Alert` variant with Networks Spec
2. **Crate** — Create `mountd/` in workspace (add to `Cargo.toml`)
3. **Config** — `MountdConfig` (TOML): poll interval, fstab path, disk thresholds, ignored prefixes
4. **Registration** — Ma'at Unix socket → `Register` → await `Acknowledged`
5. **Scanner** — Parse `/proc/self/mountinfo` (structured)
6. **Fstab Parser** — Track expected mounts for `MountMissing`
7. **Polling Loop** — Diff current vs previous state
8. **Alert Emission** — Send `DaemonMessage::Alert` with severity + payload
9. **Status Reporting** — Periodic `StatusUpdate` (mount count, last scan)
10. **Logging** — Internal errors via `DaemonMessage::Error` → logd
11. **Shutdown** — Handle `BridgeMessage::Stop` gracefully
12. **Tests** — Unit (parsers), integration (mock Bridge), proot fixtures

---

## Files to Create

```
mountd/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── registry.rs
│   ├── scanner.rs
│   ├── alerts.rs
│   └── state.rs
```

---

## Clarifying Questions

1. **Alert thresholds**: Per-mountpoint via fstab options (e.g., `x-osiris-alert-pct=85`) or global config only?

2. **Proot fallback priority**: `mount` cmd → `/proc/mounts` → `/proc/self/mountinfo` — what order for Termux/proot?

3. **Expected mount sources**: Only `/etc/fstab`, or also systemd `.mount` units (`/etc/systemd/system/*.mount`), or `/run/mount/` drop-ins?

4. **ZFS/btrfs alerts**: Emit vendor-specific alerts here, or defer to future `zfsd`/`btrfsd` daemon?

---

## Dependencies on Other Specs

| Dependency | Status |
|------------|--------|
| `DaemonMessage::Alert` variant | **Needed** — flag Networks Spec |
| `AlertSeverity` enum | **Needed** — same |
| Ma'at Unix socket path | Defined in maat |
| logd logging format | Defined by logd |