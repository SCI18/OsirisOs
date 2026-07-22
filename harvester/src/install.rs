// Harvester — install.rs
// Low level package installation.
// No dependency resolution — that is OPIUM's job.

use std::fs;
use std::path::Path;
use crate::config::OsirisConfig;
use crate::manifest::Manifest;

pub fn validate_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Package name cannot be empty".to_string());
    }
    if name.len() > 128 {
        return Err("Package name too long".to_string());
    }
    let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !valid || name.starts_with('.') || name.contains("..") {
        return Err(format!("Invalid package name: '{}' (must be alphanumeric, -, _, . only, no leading dot or '..')", name));
    }
    Ok(())
}

pub fn install(name: &str, force: bool, cfg: &OsirisConfig) -> Result<(), String> {
    validate_package_name(name)?;

    if is_installed(name, cfg) {
        println!("[harvester] {} is already installed", name);
        return Ok(());
    }

    let osr_path = cfg.pkg_cache.join(format!("{}.osr", name));
    let tar_path = cfg.pkg_cache.join(format!("{}.tar.gz", name));

    if osr_path.exists() {
        install_archive(name, &osr_path, "osiris", force, cfg)
    } else if tar_path.exists() {
        install_archive(name, &tar_path, "harvester", force, cfg)
    } else {
        Err(format!(
            "'{}' not found in cache ({}). Run: harvester harvest {}",
            name,
            cfg.pkg_cache.display(),
            name
        ))
    }
}

/// Reads manifest.toml out of an archive without extracting anything else
/// — used before any extraction/validation decision is made.
fn read_manifest_from_archive(path: &Path) -> Result<Manifest, String> {
    let path_str = path.to_str().ok_or("Invalid package path")?;
    let output = std::process::Command::new("tar")
        .args(&["-xzf", path_str, "-O", "manifest.toml"])
        .output()
        .map_err(|e| format!("Failed to read manifest from archive: {}", e))?;

    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!("Archive '{}' has no readable manifest.toml", path.display()));
    }

    let content = String::from_utf8_lossy(&output.stdout);
    Manifest::from_toml(&content)
}

fn is_unsafe_archive_entry(entry: &str) -> bool {
    if entry.starts_with('/') {
        return true;
    }
    entry.split('/').any(|component| component == "..")
}

fn list_archive_entries(path: &Path) -> Result<Vec<String>, String> {
    let path_str = path.to_str().ok_or("Invalid package path")?;
    let list_output = std::process::Command::new("tar")
        .args(&["-tzf", path_str])
        .output()
        .map_err(|e| format!("Failed to list archive contents: {}", e))?;

    if !list_output.status.success() {
        return Err("Failed to list archive contents".to_string());
    }

    let listing = String::from_utf8_lossy(&list_output.stdout);
    let mut entries = Vec::new();

    for line in listing.lines() {
        let entry = line.trim();
        if entry.is_empty() || entry == "." || entry.ends_with('/') {
            continue;
        }
        if is_unsafe_archive_entry(entry) {
            return Err(format!("Unsafe archive path entry: '{}'", entry));
        }
        entries.push(entry.to_string());
    }

    Ok(entries)
}

/// Runs a script's literal content via `sh -c`, writing it to a temp file
/// first so multi-line scripts work correctly and exit codes are real
/// process exit codes rather than shell -c string-escaping artifacts.
///
/// SECURITY NOTE: no sandboxing. See manifest.rs's Scripts doc comment.
pub(crate) fn run_script(label: &str, script: &str, cfg: &OsirisConfig) -> Result<(), String> {
    let script_path = std::env::temp_dir().join(format!("osiris-script-{}.sh", std::process::id()));
    fs::write(&script_path, script)
        .map_err(|e| format!("Could not write {} script: {}", label, e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&script_path, perms).map_err(|e| e.to_string())?;
    }

    println!("[harvester] Running {} script...", label);
    let status = std::process::Command::new("sh")
        .arg(&script_path)
        .env("OSIRIS_ROOT", &cfg.osiris_root)
        .status()
        .map_err(|e| format!("Failed to execute {} script: {}", label, e));

    let _ = fs::remove_file(&script_path);

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("{} script exited with status {}", label, s)),
        Err(e) => Err(e),
    }
}

/// FIX (checksums + atomic install + conflict detection, single pass):
/// 1. Read manifest first (no extraction yet).
/// 2. Validate every archive entry path is safe (tar-slip protection).
/// 3. Check for file conflicts against what's already on disk, unless
///    `force` is set.
/// 4. Extract to a temp staging directory — nothing touches osiris_root
///    yet.
/// 5. Verify each file's checksum (if the manifest recorded one) against
///    what was actually extracted, before trusting any of it.
/// 6. Run pre_install script (still against osiris_root — scripts may
///    need to reference the live system, e.g. checking for a running
///    service).
/// 7. Move each verified file from staging into osiris_root. Individual
///    fs::rename calls are atomic on the same filesystem; this is not a
///    single whole-package atomic transaction (a hard failure partway
///    through move-phase can still leave a partial install) — a real
///    limitation, documented rather than silently accepted as "atomic".
/// 8. Run post_install script.
/// 9. Record install (file list + manifest copy for future reverse-dep/
///    script lookups).
fn install_archive(name: &str, path: &Path, source_label: &str, force: bool, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Installing: {}", path.display());

    let manifest = read_manifest_from_archive(path)?;
    let entries = list_archive_entries(path)?;

    if entries.is_empty() {
        return Err(format!("Archive for '{}' contains no extractable files", name));
    }

    // Conflict detection: check real target paths against what's already there.
    if !force {
        let mut conflicts = Vec::new();
        for entry in &entries {
            let target = cfg.osiris_root.join(entry);
            if target.exists() {
                conflicts.push(entry.clone());
            }
        }
        if !conflicts.is_empty() {
            return Err(format!(
                "Refusing to install '{}': {} file(s) already exist and would be overwritten ({}). Use --force to override.",
                name, conflicts.len(), conflicts.join(", ")
            ));
        }
    }

    // Extract to temp staging — osiris_root is untouched until verified.
    let staging = std::env::temp_dir().join(format!("harvester-install-{}", name));
    let _ = fs::remove_dir_all(&staging); // clean any stale prior attempt
    fs::create_dir_all(&staging).map_err(|e| e.to_string())?;

    let path_str = path.to_str().ok_or("Invalid package path")?;
    let staging_str = staging.to_str().ok_or("Invalid staging path")?;

    let status = std::process::Command::new("tar")
        .args(&["-xzf", path_str, "-C", staging_str, "--no-same-owner"])
        .status()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if !status.success() {
        let _ = fs::remove_dir_all(&staging);
        return Err(format!("Extraction failed for: {}", name));
    }

    // Checksum verification against the manifest, if it recorded any.
    let file_entries = manifest.all_file_entries();
    let mut unverified_count = 0;
    for entry in &file_entries {
        let staged_path = staging.join(&entry.path);
        match &entry.checksum {
            Some(expected) => {
                match crate::manifest::compute_checksum(&staged_path) {
                    Ok(actual) if &actual == expected => {}
                    Ok(actual) => {
                        let _ = fs::remove_dir_all(&staging);
                        return Err(format!(
                            "Checksum mismatch for '{}': expected {}, got {} — refusing to install (possible corruption or tampering)",
                            entry.path, expected, actual
                        ));
                    }
                    Err(e) => {
                        let _ = fs::remove_dir_all(&staging);
                        return Err(format!("Could not verify checksum for '{}': {}", entry.path, e));
                    }
                }
            }
            None => {
                unverified_count += 1;
            }
        }
    }
    if unverified_count > 0 {
        eprintln!(
            "[harvester] Warning: {} file(s) have no recorded checksum and could not be verified",
            unverified_count
        );
    }

    // Pre-install script, if present.
    if let Some(scripts) = &manifest.scripts {
        if let Some(pre) = &scripts.pre_install {
            if let Err(e) = run_script("pre_install", pre, cfg) {
                let _ = fs::remove_dir_all(&staging);
                return Err(format!("pre_install script failed: {}", e));
            }
        }
    }

    // Move each file from staging into osiris_root. Uses the archive's own
    // entry list (not just manifest-tracked files) so anything present in
    // the archive but not individually catalogued in the manifest still
    // gets installed — the manifest's file list is for verification and
    // later removal/conflict tracking, not the sole source of what to
    // install.
    let mut installed_paths: Vec<String> = Vec::new();
    for entry in &entries {
        let src = staging.join(entry);
        let dst = cfg.osiris_root.join(entry);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {}", parent.display(), e))?;
        }
        fs::rename(&src, &dst)
            .or_else(|_| fs::copy(&src, &dst).map(|_| ())) // fallback if staging/osiris_root differ across filesystems
            .map_err(|e| format!("Could not move '{}' into place: {}", entry, e))?;
        installed_paths.push(entry.clone());
    }

    let _ = fs::remove_dir_all(&staging);

    // Post-install script, now that files are actually in place.
    if let Some(scripts) = &manifest.scripts {
        if let Some(post) = &scripts.post_install {
            if let Err(e) = run_script("post_install", post, cfg) {
                // Files are already installed at this point — a failing
                // post_install script doesn't get rolled back automatically
                // (full transactional rollback is OPIUM's Phase 2B scope
                // per ph.md, not implemented here). Reported, not silent.
                eprintln!("[harvester] Warning: post_install script failed: {}", e);
            }
        }
    }

    record_installed(name, source_label, &installed_paths, &manifest, cfg)?;
    println!("[harvester] Installed: {} ({} files)", name, installed_paths.len());
    Ok(())
}

fn record_installed(
    name: &str,
    source: &str,
    files: &[String],
    manifest: &Manifest,
    cfg: &OsirisConfig,
) -> Result<(), String> {
    fs::create_dir_all(&cfg.pkg_db)
        .map_err(|e| format!("Could not create package db: {}", e))?;

    let arch = crate::config::detect_arch();
    let files_toml = files.iter()
        .map(|f| format!("    \"{}\",\n", f.replace('"', "\\\"")))
        .collect::<String>();

    let record = format!(
        "name = \"{}\"\nversion = \"installed\"\nsource = \"{}\"\narch = \"{}\"\nfiles = [\n{}]\n",
        name, source, arch, files_toml
    );

    fs::write(cfg.pkg_db.join(format!("{}.toml", name)), record)
        .map_err(|e| format!("Could not write install record: {}", e))?;

    // Retain the full manifest for reverse-dependency checks and
    // pre/post-remove scripts at removal time.
    fs::write(cfg.pkg_db.join(format!("{}.manifest.toml", name)), manifest.to_toml())
        .map_err(|e| format!("Could not write retained manifest: {}", e))?;

    Ok(())
}

pub fn read_installed_files(name: &str, cfg: &OsirisConfig) -> Vec<String> {
    let path = cfg.pkg_db.join(format!("{}.toml", name));
    let Ok(content) = fs::read_to_string(&path) else { return Vec::new() };

    let mut files = Vec::new();
    let mut in_files_block = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("files = [") {
            in_files_block = true;
            continue;
        }
        if in_files_block {
            if trimmed == "]" {
                break;
            }
            let cleaned = trimmed.trim_end_matches(',').trim_matches('"').replace("\\\"", "\"");
            if !cleaned.is_empty() {
                files.push(cleaned);
            }
        }
    }

    files
}

/// Read the retained manifest for an installed package, if present — used
/// by remove.rs for reverse-dependency checks and pre/post-remove scripts.
pub fn read_installed_manifest(name: &str, cfg: &OsirisConfig) -> Option<Manifest> {
    let path = cfg.pkg_db.join(format!("{}.manifest.toml", name));
    Manifest::from_file(&path).ok()
}

pub fn is_installed(name: &str, cfg: &OsirisConfig) -> bool {
    cfg.pkg_db.join(format!("{}.toml", name)).exists()
}

pub fn list_installed(cfg: &OsirisConfig) {
    let path = &cfg.pkg_db;

    if !path.exists() {
        println!("[harvester] No packages installed yet");
        return;
    }

    println!("[harvester] Installed packages:");
    println!("{:-<44}", "");

    match fs::read_dir(path) {
        Ok(entries) => {
            let mut count = 0;
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname = fname.to_string_lossy();
                if fname.ends_with(".toml") && !fname.ends_with(".manifest.toml") {
                    let pkg_name = fname.replace(".toml", "");
                    let source = read_field(&cfg.pkg_db.join(fname.as_ref()), "source")
                        .unwrap_or_else(|| "unknown".to_string());
                    let arch = read_field(&cfg.pkg_db.join(fname.as_ref()), "arch")
                        .unwrap_or_else(|| "?".to_string());
                    println!("  {:<28} [{:<10}] [{}]", pkg_name, source, arch);
                    count += 1;
                }
            }
            println!("{:-<44}", "");
            println!("  Total: {} packages", count);
        }
        Err(e) => eprintln!("[harvester] Error reading package db: {}", e),
    }
}

fn read_field(path: &Path, field: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if line.starts_with(field) {
            return line.split('"').nth(1).map(|s| s.to_string());
        }
    }
    None
}
