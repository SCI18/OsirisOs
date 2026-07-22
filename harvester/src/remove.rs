// Harvester — remove.rs
// Low level package removal.
// remove — unregisters the package, deletes its files
// purge  — remove + wipes config files

use std::fs;
use crate::config::OsirisConfig;
use crate::install::{is_installed, read_installed_files, read_installed_manifest, validate_package_name};

pub fn remove(name: &str, purge: bool, force: bool, cfg: &OsirisConfig) -> Result<(), String> {
    validate_package_name(name)?;

    if !is_installed(name, cfg) {
        return Err(format!("'{}' is not installed", name));
    }

    // FIX: reverse-dependency check, now possible because harvest.rs
    // actually populates `depends` and install.rs retains each package's
    // full manifest. Scans every other installed package's retained
    // manifest for `name` in its depends list.
    if !force {
        let dependents = find_dependents(name, cfg);
        if !dependents.is_empty() {
            return Err(format!(
                "Refusing to remove '{}': depended on by {} — use --force to override (may break those packages)",
                name, dependents.join(", ")
            ));
        }
    }

    let manifest = read_installed_manifest(name, cfg);

    // Pre-remove script, if the retained manifest has one.
    if let Some(m) = &manifest {
        if let Some(scripts) = &m.scripts {
            if let Some(pre) = &scripts.pre_remove {
                if let Err(e) = crate::install::run_script("pre_remove", pre, cfg) {
                    if !force {
                        return Err(format!("pre_remove script failed: {} (use --force to remove anyway)", e));
                    }
                    eprintln!("[harvester] Warning: pre_remove script failed, continuing due to --force: {}", e);
                }
            }
        }
    }

    let record_path = cfg.pkg_db.join(format!("{}.toml", name));
    let manifest_path = cfg.pkg_db.join(format!("{}.manifest.toml", name));
    let source = read_source(&record_path);

    let files = read_installed_files(name, cfg);
    let mut removed_count = 0;
    let mut missing_count = 0;

    for rel_path in &files {
        let full_path = cfg.osiris_root.join(rel_path);
        match fs::remove_file(&full_path) {
            Ok(()) => removed_count += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing_count += 1,
            Err(e) => {
                eprintln!("[harvester] Warning: could not remove {}: {}", full_path.display(), e);
            }
        }
    }

    println!(
        "[harvester] Removed {} file(s){} for {}",
        removed_count,
        if missing_count > 0 { format!(" ({} already missing)", missing_count) } else { String::new() },
        name
    );

    if files.is_empty() {
        eprintln!(
            "[harvester] Warning: no file list recorded for '{}' — package may predate file-list tracking. \
             Only DB records will be removed; files (if any) may remain on disk.",
            name
        );
    }

    fs::remove_file(&record_path)
        .map_err(|e| format!("Could not remove install record: {}", e))?;
    let _ = fs::remove_file(&manifest_path); // best-effort, may not exist for pre-expansion installs

    println!("[harvester] Unregistered: {} (was: {})", name, source);

    // Post-remove script.
    if let Some(m) = &manifest {
        if let Some(scripts) = &m.scripts {
            if let Some(post) = &scripts.post_remove {
                if let Err(e) = crate::install::run_script("post_remove", post, cfg) {
                    eprintln!("[harvester] Warning: post_remove script failed: {}", e);
                }
            }
        }
    }

    if purge {
        purge_config(name, cfg)?;
    }

    Ok(())
}

/// Scan every other installed package's retained manifest for `name` in
/// its depends list. O(n) over installed packages — fine at expected
/// package-database scale; would want an index if that changes.
fn find_dependents(name: &str, cfg: &OsirisConfig) -> Vec<String> {
    let mut dependents = Vec::new();

    let Ok(entries) = fs::read_dir(&cfg.pkg_db) else { return dependents };

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        if !fname.ends_with(".manifest.toml") {
            continue;
        }
        let other_name = fname.trim_end_matches(".manifest.toml");
        if other_name == name {
            continue; // don't count the package against itself
        }

        if let Some(other_manifest) = read_installed_manifest(other_name, cfg) {
            if let Some(deps) = &other_manifest.package.depends {
                if deps.iter().any(|d| d == name) {
                    dependents.push(other_name.to_string());
                }
            }
        }
    }

    dependents
}

fn purge_config(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Purging config files for {}...", name);
    let config_dir = cfg.osiris_root.join("etc").join(name);

    if config_dir.exists() {
        fs::remove_dir_all(&config_dir)
            .map_err(|e| format!("Could not purge config dir: {}", e))?;
        println!("[harvester] Purged: {}", config_dir.display());
    } else {
        println!("[harvester] No config files found for {}", name);
    }

    Ok(())
}

fn read_source(path: &std::path::Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("source"))
        .and_then(|l| l.split('"').nth(1))
        .unwrap_or("unknown")
        .to_string()
}
