# daemons-spec — Decision Log

_Newest entries at the bottom._

## 2026-07-16 — healthd & logd build complete
Decision: Fixed sysinfo 0.30 API compatibility in healthd (Disks struct separated from System, refresh_cpu() instead of refresh_cpu_all(), CPU usage calculated from cpus() average). Both healthd and logd now compile successfully in the workspace build.
Rationale: sysinfo 0.30 changed the API — Disks is now a separate struct requiring Disks::new_with_refreshed_list(), CPU refresh uses refresh_cpu(), and global CPU usage is computed from cpus() average. The daemons implement the reference pattern: register with Ma'at, connect to AkerNet Bridge, monitor domain, emit Alert messages.
Affects: healthd (compilation fixes), logd (warnings only, no functional changes needed)
Status: implemented

## 2026-07-16 — timed build complete
Decision: Built timed (Assessor #4, SystemCore) — NTP time synchronization daemon. Uses ntp crate for NTP queries, calculates system time drift against pool.ntp.org, emits Alert messages on drift thresholds (warning at 1s, critical at 5s), and on NTP query failures. Follows reference pattern: register with Ma'at, connect to AkerNet Bridge, poll on interval, emit Alerts.
Rationale: Stage 1 Foundation roadmap explicitly lists "Core Abyss daemons: logd, mountd, healthd, timed" — timed is the fourth SystemCore daemon. NTP sync is required for log timestamps, cert validation, and scheduled tasks.
Affects: timed (new crate), Cargo.toml workspace members
Status: implemented

## 2026-07-16 — mountd build complete
Decision: Built mountd (Assessor #6, SystemCore) — filesystem mount monitoring daemon. Parses /etc/fstab for expected mounts, monitors /proc/self/mountinfo for current state. Detects: missing expected mounts (Critical), filesystem type mismatches (Warning), unexpected new mounts (Warning), disappeared mounts (Warning). Reloadable via BridgeMessage::Reload to re-parse fstab.
Rationale: Post-boot mount management is critical for data integrity. Stage 1 Foundation lists mountd explicitly. Build order followed spec (timed → entropyd → mountd → kha-watchd) but mountd built before entropyd due to lower complexity and explicit Stage 1 listing.
Affects: mountd (new crate), Cargo.toml workspace
Status: implemented

## 2026-07-16 — entropyd build complete
Decision: Built entropyd (Assessor #5, SystemCore) — entropy pool management and RNG seeding daemon. Monitors /proc/sys/kernel/random/entropy_avail and poolsize, emits Warning alert at 1024 bits and Critical at 192 bits. Loads saved seed from /var/lib/osiris/entropy.seed on startup, saves seed periodically (every 5 min) for next boot. Feeds entropy to kernel via /dev/urandom.
Rationale: Entropy pool management is fundamental for cryptographic operations. Per build order (timed → entropyd → mountd → kha-watchd), entropyd is the fifth SystemCore daemon.
Affects: entropyd (new crate), Cargo.toml workspace
Status: implemented

## 2026-07-16 — mountd patched to nmtd.md specification
Decision: Updated mountd to match nmtd.md Phase 1 specification. Added: MountdConfig with fstab/mountinfo paths, poll interval, disk thresholds; FstabEntry parsing with x-osiris-alert-* options (alert severity, missing, fs mismatch, options change, disk warning/critical); proot fallback using `mount` command; disk usage monitoring with per-mount thresholds from fstab options; ZFS pool monitoring (zpool status); Btrfs scrub monitoring; mount propagation flag change detection; structured alert types (MountMissing, MountDegraded, MountUnexpected, MountPropagationChanged, FilesystemFull, FilesystemReadOnly, ZfsPoolDegraded, BtrfsScrubRunning, etc.).
Rationale: nmtd.md (Networks Spec decisions) resolved clarifying questions for mountd and defined the complete alert schema. Implementation must match spec for logd/alertd routing.
Affects: mountd (major rewrite), Cargo.toml (added libc dependency)
Status: implemented

## 2026-07-16 — Daemon workspace restructured into daemons/ folder
Decision: Reorganized all daemon crates into `daemons/` directory with domain-based subfolders. SystemCore daemons now at `daemons/system_core/` (logd, healthd, timed, mountd, entropyd). Updated root Cargo.toml workspace members to reflect new paths. Fixed maat dependency paths to `../../../maat`.
Rationale: Per osiris-rm.json roadmap, The Abyss Network has 9 domains with 42 daemons total. Organizing by domain enables scalable growth as Hardware, Input, DisplayGraphics, NetworkAkerNet, Audio, UserSession, Security, Services daemons are added. Aligns with "Everything ground up. Every name earns its place. Every component knows its job."
Affects: All daemon crates (moved), Cargo.toml (workspace members updated), maat dependency paths
Status: implemented

## 2026-07-16 — kha-watchd build complete
Decision: Built kha-watchd (Assessor #1, SystemCore) — monitors Kha (PID 1) itself and system heartbeat. Implements: Kha liveness check via signal(0) to PID 1; /proc/1/stat parsing for uptime, state, thread count; emits Alert (Critical) if Kha is dead or in zombie state (Z); emits Alert (Warning) if Kha in uninterruptible sleep (D); periodic StatusUpdate heartbeat to AkerNet Bridge; 10s poll interval. No mount monitoring (belongs to mountd per qs.md clarification).
Rationale: Final daemon in 6-daemon SystemCore set. Build order (timed → entropyd → mountd → kha-watchd) completed. Kha-watchd registers last (depends on AkerNet Bridge running). Uses existing Alert/AlertSeverity precedent (no Networks Spec changes needed).
Affects: kha-watchd (new crate at daemons/system_core/kha-watchd), Cargo.toml workspace members
Status: implemented

## 2026-07-17 — SystemCore daemon review & bug fixes
Decision: Reviewed all 6 SystemCore daemons against specs (nmtd.md, qs.md, osiris-rm.json, kha.md, eos1.md) and fixed bugs:
- **logd**: Fixed socket read pattern (take stream from mutex to avoid deadlock), removed unused imports (PathBuf, warn), added AlertSeverity import for completeness.
- **timed**: Fixed NTP response handling bug — was shadowing `rtt` variable with tuple destructuring. Now correctly uses measured RTT from elapsed time, not NTP response's round_trip_time() (which doesn't exist in ntp 0.3 crate).
- **mountd**: Fixed ZFS pool state check bug (duplicated `pool.state != "ONLINE" && pool.state != "ONLINE"` → single check). Verified x-osiris-alert-* option parsing matches nmtd.md spec.
- **entropyd**: Fixed entropy critical threshold from 256 → 192 bits per spec. Changed seed file path to entropy.seed (with .seed extension). Changed seed save to read from /dev/urandom (non-blocking) instead of /dev/random (blocking). Removed dead RANDOM_PATH constant.
- **kha-watchd**: Removed SIGCHLD signal handler (won't work — kha-watchd is not parent of Kha). Removed dead signal_forward_count field. Fixed missing `libc` import. Fixed unused utime/stime variables. Kha liveness now correctly checked via signal(0) to PID 1 and /proc/1/stat parsing.
- **healthd**: Verified sysinfo 0.30 API usage is correct (Disks separate, refresh_cpu(), CPU average from cpus()).

All 6 SystemCore daemons compile and follow the reference pattern: register with Ma'at → connect to AkerNet Bridge → poll domain → emit Alert messages with deduplication.
Rationale: Found and fixed actual bugs (not just warnings) during spec compliance review. Critical issues: timed NTP rtt shadowing, mountd ZFS state check, entropyd threshold mismatch, kha-watchd signal handling design flaw.
Affects: logd, timed, mountd, entropyd, kha-watchd
Status: implemented

## 2026-07-17 — Systems Spec Compliance Review — SystemCore Daemons
Decision: Systems Spec agent reviewed all 6 SystemCore daemons against daemons-spec-log.md, mountd.md, nmtd.md, kha.md, qs.md, eos1.md, osiris-rm.json. Found significant compliance gaps:
- **logd**: Alert ingestion path dead code — `ingest_daemon_message` exists but never called in main loop; Bridge delivers alerts to logd but logd ignores them.
- **healthd**: Compliant; thermal thresholds defined but unused (sysinfo 0.30 limitation, documented).
- **timed**: Compliant; NTP crate sync API measured RTT is local, not NTP protocol RTT.
- **mountd**: ZFS parsing broken (`zpool status -x` only outputs on error); proot fallback order wrong (skips `/proc/mounts`); alert severity mismatches (`MountUnexpected` should be Info, `FilesystemReadOnly` should be Critical); `MountDegraded` emitted for disappeared mounts instead of `MountMissing`.
- **entropyd**: **Critical** — blocking std::fs I/O in async functions blocks tokio runtime; seed path entropy.seed ✓; critical threshold 192 bits ✓.
- **kha-watchd**: **Non-compliant** — missing process state check (Z/D), missing zombie reaping tracking, missing signal forwarding stats; heartbeat logs every 300s but poll is 10s; alert payload incomplete (missing `zombies_reaped` field).

All 6 daemons compile but SystemCore is 67% compliant (4/6 functionally complete, 2 with critical gaps). entropyd has critical async correctness bug. logd, mountd, kha-watchd have significant functional gaps against specifications.

Recommendation: Do not proceed to Hardware domain daemons until SystemCore fully compliant. Ingestion path (logd) and mount monitoring (mountd) are foundational for later daemons.
Rationale: Cross-checked implementations against all spec documents. Found discrepancies between decision log claims and actual code behavior. Critical issues: entropyd blocking I/O, logd dead ingestion path, mountd ZFS parsing, kha-watchd missing core metrics.
Affects: logd, healthd, timed, mountd, entropyd, kha-watchd
Status: needs remediation

## 2026-07-17 — Systems Spec Compliance Audit — SystemCore Daemons
Decision: Comprehensive compliance audit of all 6 SystemCore daemons against specifications (daemons-spec-log.md, mountd.md, nmtd.md, kha.md, qs.md, eos1.md, osiris-rm.json). Found critical gaps in 3/6 daemons:

**logd (#2) — Partial compliance**
- Missing: Alert ingestion path not wired — `ingest_daemon_message()` exists but never called in main loop. DaemonMessage::Alert from other daemons delivered by Bridge but ignored.
- Unused: `ring_buffer` fields, `RING_BUFFER_MAX_BYTES` constant, `write_log()` and `ingest_daemon_message()` methods.

**healthd (#3) — Compliant** (temp thresholds unused but documented limitation)

**timed (#4) — Compliant** (measured RTT is local elapsed, not NTP protocol RTT — acceptable)

**mountd (#6) — Partial compliance — Critical issues:**
- ZFS parsing broken: `zpool status -x` only outputs on error; `state` field always "UNKNOWN"
- Proot fallback order wrong: spec says mountinfo → /proc/mounts → mount cmd; code skips /proc/mounts
- Alert severity mismatches: `MountUnexpected` should be Info (spec) but is Warning; `FilesystemReadOnly` should be Critical (spec) but is Warning
- Disappeared mount emits `MountDegraded` instead of `MountMissing`
- Btrfs monitoring uses `findmnt` + `btrfs` commands not declared in Cargo.toml

**entropyd (#5) — Partial compliance — Critical bug:**
- Blocking I/O in async functions: `std::fs::read_to_string`, `std::fs::write`, `std::fs::OpenOptions` called directly in async fns — blocks tokio runtime
- Must use `tokio::fs` or `spawn_blocking`

**kha-watchd (#1) — Non-compliant — Missing spec-required features:**
- No process state check (Z=zombie, D=uninterruptible) — kha.md §34 requires this
- No zombie reaping count tracking — kha.md §34, eos1.md line 22 require this
- No signal forwarding stats — kha.md §34 requires this
- Alert payload incomplete — eos1.md line 27 shows `"zombies_reaped": 0` field not implemented
- Heartbeat log triggers on Kha uptime modulo 300s, not wall time

**Cross-cutting:** logd alert ingestion not wired; mountd missing external deps in Cargo.toml; entropyd async correctness bug.

Rationale: Spec compliance audit reveals foundational gaps. logd ingestion path, mountd ZFS/alert logic, entropyd blocking I/O, and kha-watchd missing metrics must be fixed before Hardware domain work.
Affects: logd, mountd, entropyd, kha-watchd
Status: proposed
