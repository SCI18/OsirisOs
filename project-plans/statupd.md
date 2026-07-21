# Systems Spec — Status Update Log

## 2026-07-21 — Initial Workspace Analysis

### Project Overview
Osiris OS — Modular, freedom-first operating system ecosystem in Rust. Monorepo workspace with 13 crates targeting Linux kernel (currently hosted on Termux/proot Debian Trixie, aarch64).

### Workspace Structure (13 crates)
```
OsirisOs-main/
├── maat/                      # IPC protocol (DaemonMessage, BridgeMessage, Frame)
├── kha/                       # PID 1 init
├── akernet/akernet-bridge/    # Daemon orchestrator + HTTP surface
├── ra/ra-wm, ra-panel, ra-config, ra-shell/  # Wayland compositor (stub)
├── opium/                     # Package manager CLI (apt-equivalent)
├── harvester/                 # Low-level package installer (dpkg-equivalent)
├── anubis/                    # System manager (stub)
├── thoth/                     # Text editor (stub)
├── heka/                      # IDE (stub)
└── daemons/system_core/       # 6 SystemCore daemons:
    ├── logd/                  # Unified logging, ring buffer
    ├── healthd/               # CPU/RAM/thermal monitoring
    ├── timed/                 # System time, NTP sync
    ├── mountd/                # Filesystem mount monitoring
    ├── entropyd/              # Entropy pool, RNG seeding
    └── kha-watchd/            # Kha (PID 1) watchdog + heartbeat
```

---

## 2026-07-21 — SystemCore Daemon Audit (from ssasc.md)

### Executive Summary from Audit
**SystemCore: 67% compliant (4/6 daemons functionally complete, 2 with critical gaps)**

| Daemon | Spec Compliance | Critical Issues | Warnings |
|--------|----------------|-----------------|----------|
| **logd** (#2) | ⚠️ Partial | Missing `ingest_daemon_message` hook in main loop; unused `write_log`/`ingest_daemon_message` | Unused `ring_buffer` fields, `RING_BUFFER_MAX_BYTES` constant |
| **healthd** (#3) | ✅ Compliant | — | Temp thresholds defined but unused (no thermal sensor support in sysinfo 0.30) |
| **timed** (#4) | ✅ Compliant | — | NTP crate 0.3 uses sync API; measured RTT is local, not NTP protocol RTT |
| **mountd** (#6) | ⚠️ Partial | **ZFS parsing broken** — `zpool status -x` only outputs on error; Btrfs `findmnt` dependency not in Cargo.toml | Alert type `MountDegraded` emitted for disappeared mounts (should be `MountMissing` or new type) |
| **entropyd** (#5) | ⚠️ Partial | **Blocking I/O in async** — `fs::read_to_string`, `fs::write`, `fs::OpenOptions` in async fns; `/dev/random` read blocks | Critical threshold 192 bits (log says 192, code says 192 ✓); seed path `entropy.seed` ✓ |
| **kha-watchd** (#1) | ❌ Non-compliant | **Missing core spec features**: no process state check (Z/D), no zombie reaping tracking, no signal forwarding stats; heartbeat every 300s but poll is 10s | Unused `warn` import |

---

## 2026-07-21 — Source Code Verification (Post-Audit)

### Re-verification of Current Source Code
After reading actual daemon source files, **the ssasc.md audit reflects an older code state**. Current implementations have addressed most issues:

#### ✅ FIXED Issues
| Daemon | Issue | Current Status |
|--------|-------|----------------|
| **entropyd** | Blocking `std::fs` in async | **FIXED** — Uses `tokio::fs` throughout (lines 228, 233, 242, 244, 258, 261) |
| **logd** | Dead `ingest_daemon_message` | **FIXED** — `BridgeMessage::Forward` handled at lines 201-206, calls `ingest_daemon_message` |
| **healthd** | Thermal monitoring absent | **FIXED** — Reads `/sys/class/thermal` directly (lines 49-92) |
| **mountd** | ZFS parsing broken | **FIXED** — Checks "all pools are healthy" (line 513), parses actual state |
| **mountd** | `MountUnexpected` severity wrong | **FIXED** — Emits `AlertSeverity::Info` (line 891) per spec |
| **mountd** | `FilesystemReadOnly` severity wrong | **FIXED** — Emits `AlertSeverity::Critical` (line 872) per spec |
| **mountd** | Proot fallback order wrong | **FIXED** — Correct order: mountinfo → `/proc/mounts` → `mount` cmd (lines 445-461) |
| **kha-watchd** | Missing process state (Z/D) | **FIXED** — `KhaState` enum with parsing (lines 33-70, 116-146) |
| **kha-watchd** | Missing zombie reaping tracking | **FIXED** — Zombie streak counter (lines 359-379) |

#### ❌ REMAINING Issues
| Daemon | Issue | Location |
|--------|-------|----------|
| **mountd** | Disappeared mount emits `MountDegraded` + `Warning` instead of `MountMissing` + `Critical` | Lines 905-911 |
| **kha-watchd** | No signal forwarding stats (kha.md §34) | Not implemented |
| **kha-watchd** | No reaping delta counter (eos1.md line 22) | Not implemented |
| **kha-watchd** | No `zombies_reaped` in alert payload (eos1.md line 27) | Not implemented |

**Note**: True reaping delta requires Kha to expose counter via Ma'at — current zombie-streak is best external proxy.

---

## 2026-07-21 — Systems Spec: Stage 1.5 Decisions (Proposed)

### 1. Bootloader Selection
| Target | Decision | Rationale |
|--------|----------|-----------|
| Pinephone Pro (ARM64) | **U-Boot** | Only viable ARM mobile bootloader with mainline support, RK3399S init |
| Framework Laptop (x86_64) | **systemd-boot** | Simpler than GRUB, native EFI stub, aligns with freedom-first philosophy |
| Sokar (Reference) | U-Boot (ARM) / systemd-boot (x86) | Match target arch |

**No GRUB** — adds complexity Osiris doesn't need. Kha is the init; bootloader only loads kernel + initramfs + cmdline.

### 2. Libc Decision: **musl**
| Factor | musl | glibc |
|--------|------|-------|
| Static linking | ✅ Native | ❌ Problematic |
| Binary size | ~400KB static | ~2MB+ dynamic |
| Rust crate compatibility | Most work | Universal |
| Osiris philosophy | **Minimal, auditable, static-friendly** | Heavy, dynamic-only |

**Migration path**: Harvester harvests glibc bins from Debian proot for compat; Osiris-native builds target `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl`.

### 3. Core Userspace via Harvester
Harvester harvests from Debian Trixie proot: `coreutils`, `bash`, `ncurses`, `util-linux`, `findutils`, `grep`, `sed`, `gawk`, `tar`, `gzip`, `zstd`, `less`, `vim-tiny`.

Mechanism: `ldd` walk → recursive dpkg dep resolution → package as `.osr` → install to `/usr/lib/osiris/<pkg>/bin` → symlink farm at `/usr/bin/` managed by OPIUM alternatives.

### 4. Filesystem & Partition Scheme
| Decision | Value |
|----------|-------|
| Default FS | **ext4** (universal, no kernel module deps) |
| Boot partition | 512MB FAT32 (EFI) / 4MB raw (U-Boot) |
| Root (`/osiris`) | ext4, 8-16GB |
| Var (`/osiris/var`) | ext4, separate partition, 4-8GB (logs, entropy seed, pkg DB) |
| Home (`/osiris/home`) | ext4/f2fs, remaining space |
| Swap | **zram** (no dedicated partition) — Kha manages via `zram-generator` equivalent |

Standard directory layout documented for `mountd` fstab expectations:
```
/osiris          ← Osiris root (read-only, from .osr packages)
/osiris/bin      ← Symlinks to active package bins
/osiris/lib      ← Shared libs (musl + harvested glibc compat)
/osiris/etc      ← Config (mutable, bind-mounted from /etc)
/osiris/var      ← State (logs, entropy, pkg DB)
/osiris/run      ← Runtime (tmpfs)
/osiris/home     ← User data (separate partition)
/osiris/opt      ← Optional/large packages
```

### 5. Secure Boot & Kha Invocation
| Decision | Value |
|----------|-------|
| Secure Boot | **Explicitly rejected** — violates freedom-first philosophy; users own hardware keys or disable |
| Kha Invocation | **initramfs handoff** — `init=/sbin/kha` in kernel cmdline; initramfs mounts `/osiris` (rootfs), pivots, execs Kha as PID 1 |
| Kernel cmdline | `root=PARTUUID=... ro init=/sbin/kha osiris.rootfs=/dev/disk/by-partuuid/...` |

---

## 2026-07-21 — Cross-Domain Blast Radius

| Decision | Affects |
|----------|---------|
| **musl libc** | All workspace crates (build targets), Harvester harvest logic, OPIUM package format |
| **Bootloader** | Stage 5/6 hardware bring-up, Kha invocation docs |
| **FS/Partitioning** | `mountd` fstab expectations, Harvester install paths, Kha mount essentials |
| **Secure Boot rejection** | Documentation only — no code change |

### Required Consultations
> **Networks Spec** — Confirm boot-time socket availability: Kha mounts `/run` (tmpfs) before spawning Bridge, so `/tmp/osiris-bridge.sock` is available. Networks Spec owns socket protocol; Systems Spec owns mount timing.

> **Daemons Spec** — Confirm `mountd` fstab `x-osiris-alert-*` options align with partition scheme above.

---

## 2026-07-21 — Updated Next Steps (Priority Order)

| Priority | Task | Status |
|----------|------|--------|
| 🔴 **Critical** | Fix `mountd` disappeared mount alert: use `MountMissing` + `Critical` | **TODO** |
| 🟠 **High** | Add signal forwarding stats + reaping delta to `kha-watchd` (requires Kha Ma'at exposure design) | **TODO** |
| 🟡 **Medium** | Verify `cargo build --workspace` passes | **PENDING** |
| 🟢 **Low** | Stage 1.5 decisions approval → append to `docs/decisions/systems-spec-log.md` | **PROPOSED** |

---

## 2026-07-21 — Decision Log Entry (Proposed)

```
## 2026-07-21 — Stage 1.5 Systems Layer Decisions

Decision: 
1. Bootloader: U-Boot (ARM/Pinephone Pro/Sokar), systemd-boot (x86/Framework)
2. Libc: musl for Osiris-native builds; Harvester harvests glibc bins from Debian for compat
3. Core userspace: Harvester harvests coreutils/bash/ncurses/etc from Debian Trixie proot into .osr packages
4. Filesystem: ext4 default; partition scheme: boot/root/var/home (+ zram swap)
5. Secure Boot: Rejected; Kha invoked via initramfs handoff (init=/sbin/kha)

Rationale: Aligns with freedom-first philosophy, static binary deployment, minimal base, and hardware roadmap. ext4 avoids FS module deps on bare metal. musl enables single-file daemon deploys. Secure Boot rejected as user-hostile.

Affects: All workspace crates (build targets), Harvester/OPIUM (package format), mountd (fstab expectations), Kha (mount timing), Networks Spec (socket availability), Daemons Spec (alert options)

Status: proposed
```