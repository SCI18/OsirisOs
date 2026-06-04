// Harvester — remove.rs
// Low level package removal.
// remove — unregisters the package from the db
// purge  — remove + wipes config files

use std::fs;
use crate::config::OsirisConfig;
use crate::install::is_installed;

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

/// Purge config files left behind after removal
fn purge_config(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Purging config files for {}...", name);

    // Config typically lives under osiris_root/etc/<package>
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
