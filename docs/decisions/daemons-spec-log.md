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
Decision: Built entropyd (Assessor #5, SystemCore) — entropy pool management and RNG seeding daemon. Monitors /proc/sys/kernel/random/entropy_avail and poolsize, emits Warning alert at 1000 bits and Critical at 192 bits. Loads saved seed from /var/lib/osiris/entropy.seed on startup, saves seed periodically (every 5 min) for next boot. Feeds entropy to kernel via /dev/urandom.
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

## 2026-07-16 — Plan: Complete SystemCore with kha-watchd
Decision: Build kha-watchd (Assessor #1, SystemCore) — monitors Kha (PID 1) itself, system heartbeat. Final daemon in 6-daemon SystemCore set.
Rationale: Build order from daemons-spec.md is timed → entropyd → mountd → kha-watchd. Five of six SystemCore daemons complete.
Affects: kha-watchd (new crate)
Status: proposed
