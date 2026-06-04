// Osiris Package Database
// Tracks installed packages. Low-level record keeping.
// OPIUM calls this. Users don't call this directly.

use std::fs;
use std::path::Path;
use crate::config::OsirisConfig;
// manifest imported when package version tracking is implemented

pub fn install(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    if is_installed(name, cfg) {
        println!("[opium] {} is already installed", name);
        return Ok(());
    }

    // Look for .osr package in cache first, then legacy .tar.gz
    let osr_path = cfg.pkg_cache.join(format!("{}.osr", name));
    let tar_path = cfg.pkg_cache.join(format!("{}.tar.gz", name));

    if osr_path.exists() {
        install_osr(name, osr_path.to_str().unwrap(), cfg)
    } else if tar_path.exists() {
        install_tar(name, tar_path.to_str().unwrap(), cfg)
    } else {
        Err(format!(
            "Package '{}' not found in cache. Run: opium harvest {}",
            name, name
        ))
    }
}

fn install_osr(name: &str, path: &str, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[opium] Installing from .osr: {}", path);
    extract_package(name, path, cfg)?;
    record_installed(name, "osiris", cfg)?;
    Ok(())
}

fn install_tar(name: &str, path: &str, cfg: &OsirisConfig) -> Result<(), String> {
    println!("[opium] Installing from harvested archive: {}", path);
    extract_package(name, path, cfg)?;
    record_installed(name, "harvester", cfg)?;
    Ok(())
}

fn extract_package(name: &str, path: &str, cfg: &OsirisConfig) -> Result<(), String> {
    let osiris_root = cfg.osiris_root.to_str()
        .ok_or("Invalid Osiris root path")?;

    let status = std::process::Command::new("tar")
        .args(&["-xzf", path, "-C", osiris_root])
        .status()
        .map_err(|e| e.to_string())?;

    if status.success() {
        println!("[opium] Extracted: {}", name);
        Ok(())
    } else {
        Err(format!("Failed to extract package: {}", name))
    }
}

fn record_installed(name: &str, source: &str, cfg: &OsirisConfig) -> Result<(), String> {
    fs::create_dir_all(&cfg.pkg_db).map_err(|e| e.to_string())?;

    let arch = crate::config::detect_arch();
    let record = format!(
        "name = \"{}\"\nsource = \"{}\"\narch = \"{}\"\n",
        name, source, arch
    );

    fs::write(cfg.pkg_db.join(format!("{}.toml", name)), record)
        .map_err(|e| e.to_string())
}

pub fn remove(name: &str, purge: bool, cfg: &OsirisConfig) -> Result<(), String> {
    if !is_installed(name, cfg) {
        return Err(format!("'{}' is not installed", name));
    }

    let record_path = cfg.pkg_db.join(format!("{}.toml", name));
    fs::remove_file(&record_path).map_err(|e| e.to_string())?;

    if purge {
        println!("[opium] Purging config files for {}...", name);
        // Config cleanup — expanded in future milestone
    }

    println!("[opium] Removed: {}", name);
    Ok(())
}

pub fn is_installed(name: &str, cfg: &OsirisConfig) -> bool {
    cfg.pkg_db.join(format!("{}.toml", name)).exists()
}

pub fn list_installed(cfg: &OsirisConfig) {
    let path = &cfg.pkg_db;

    if !path.exists() {
        println!("[opium] No packages installed yet");
        return;
    }

    println!("[opium] Installed packages:");
    println!("{:-<40}", "");

    match fs::read_dir(path) {
        Ok(entries) => {
            let mut count = 0;
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".toml") {
                    // Read source from record
                    let source = read_source(&cfg.pkg_db
                        .join(name.as_ref()))
                        .unwrap_or_else(|| "unknown".to_string());
                    println!("  {:<30} [{}]", name.replace(".toml", ""), source);
                    count += 1;
                }
            }
            println!("{:-<40}", "");
            println!("  Total: {} packages", count);
        }
        Err(e) => eprintln!("[opium] Error reading package db: {}", e),
    }
}

fn read_source(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines() {
        if line.starts_with("source") {
            return line.split('"').nth(1).map(|s| s.to_string());
        }
    }
    None
}
