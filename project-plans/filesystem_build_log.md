# Systems Spec — Filesystem Build Log

## 2026-07-21 — Osiris OS Filesystem Hierarchy & Configuration Complete

### Summary
Created complete filesystem structure, fstab, master config, and Kha boot integration for Osiris OS targeting Termux/proot (aarch64) and bare metal (Pinephone Pro, Framework).

### Files Created

```
osiris-fs/
├── bin/
├── lib/
├── etc/
│   ├── fstab
│   └── osiris/
│       └── osiris.conf
├── var/
│   ├── log/
│   ├── lib/osiris/
│   ├── lib/harvester/
│   ├── lib/opium/
│   ├── cache/osiris/
│   └── spool/
├── run/
│   ├── lock/
│   └── osiris/
├── home/
├── opt/
├── FILESYSTEM_HIERARCHY.md
```

### Key Configuration Files

#### 1. `/etc/fstab` — Mountd-Monitored Filesystems
- 5 physical partitions: boot (vfat), root (ext4, ro), var (ext4), home (ext4), opt (ext4)
- Virtual: proc, sysfs, devtmpfs, tmpfs (/run), tmpfs (/osiris/run), securityfs
- **x-osiris-alert-*** options parsed by mountd:
  - `x-osiris-alert=critical|warning|info` — base severity
  - `x-osiris-alert-missing=1` — alert if mount absent
  - `x-osiris-alert-fsmismatch=1` — alert on FS type mismatch
  - `x-osiris-disk-warning=85` — disk usage warning %
  - `x-osiris-disk-critical=95` — disk usage critical %

#### 2. `/etc/osiris/osiris.conf` — Master Daemon Config
Sections: [system], [paths], [daemons], [network], [entropy], [time], [health], [mount]
All 12 daemons (SystemCore 6 + NetworkAkernet 6) load order defined.

#### 3. `/osiris/bin/mount-essentials` — Kha Boot Script
Mounts virtual FS, Osiris root (ro), bind-mounts /run→/osiris/run, var/home/opt partitions.
Called by Kha before spawning AkerNet Bridge.

### Partition Scheme (Bare Metal)
| Partition | FS | Size | Mount |
|-----------|-----|------|-------|
| BOOT | vfat | 512M | /boot |
| OSIRIS_ROOT | ext4 | 8-16G | /osiris (ro) |
| OSIRIS_VAR | ext4 | 4-8G | /osiris/var |
| OSIRIS_HOME | ext4/f2fs | rest | /osiris/home |
| OSIRIS_OPT | ext4 | optional | /osiris/opt |
| swap | zram | — | (no partition) |

### Daemon Config Consumption Mapping
| Daemon | Reads | Writes | Alerts |
|--------|-------|--------|--------|
| logd | log_root, ring_log, bootstrap_log | Ring buffer, .ring, bootstrap.log | — |
| healthd | health.* thresholds | — | CPU, mem, disk, thermal |
| timed | time.* NTP | — | Drift, NTP fail, sync stale |
| mountd | mount.*, /etc/fstab | Known mounts, propagation | MountMissing, Degraded, Unexpected, FSFull, FSReadOnly, ZFS/Btrfs |
| entropyd | entropy.*, entropy_seed | entropy.seed | Entropy warn/crit |
| kha-watchd | — | — | Kha liveness, state, zombies |
| netd | network.interfaces | Interface state | Carrier loss, IP change |
| wifid | — | — | Disconnect, auth fail, roam |
| dnsd | network.adguard_home_url | — | Upstream fail, cache stats |
| vpnd | network.kill_switch_default | — | Handshake fail, tunnel drop, killswitch |
| firewalld | network.default_policy | Rule counters | Policy violation, rule mismatch |
| proxyd | network.proxy_chains, rotation | — | Proxy fail, circuit break |

### Termux/proot Adaptations
- /proc, /sys, /dev pre-mounted by Android host
- /run symlink to Termux var/run
- PARTUUID mounts unavailable — fstab entries ignored gracefully
- /osiris bind-mounted from proot Debian root
- Daemon binaries run from target/debug/ via OSIRIS_DAEMON_BIN_DIR

### Cross-Domain Integration Points
| This Work | Affects |
|-----------|---------|
| /etc/fstab x-osiris-alert-* | Daemons Spec → mountd alert logic |
| /etc/osiris/osiris.conf paths | All Daemons → config loading |
| Partition scheme | Systems Spec → bootloader, initramfs |
| /osiris/bin/mount-essentials | Kha → pre-Bridge spawn |
| zram swap | Kha → boot-time zram init |

### Build Verification
```bash
$ cargo build --workspace
# -> Finished `dev` profile [unoptimized + debuginfo] target(s) in 19.14s
```
All 12 daemons compile cleanly.

### Next Steps
1. Kha: integrate mount-essentials before spawn_bridge()
2. Harvester/OPIUM: package as base .osr packages
3. mountd: verify fstab parsing matches x-osiris-alert-* options
4. All daemons: load config from /etc/osiris/osiris.conf (currently hardcoded)
5. Bootloader: generate U-Boot/systemd-boot with kernel cmdline PARTUUIDs

---

**Status**: Filesystem hierarchy and configuration complete. Ready for Kha boot integration and Harvester packaging.

---

### 2026-07-21 — Clarification: `/usr` Directory Design

**Question** (from Sitratis): *"I noticed no mention of 'usr' directory in the file system. Can you explain?"*

**Answer**: `/usr` exists as a **single symlink** in the hierarchy:

```
/usr    -> /osiris
```

**Rationale** (from `FILESYSTEM_HIERARCHY.md` lines 15-19):
- Traditional FHS splits `/bin` vs `/usr/bin`, `/lib` vs `/usr/lib`
- Osiris unifies: all packages install to `/osiris` (read-only)
- Compatibility symlinks: `/bin->/osiris/bin`, `/sbin->/osiris/bin`, `/lib->/osiris/lib`, `/lib64->/osiris/lib`, `/usr->/osiris`
- No `/usr/local` — all software via OPIUM/Harvester `.osr` packages

**Tradeoffs**:
| Pros | Cons |
|------|------|
| Single package prefix (`/osiris`) | Not strict FHS compliant |
| Read-only root enforceable | Third-party binaries may expect `/usr/lib/x86_64-linux-gnu/...` |
| Atomic package transactions | Mitigated by `/lib` symlink + `ld.so.conf` |

**Decision**: Keep single `/usr -> /osiris` symlink. Add subdirectory symlinks (`/usr/bin`, `/usr/lib`, etc.) only if third-party binary compatibility demands it.
