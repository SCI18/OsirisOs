// Harvester — remove.rs
// Low level package removal.
// remove — unregisters the package from the db AND deletes its files
// purge  — remove + wipes config files

use std::fs;
use crate::config::OsirisConfig;
use crate::install::{is_installed, read_installed_files, validate_package_name};

pub fn remove(name: &str, purge: bool, cfg: &OsirisConfig) -> Result<(), String> {
    validate_package_name(name)?;

    if !is_installed(name, cfg) {
        return Err(format!("'{}' is not installed", name));
    }

    let record_path = cfg.pkg_db.join(format!("{}.toml", name));
    let source = read_source(&record_path);

    // FIX (dict3.md #2): actually delete the files this package installed.
    // Previously this function only deleted the DB record, leaving every
    // installed file on disk permanently regardless of "removal".
    let files = read_installed_files(name, cfg);
    let mut removed_count = 0;
    let mut missing_count = 0;

    for rel_path in &files {
        let full_path = cfg.osiris_root.join(rel_path);
        match fs::remove_file(&full_path) {
            Ok(()) => removed_count += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Already gone — not fatal, just note it. A package
                // manager removing a file a user already deleted manually
                // shouldn't abort the whole operation.
                missing_count += 1;
            }
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
            "[harvester] Warning: no file list recorded for '{}' — this package may have been installed \
             before file-list tracking was added, or the install record is malformed. Only the DB record \
             will be removed; files (if any) may remain on disk.",
            name
        );
    }

    fs::remove_file(&record_path)
        .map_err(|e| format!("Could not remove install record: {}", e))?;

    println!("[harvester] Unregistered: {} (was: {})", name, source);

    if purge {
        purge_config(name, cfg)?;
    }

    Ok(())
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
