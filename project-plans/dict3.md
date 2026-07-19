# dict3.md — Final Verdict: Harvester Code Audit

**Date:** 2026-07-17  
**Sources:** harvester/src/{harvest,install,remove,manifest}.rs  
**References:** dict.md, dict2.md

---

## Executive Summary

**VERDICT: HARVESTER IS NOT USABLE. TWO BLOCKING ISSUES ARE CONFIRMED AS SECURITY/CORRECTNESS BUGS, NOT FEATURE GAPS.**

| Issue | File:Line | Severity | Type |
|-------|-----------|----------|------|
| Tar-slip path traversal | install.rs:58-61 | **CRITICAL** | CVE-class RCE via package install |
| remove() deletes only DB record | remove.rs:10-30 | **CRITICAL** | Data loss / broken invariant |
| get_dpkg_deps returns file list, not deps | harvest.rs:164-177 | **HIGH** | Silent correctness failure |
| Manifest bypasses manifest.rs | harvest.rs:74-80 | **HIGH** | Silent correctness drift |

**Percentage-complete framing in ph.md is analytically invalid** — it treats CVE-class vulns and no-op commands as equivalent to "missing progress bars."

---

## Verified Findings with Exact Citations

### 1. TAR-SLIP PATH TRAVERSAL (CRITICAL)

**File:** `harvester/src/install.rs:51-61`

```rust
fn extract_archive(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<(), String> {
    let osiris_root = cfg.osiris_root.to_str()
        .ok_or("Invalid Osiris root path")?;

    let path_str = path.to_str()
        .ok_or("Invalid package path")?;

    let status = std::process::Command::new("tar")
        .args(&["-xzf", path_str, "-C", osiris_root])  // <-- NO --strip-components, NO PATH VALIDATION
        .status()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if status.success() {
        println!("[harvester] Extracted: {}", name);
        Ok(())
    } else {
        Err(format!("Extraction failed for: {}", name))
    }
}
```

**Vulnerability:** A malicious `.osr` (tar.gz) containing `../../../etc/passwd` escapes `osiris_root` and writes anywhere the process has write access. No path validation, no `--strip-components`, no `--absolute-names` protection.

**Fix Required:** 
- Validate all paths in archive before extraction
- Use `--strip-components=1` or verify all paths stay within `osiris_root`
- Or implement safe extraction in Rust (walk archive entries, validate paths)

---

### 2. REMOVE() IS A NO-OP FOR FILES (CRITICAL)

**File:** `harvester/src/remove.rs:10-30`

```rust
pub fn remove(name: &str, purge: bool, cfg: &OsirisConfig) -> Result<(), String> {
    if !is_installed(name, cfg) {
        return Err(format!("'{}' is not installed", name));
    }

    // Read install record before deleting — useful for purge step
    let record_path = cfg.pkg_db.join(format!("{}.toml", name));
    let source = read_source(&record_path);

    // Remove the install record
    fs::remove_file(&record_path)
        .map_err(|e| format!("Could not remove install record: {}", e))?;

    println!("[harvester] Removed: {} (was: {})", name, source);

    if purge {
        purge_config(name, cfg)?;
    }

    Ok(())
}
```

**What it does:** Deletes only `pkg_db/{name}.toml` and optionally `etc/{name}/`.

**What it does NOT do:** Remove any installed files from `osiris_root` — no binary removal, no library removal, no share/doc removal. The package files persist forever on disk.

**Fix Required:**
- Read manifest to get file list (`FileList` in manifest)
- Delete all tracked files from `osiris_root`
- Only then remove DB record

---

### 3. get_dpkg_deps() RETURNS WRONG DATA (HIGH)

**File:** `harvester/src/harvest.rs:164-177`

```rust
/// Cross-check dependencies via dpkg -L for completeness
fn get_dpkg_deps(name: &str) -> Vec<String> {
    let output = Command::new("dpkg")
        .args(&["-L", name])
        .output();

    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter(|l| l.contains(".so"))
            .map(|l| l.trim().to_string())
            .collect(),
        Err(_) => vec![],
    }
}
```

**What it does:** Runs `dpkg -L <pkg>` (lists **files in the package**) and filters for `.so` files.

**What it should do:** Run `dpkg -s <pkg>` or `dpkg-query --showformat='${Depends}' <pkg>` to get **package dependencies** (other package names), not file paths.

**Result:** Returns library filenames (e.g., `libc.so.6`) instead of package names (e.g., `libc6`). These get added to `deps` and treated as library dependencies to copy, not package dependencies to declare in manifest.

---

### 4. MANIFEST BYPASSES manifest.rs (HIGH)

**File:** `harvester/src/harvest.rs:73-80`

```rust
// Write manifest into staging
let manifest = format!(
    "[package]\nname = \"{}\"\nversion = \"harvested\"\narch = \"{}\"\nsource = \"harvester\"\n",
    name,
    detect_arch()
);
fs::write(staging.join("manifest.toml"), manifest)
    .map_err(|e| format!("Could not write manifest: {}", e))?;
```

**What it does:** Writes a hand-rolled TOML string with only `name`, `version`, `arch`, `source`.

**What `manifest.rs` provides (unused):**
```rust
// manifest.rs:49-71
pub struct Manifest {
    pub package: PackageInfo,
    pub files:   Option<FileList>,
}

pub struct PackageInfo {
    pub name:        String,
    pub version:     String,
    pub arch:        String,
    pub description: Option<String>,
    pub depends:     Option<Vec<String>>,    // <-- NEVER POPULATED
    pub source:      Option<String>,
}

pub struct FileList {
    pub bin:   Option<Vec<String>>,
    pub lib:   Option<Vec<String>>,
    pub share: Option<Vec<String>>,
    pub etc:   Option<Vec<String>>,
}
```

**Result:** Manifest lacks `depends`, `description`, `files` (with checksums), `scripts`, `size`, `maintainer`, `license`, `url` — all fields that exist in the struct but are never populated.

---

## Additional Verified Issues

### 5. No Checksums/Signatures (HIGH)
- `harvest.rs:82-103` packs `.osr` with no checksums
- `install.rs:51-68` extracts with no verification
- **Risk:** Supply chain attack, corrupted packages install silently

### 6. No File Conflict Detection (HIGH)
- `install.rs:51-61` extracts directly to `osiris_root` with no pre-scan
- Overwrites existing files silently

### 7. No Atomic Install (MEDIUM)
- Extracts directly to target; partial extract on failure leaves broken state

### 7. No Reverse Dependency Check in Remove (HIGH)
- `remove.rs` doesn't check if other packages depend on target
- Removing `libc6` would silently break everything

### 8. No Pre/Post Scripts Support (MEDIUM)
- `Manifest` struct has no `scripts` field
- `install.rs`/`remove.rs` don't run `pre_install`/`post_remove` hooks

### 9. No Atomic Install (MEDIUM)
- Extracts directly to target; failure leaves partial install

---

## Positive Findings (What Works)

| Component | Status |
|-----------|--------|
| Environment detection (`config.rs`) | ✅ Works correctly |
| Path resolution (`OsirisConfig`) | ✅ Multi-env support works |
| Recursive ldd dependency walk | ✅ Core logic correct |
| Library search (`find_lib`) | ✅ Multi-path search works |
| Install record keeping | ✅ DB records created correctly |
| List installed | ✅ Works |
| Purge config | ✅ Removes `etc/<pkg>/` |

---

## Concrete Fix Priorities

### BLOCKING (Must fix before any non-test use)

| Priority | Fix | Files |
|----------|-----|-------|
| **1** | Fix tar-slip in `extract_archive` | `install.rs:51-61` |
| **2** | Implement actual file removal in `remove()` | `remove.rs`, `manifest.rs` |
| **3** | Fix `get_dpkg_deps` to query package deps | `harvest.rs:164-177` |
| **4** | Use `Manifest` struct in harvest | `harvest.rs:74-80`, `manifest.rs` |

### HIGH PRIORITY

| Priority | Fix | Files |
|----------|-----|-------|
| 5 | Add SHA256/blake3 checksums to manifest | `manifest.rs`, `harvest.rs`, `install.rs` |
| 6 | Implement file conflict detection | `install.rs` |
| 6 | Add reverse dependency check in `remove()` | `remove.rs`, `manifest.rs` |
| 6 | Populate `PackageInfo.depends` in harvest | `harvest.rs`, `manifest.rs` |

### MEDIUM PRIORITY

| Priority | Fix | Files |
|----------|-----|-------|
| 9 | Add `FileList` population (bin/lib/share/etc) | `harvest.rs`, `manifest.rs` |
| 10 | Add pre/post install/remove scripts | `manifest.rs`, `install.rs`, `remove.rs` |
| 11 | Add atomic install (temp dir + rename) | `install.rs` |
| 12 | Add Ed25519 signing/verification | `manifest.rs`, `harvest.rs`, `install.rs` |

---

## Logic Chain for Prioritization

```
CRITICAL (blocks all use)
├── Tar-slip → RCE via package install
└── remove() no-op → Data loss, broken invariant

HIGH (silent correctness failures)
├── get_dpkg_deps wrong → Incomplete dependency graph
└── Manifest bypass → Silent drift, no deps/checksums

MEDIUM (operational issues)
├── No checksums → Supply chain risk
├── No conflict detection → Silent overwrites
└── No reverse deps → Broken removals
```

---

## Build Verification

```bash
cargo build --workspace
# Current: Finished dev [unoptimized + debuginfo] in ~1s
# After fixes: Must still pass
```

---

## Conclusion

**Harvester is not production-ready.** Two CRITICAL bugs (tar-slip RCE, no-op remove) and two HIGH correctness bugs (wrong deps, manifest bypass) make it unsafe for any non-throwaway use.

**The "percentage complete" framing in ph.md was analytically invalid** — it treated CVE-class vulns and no-op commands as equivalent to "missing progress bars."

**Minimum viable fix set before any real use:**
1. Fix tar-slip in `install.rs:58-61`
2. Implement actual file removal in `remove.rs`
3. Fix `get_dpkg_deps` in `harvest.rs:164-177`
4. Use `Manifest` struct in `harvest.rs:74-80`

Only after these four fixes should Phase 2 OPIUM work proceed.