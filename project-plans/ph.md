# ph.md — Phase 2 Plan: Harvester & OPIUM Completion

**Date:** 2026-07-17  
**Status:** Planning  
**Prerequisite:** SystemCore COMPLIANT (6/6 daemons)

---

## Phase 2 Goal

Complete **Harvester** (dpkg-equivalent) and **OPIUM** (apt-equivalent) to enable package management on Osiris.

---

## Current State Assessment

### Harvester — ~85% Complete

| Component | Status | Notes |
|-----------|--------|-------|
| **Config** (`config.rs`) | ✅ Complete | Multi-env path resolution, env detection |
| **Harvest** (`harvest.rs`) | ⚠️ ~80% | Core logic works; needs: `.osr` manifest v2, deps in manifest, sig verification |
| **Install** (`install.rs`) | ⚠️ ~70% | Extracts tarballs; needs: pre/post scripts, file conflict check, atomic install |
| **Remove** (`remove.rs`) | ⚠️ ~60% | Unregisters; needs: reverse deps check, config backup before purge |
| **Manifest** (`manifest.rs`) | ⚠️ ~50% | Basic structure; needs: depends array, file lists, checksums |
| **Config** (`config.rs`) | ✅ Complete | Multi-env paths, env detection |

**Gaps:**
1. **Manifest v2**: No `depends` array in package, no file checksums, no pre/post scripts
2. **Install**: No pre/post scripts, no file conflict detection, not atomic
3. **Remove**: No reverse dependency check, no config backup
4. **Harvest**: No signature verification, no reproducible builds
5. **Config**: No repo index format, no channel support

### OPIUM — ~40% Complete

| Component | Status | Notes |
|-----------|--------|-------|
| **CLI** (`main.rs`) | ✅ ~80% | Commands wired; delegates to Harvester |
| **Repo** (`repo.rs`) | ⚠️ ~30% | Basic search/info/update; needs: remote fetch, deps resolution |
| **DB** (`db.rs`) | ❌ ~10% | Stubs only; needs: dep resolution, install planning |
| **Manifest** (`manifest.rs`) | ✅ ~80% | Structure good; used by OPIUM |
| **Config** (`config.rs`) | ✅ Complete | Reuses Harvester config |

**Gaps:**
1. **DB**: No dependency resolution, no install planning, no transaction atomicity
2. **Repo**: No remote fetch, no dependency resolution, no channel support
3. **CLI**: No transaction rollback, no dry-run, no progress output

---

## Phase 2A: Harvester Completion (Week 1-2)

### Priority 1: Manifest v2 + File Integrity

**Files:** `harvester/src/manifest.rs`, `harvester/src/harvest.rs`, `harvester/src/install.rs`

**Tasks:**
1. **Manifest v2** (`manifest.rs`):
   - Add `depends: Vec<String>` to `PackageInfo`
   - Add `checksums: HashMap<String, String>` to `FileList` (blake3)
   - Add `scripts: Scripts` struct with `pre_install`, `post_install`, `pre_remove`, `post_remove`
   - Add `size: u64` (installed size), `maintainer`, `license`, `url`

2. **Harvest** (`harvest.rs`):
   - Compute blake3 checksum for every file in staging
   - Generate `Manifest` with `FileList` including checksums
   - Write `manifest.toml` into staging before tar
   - Optional: sign manifest with Ed25519 (Ed25519 key from env)

3. **Install** (`install.rs`):
   - Verify checksums before extract
   - Run `pre_install` script (if present) before extract
   - Run `post_install` script after extract
   - Atomic install: extract to temp, verify, then `rename` into place
   - File conflict detection: scan existing files, abort on conflict (unless `--force`)

### Priority 2: Remove + Reverse Deps

**Files:** `harvester/src/remove.rs`, `harvester/src/manifest.rs`

1. **Reverse dependency check** in `remove::remove()`:
   - Scan `pkg_db` for packages with `depends` containing target
   - Abort unless `--force` or `--cascade`

2. **Config backup** before purge:
   - Backup `etc/<pkg>/` to `pkg_cache/backup/<pkg>-<timestamp>.tar.gz`

### Priority 3: Harvest Robustness

1. **Reproducible builds**: Sort files deterministically in tar, set mtime=0
2. **Signature verification**: Optional Ed25519 verify on install (public key from repo)
2. **Channel support**: `--channel stable|testing|unstable` in config

---

## Phase 2B: OPIUM Core (Week 3-4)

### Priority 1: Dependency Resolution + Install Planning

**Files:** `opium/src/db.rs`, `opium/src/repo.rs`

**DB Module (`db.rs`) — New Implementation:**
```rust
pub struct InstallPlan {
    pub to_install: Vec<PackageSpec>,  // topological order
    pub to_remove: Vec<String>,
    pub to_upgrade: Vec<PackageSpec>,
    pub download_size: u64,
    pub disk_space: u64,
}

pub fn resolve_install(packages: &[String], cfg: &OsirisConfig) -> Result<InstallPlan, String>;
pub fn execute_plan(plan: InstallPlan, cfg: &OsirisConfig) -> Result<(), String>;
```

**Algorithm:**
1. Load local index (`pkg_index`) + remote index (if configured)
2. Build dependency graph from `depends` in manifests
3. Topological sort for install order
4. Detect conflicts (file overlaps, version conflicts)
2. Generate `InstallPlan` with ordered steps

### Priority 2: Remote Repo + Channel Support

**Files:** `opium/src/repo.rs`, `opium/src/config.rs`

1. **Remote index fetch** (`repo::update`):
   - HTTP GET `https://repo.osirisos.org/<channel>/index.toml`
   - Verify signature (Ed25519 pubkey in config)
   - Merge with local index

2. **Channel support** (`config.rs`):
   - Add `channel: stable|testing|unstable` to `OsirisConfig`
   - Default: `stable`
   - `opium update --channel testing`

### Priority 3: Transaction Atomicity + CLI Polish

**Files:** `opium/src/db.rs`, `opium/src/main.rs`

1. **Transaction log**: Write intent to `/osiris/var/lib/opium/transaction.log` before any action
2. **Rollback**: On failure, reverse completed steps (remove installed, restore removed)
3. **CLI**: Progress bars, dry-run (`--dry-run`), verbose (`-v`), quiet (`-q`)

---

## Phase 2C: Integration Testing (Week 5)

### Test Matrix

| Scenario | Harvester | OPIUM | Expected |
|----------|-----------|-------|----------|
| Harvest `vim` from Debian proot | ✅ | — | `.osr` in cache |
| Install harvested `.osr` | ✅ | ✅ | Files in `/osiris`, record in db |
| Install with deps | — | ✅ | Topological order |
| Remove with rev-dep check | ✅ | ✅ | Blocks if depended on |
| Purge removes config | ✅ | ✅ | `etc/pkg/` gone |
| Install with pre/post scripts | ✅ | ✅ | Scripts execute |
| Upgrade replaces files atomically | ✅ | ✅ | No partial state |
| Rollback on failure | — | ✅ | Clean state |
| Remote repo update | — | ✅ | Index updated |
| Dependency resolution | — | ✅ | Topo order |

---

## Definition of Done (Phase 2)

| Criterion | Status |
|-----------|--------|
| `harvester harvest <pkg>` produces signed `.osr` with manifest v2 | ❌ |
| `harvester install <pkg>` verifies checksums, runs scripts, atomic | ❌ |
| `harvester remove --purge` checks rev-deps, backs up config | ❌ |
| `opium install <pkg>` resolves deps, topological order | ❌ |
| `opium update` fetches remote index, verifies sig | ❌ |
| `opium install` rolls back on failure | ❌ |
| `opium install --dry-run` shows plan | ❌ |
| All tests pass in CI | ❌ |

---

## Next Steps

1. **Start with Harvester Manifest v2** — foundation for everything else
2. **Then Harvester install atomicity + scripts**
3. **Then Harvester remove with rev-deps**
4. **Then OPIUM db.rs (dep resolution)**
5. **Then OPIUM repo remote fetch**
6. **Then OPIUM transaction + CLI polish**

---

## Dependencies

- **blake3** for checksums (add to workspace deps)
- **ed25519-dalek** for signing (optional, can be feature-gated)
- **toml_edit** for manifest manipulation (if modifying existing)

---

## Notes

- Keep Harvester OPIUM-agnostic (no OPIUM imports in Harvester)
- OPIUM calls Harvester via CLI — maintain clear boundary
- All I/O in Harvester must use `tokio::fs` (already done)
- Manifest v2 must be backward compatible (optional fields)

---

## Decision Points

1. **Signature verification**: Required for stable channel? (Yes, but can be opt-in for testing)
2. **Channel config**: Global or per-repo? (Start global, per-repo later)
3. **Script sandbox**: Run scripts in isolated namespace? (Defer to Phase 3)

---