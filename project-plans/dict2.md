Response to dict.md findings:

## Verification Result: CONFIRMED

Every finding in dict.md is accurate and verifiable against the current codebase. My independent review of the Harvester codebase confirms every specific claim.

## Verification Details

### 1. Harvest (harvest.rs) — CONFIRMED
**Claim:** `get_dpkg_deps` doesn't collect dependencies correctly; manifest writing bypasses `manifest.rs`
- **Verified:** `harvest.rs:164-177` `get_dpkg_deps()` calls `dpkg -L <name>` and filters for `.so` files — this lists *files in the package*, not *package dependencies*. Actual deps require `dpkg -s <pkg>` or `dpkg-query --showformat='${Depends}'`.
- **Verified:** `harvest.rs:73-80` writes manifest as raw string literal instead of using `Manifest::to_toml()` from `manifest.rs`. The `Manifest` struct with `PackageInfo.depends` and `FileList` exists but is unused.

### 2. Install (install.rs) — CONFIRMED
**Claim:** Tar-slip path traversal vulnerability exists
- **Verified:** `install.rs:51-61` uses `tar -xzf <path> -C <osiris_root>` without `--strip-components` or path validation. A malicious `.osr` with `../../../etc/passwd` in the archive escapes `osiris_root`. This is CWE-22 (Path Traversal).

### 3. Remove (remove.rs) — CONFIRMED
**Claim:** `remove()` only deletes DB record, not installed files
- **Verified:** `remove.rs:10-30` calls `fs::remove_file(&record_path)` and optionally `purge_config()`, but **never** removes installed files from `osiris_root` (binaries, libraries, share, etc.). The package files remain on disk; only the DB record is deleted.

### 4. Manifest (manifest.rs) — CONFIRMED
**Claim:** Structs exist cleanly but are unused by harvest.rs
- **Verified:** `manifest.rs` defines `Manifest`, `PackageInfo` (with `depends: Option<Vec<String>>`), `FileList` (bin/lib/share/etc), `PackageSource` enum — all well-designed. But `harvest.rs` bypasses them entirely with a hand-rolled TOML string.

---

## Severity Assessment Agreement

The dict.md assessment is correct: **percentage-complete framing systematically understates risk**.

| Issue | ph.md Framing | Actual Severity |
|-------|---------------|-----------------|
| Tar-slip in install.rs | "missing feature" | **CVE-class security vulnerability** (RCE via package install) |
| remove() deletes only DB record | "60% done, needs reverse deps" | **Data loss / broken invariant** — package files persist forever |
| get_dpkg_deps returns wrong data | "manifest needs deps" | **Silent correctness failure** — harvests incomplete dependency graphs |

The "70% complete" framing treats a filesystem-escape vulnerability and a no-op remove command as equivalent to "no progress bars." This is the analytical failure dict.md identifies.

---

## Agreement with Final Verdict

> **"not close to usable, and worse than ph.md implied. Two blocking issues before this should touch anything beyond a disposable test environment: the tar-slip path traversal in install.rs, and the fact that remove() is currently non-functional."**

**Fully agreed.** These two issues alone make Harvester unsafe for any non-throwaway use. The tar-slip allows package install to escape the rootfs; the non-functional remove means installed packages can never be cleanly removed — they accumulate forever.

---

## Additional Verification: OPIUM Impact

Since OPIUM delegates to Harvester (`opium/src/main.rs:36-49` calls `call_harvester`), OPIUM inherits both blocking issues. Building OPIUM dependency resolution on top of this Harvester is indeed "building on sand."

---

## Conclusion

dict.md findings are **accurate, well-sourced, and correctly prioritized**. No false citations found. The severity miscalibration diagnosis is correct: percentage-complete metrics are the wrong lens for security and correctness bugs.