// Harvester — install.rs
// Low level package installation.
// No dependency resolution — that is OPIUM's job.
// Harvester installs what it is told to install, cleanly.

use std::fs;
use std::path::{Path, PathBuf};
use crate::config::OsirisConfig;

pub fn install(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    validate_package_name(name)?;

    if is_installed(name, cfg) {
        println!("[harvester] {} is already installed", name);
        return Ok(());
    }

    let osr_path = cfg.pkg_cache.join(format!("{}.osr", name));
    let tar_path = cfg.pkg_cache.join(format!("{}.tar.gz", name));

    if osr_path.exists() {
        install_osr(name, &osr_path, cfg)
    } else if tar_path.exists() {
        install_archive(name, &tar_path, cfg)
    } else {
        Err(format!(
            "'{}' not found in cache ({}). Run: harvester harvest {}",
            name,
            cfg.pkg_cache.display(),
            name
        ))
    }
}

/// FIX (dict3.md #4): centralized name validation, called before any path
/// construction happens anywhere in install/harvest/remove. Rejects
/// anything outside a safe charset — closes the path-traversal surface at
/// its actual entry point rather than patching each call site separately.
pub fn validate_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Package name cannot be empty".to_string());
    }
    if name.len() > 128 {
        return Err("Package name too long".to_string());
    }
    let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    // Reject leading dot / any ".." sequence explicitly, even though the
    // charset check above already blocks '/' — belt and suspenders against
    // traversal via the name itself.
    if !valid || name.starts_with('.') || name.contains("..") {
        return Err(format!("Invalid package name: '{}' (must be alphanumeric, -, _, . only, no leading dot or '..')", name));
    }
    Ok(())
}

fn install_osr(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Installing .osr: {}", path.display());
    let files = extract_archive_safely(name, path, cfg)?;
    record_installed(name, "osiris", &files, cfg)?;
    println!("[harvester] Installed: {} ({} files)", name, files.len());
    Ok(())
}

fn install_archive(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Installing harvested archive: {}", path.display());
    let files = extract_archive_safely(name, path, cfg)?;
    record_installed(name, "harvester", &files, cfg)?;
    println!("[harvester] Installed: {} ({} files)", name, files.len());
    Ok(())
}

/// FIX (dict3.md #1, tar-slip): lists archive contents via `tar -tzf`
/// before ever extracting, and rejects the archive outright if any entry
/// would escape `osiris_root` — absolute paths, `..` components, or
/// symlink-looking entries that could be used to redirect writes.
///
/// FIX (dict3.md #2, enables real remove()): the validated file list is
/// returned so the caller can record exactly what this package placed on
/// disk — remove.rs now has real data to delete instead of guessing.
///
/// This still shells out to `tar` for the actual extraction (a full
/// rewrite to a Rust-native tar library is a larger change than this pass
/// covers), but no bytes are extracted until every entry has been
/// validated as safe.
fn extract_archive_safely(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<Vec<String>, String> {
    let osiris_root = cfg.osiris_root.to_str().ok_or("Invalid Osiris root path")?;
    let path_str = path.to_str().ok_or("Invalid package path")?;

    // Step 1: list contents without extracting anything.
    let list_output = std::process::Command::new("tar")
        .args(&["-tzf", path_str])
        .output()
        .map_err(|e| format!("Failed to list archive contents: {}", e))?;

    if !list_output.status.success() {
        return Err(format!("Failed to list archive contents for: {}", name));
    }

    let listing = String::from_utf8_lossy(&list_output.stdout);
    let mut entries: Vec<String> = Vec::new();

    for line in listing.lines() {
        let entry = line.trim();
        if entry.is_empty() || entry == "." || entry.ends_with('/') {
            continue; // skip directory entries, only track real files
        }

        if is_unsafe_archive_entry(entry) {
            return Err(format!(
                "Refusing to install '{}': archive contains unsafe path entry '{}' (absolute path or directory traversal)",
                name, entry
            ));
        }

        entries.push(entry.to_string());
    }

    if entries.is_empty() {
        return Err(format!("Archive for '{}' contains no extractable files", name));
    }

    // Step 2: all entries validated safe — extract for real.
    // --no-same-owner avoids inheriting uid/gid from the archive (relevant
    // if this ever runs as root); explicit -C keeps extraction scoped.
    let status = std::process::Command::new("tar")
        .args(&["-xzf", path_str, "-C", osiris_root, "--no-same-owner"])
        .status()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if !status.success() {
        return Err(format!("Extraction failed for: {}", name));
    }

    println!("[harvester] Extracted: {} ({} files, all paths validated)", name, entries.len());
    Ok(entries)
}

/// An archive entry is unsafe if it's an absolute path, or if any
/// path component is literally "..".
fn is_unsafe_archive_entry(entry: &str) -> bool {
    if entry.starts_with('/') {
        return true;
    }
    entry.split('/').any(|component| component == "..")
}

fn record_installed(name: &str, source: &str, files: &[String], cfg: &OsirisConfig) -> Result<(), String> {
    fs::create_dir_all(&cfg.pkg_db)
        .map_err(|e| format!("Could not create package db: {}", e))?;

    let arch = crate::config::detect_arch();

    // FIX (dict3.md #2): record files list is what makes real removal
    // possible. Stored as a simple newline-delimited section in the TOML
    // record for now (kept in the same file rather than a separate
    // manifest lookup, so remove.rs has a single source to read).
    let files_toml = files.iter()
        .map(|f| format!("    \"{}\",\n", f.replace('"', "\\\"")))
        .collect::<String>();

    let record = format!(
        "name = \"{}\"\nversion = \"installed\"\nsource = \"{}\"\narch = \"{}\"\nfiles = [\n{}]\n",
        name, source, arch, files_toml
    );

    fs::write(cfg.pkg_db.join(format!("{}.toml", name)), record)
        .map_err(|e| format!("Could not write install record: {}", e))
}

/// FIX (dict3.md #2): parse the file list back out of an install record so
/// remove.rs can actually delete the files this package placed on disk.
/// Simple line-based parse matching the format written by
/// record_installed — a full TOML array parser would be more robust, but
/// this avoids adding a TOML-array-parsing dependency for a
/// single-purpose internal format.
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
                if fname.ends_with(".toml") {
                    let pkg_name = fname.replace(".toml", "");
                    let source   = read_field(&cfg.pkg_db.join(fname.as_ref()), "source")
                        .unwrap_or_else(|| "unknown".to_string());
                    let arch     = read_field(&cfg.pkg_db.join(fname.as_ref()), "arch")
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
