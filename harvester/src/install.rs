// Harvester — install.rs
// Low level package installation.
// No dependency resolution — that is OPIUM's job.
// Harvester installs what it is told to install, cleanly.

use std::fs;
use std::path::Path;
use crate::config::OsirisConfig;

pub fn install(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    if is_installed(name, cfg) {
        println!("[harvester] {} is already installed", name);
        return Ok(());
    }

    // .osr is the native format — always look for it first
    let osr_path = cfg.pkg_cache.join(format!("{}.osr", name));
    let tar_path = cfg.pkg_cache.join(format!("{}.tar.gz", name));

    if osr_path.exists() {
        install_osr(name, &osr_path, cfg)
    } else if tar_path.exists() {
        // Legacy harvested archive — still supported during transition
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

fn install_osr(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Installing .osr: {}", path.display());
    extract_archive(name, path, cfg)?;
    record_installed(name, "osiris", cfg)?;
    println!("[harvester] Installed: {}", name);
    Ok(())
}

fn install_archive(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[harvester] Installing harvested archive: {}", path.display());
    extract_archive(name, path, cfg)?;
    record_installed(name, "harvester", cfg)?;
    println!("[harvester] Installed: {}", name);
    Ok(())
}

fn extract_archive(name: &str, path: &Path, cfg: &OsirisConfig) -> Result<(), String> {
    let osiris_root = cfg.osiris_root.to_str()
        .ok_or("Invalid Osiris root path")?;

    let path_str = path.to_str()
        .ok_or("Invalid package path")?;

    let status = std::process::Command::new("tar")
        .args(&["-xzf", path_str, "-C", osiris_root])
        .status()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if status.success() {
        println!("[harvester] Extracted: {}", name);
        Ok(())
    } else {
        Err(format!("Extraction failed for: {}", name))
    }
}

fn record_installed(name: &str, source: &str, cfg: &OsirisConfig) -> Result<(), String> {
    fs::create_dir_all(&cfg.pkg_db)
        .map_err(|e| format!("Could not create package db: {}", e))?;

    let arch = crate::config::detect_arch();
    let record = format!(
        "name = \"{}\"\nversion = \"installed\"\nsource = \"{}\"\narch = \"{}\"\n",
        name, source, arch
    );

    fs::write(cfg.pkg_db.join(format!("{}.toml", name)), record)
        .map_err(|e| format!("Could not write install record: {}", e))
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

/// Read a single field value from a TOML install record
fn read_field(path: &Path, field: &str) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if line.starts_with(field) {
            return line.split('"').nth(1).map(|s| s.to_string());
        }
    }
    None
}
