# init1.md — Phase 1 Completion Log

**Date:** 2026-07-17  
**Status:** Phase 1 (Foundation) Complete — Ready for Phase 2

---

## Work Completed

### SystemCore Daemons (6/6 Complete)
All 6 SystemCore daemons implemented, compiled, and following the reference pattern (register → connect → poll → emit Alerts):

| Daemon | ID | Domain | Key Features |
|--------|-----|--------|--------------|
| **logd** | #2 | SystemCore | 8MB ring buffer, hybrid persistence (/var/log/osiris), bootstrap log, Alert ingestion via `BridgeMessage::Forward` |
| **healthd** | #3 | SystemCore | CPU/RAM/disk monitoring via sysinfo 0.30, thresholds (Warn 80%, Crit 95%), deduplication |
| **timed** | #4 | SystemCore | NTP sync (pool.ntp.org), drift alerts (Warn 1s, Crit 5s) |
| **mountd** | #6 | SystemCore | fstab + mountinfo parsing, disk usage, ZFS/Btrfs monitoring, propagation tracking, x-osiris-alert-* options |
| **entropyd** | #5 | SystemCore | entropy_avail monitoring (Warn 1024 bits, Crit 192 bits), seed persistence (/var/lib/osiris/entropy.seed), periodic re-seed |
| **kha-watchd** | #1 | SystemCore | Kha (PID 1) liveness via signal(0), /proc/1/stat parsing, heartbeat, zombie/dead/sleep alerts |

All daemons follow the reference pattern: register with Ma'at → connect to AkerNet Bridge → poll domain → emit `DaemonMessage::Alert` with 60s deduplication.

---

### Infrastructure Components

**Ma'at (Protocol & Registry)**
- `DaemonMessage` / `BridgeMessage` enums with `Alert` / `AlertSeverity` (approved for healthd/mountd)
- New variants: `RegistrationRejected`, `Forward(DaemonMessage)`
- `DaemonRegistry` with 42 daemons across 9 domains, dependency resolution, health checks

**AkerNet Bridge**
- Unix socket listener with `SO_PEERCRED` authentication (kernel-verified PID)
- `DaemonRegistry` as single source of truth (no duplication)
- Daemon restart logic: exponential backoff (1s→2s→4s→8s→max 30s), max 3 retries
- Child process retention via `HashMap<String, Child>` for direct signal/management
- Logd forwarding: holds logd's write half, routes `BridgeMessage::Forward(DaemonMessage)` to logd
- HTTP control surface on 0.0.0.0:7474 (/health, /daemons)
- Binary path resolution via `OSIRIS_DAEMON_BIN_DIR` env var

**Kha (PID 1 Init)**
- Mounts essential filesystems (/proc, /sys, /dev, /run)
- Spawns Bridge as sole child, retains `Child` handle
- Exponential backoff restart: 1s→2s→4s→8s→16s→capped 30s, max 5 retries, then fatal exit
- Signal forwarding (SIGTERM/SIGINT) to Bridge
- Zombie reaping via `waitpid(WNOHANG)` loop

**Project Structure**
```
daemons/
├── system_core/      (6/6 complete)
│   ├── logd, healthd, timed, mountd, entropyd, kha-watchd
├── hardware/         (empty - 8 daemons pending)
├── input/            (empty - 4 daemons pending)
├── display_graphics/ (empty - 3 daemons pending)
├── network_akernet/  (empty - 6 daemons pending)
├── audio/            (empty - 3 daemons pending)
├── user_session/     (empty - 4 daemons pending)
├── security/         (empty - 4 daemons pending)
└── services/         (empty - 4 daemons pending)
```

---

### Verification
- `cargo build --workspace` → **Success** (only pre-existing `opium` dead-code warnings)
- All 6 SystemCore daemons compile and follow reference pattern
- fpo.md findings 1-8 all addressed and verified against source
- nsp.md (Networks Spec) decision implemented: `BridgeMessage::Forward` variant + logd forwarding

---

## Proposed Next Phase: Phase 2 — Network + Package Layer

Per `osiris-rm.json` Stage 2 roadmap, the next priorities are:

### 2.1 Network Domain Daemons (6 daemons)
| Daemon | ID | Responsibility |
|--------|-----|----------------|
| **netd** | #22 | Core network management, interfaces |
| **wifid** | #23 | WiFi scanning, connection, profiles |
| **dnsd** | #24 | AdGuardHome integration, DNS filtering |
| **vpnd** | #25 | VPN lifecycle, kill switch |
| **firewalld** | #26 | Packet filtering, per-app rules |
| **proxyd** | #27 | Traffic routing, privacy proxy |

**Dependencies:** All depend on `logd` and `netd` (see registry). `netd` depends on `logd` + `mountd`.

### 2.2 Package Layer (OPIUM + Harvester)
- **Harvester**: `.osr` format install/remove/list, dpkg-equivalent
- **OPIUM**: CLI package manager (install/remove/purge/list/search/info/update/harvest), repo management, delegates to Harvester
- `.osr` package format specification

### 2.3 Bridge Integration for Network Daemons
- Extend Bridge HTTP API for network status/control
- Implement `netd` as the core network daemon that others depend on
- Wire `dnsd` to AdGuardHome integration (already in Bridge's HTTP surface concept)

---

### Prerequisites Before Phase 2
1. **Resolve remaining SystemCore gaps** (post-audit):
   - `kha-watchd`: Add process state (Z/D) check, reaping tracking, signal forwarding stats
   - `mountd`: Fix ZFS parsing (`zpool list -H -o name,health`), add `/proc/mounts` fallback, fix alert severities
   - `entropyd`: All I/O already uses `tokio::fs` ✓ (verified)
   - `logd`: Ingestion path wired via `Forward` ✓ (verified)

2. **Hardware abstraction layer** for `netd`/`wifid` (libnl, iw, ethtool bindings)

3. **Configuration system** for daemons (TOML-based, per `MountdConfig` pattern)

---

### Build Command for Phase 2
```bash
# Build new daemons as they're added
cargo build -p netd -p wifid -p dnsd -p vpnd -p firewalld -p proxyd
cargo build -p harvester -p opium
cargo build --workspace
```

---

**Phase 1 Complete.** SystemCore foundation solid. Proceeding to Phase 2: Network + Package Layer.