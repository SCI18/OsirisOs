// Harvester — harvest.rs
// Extracts packages from Debian proot into .osr format.
// Your original recursive ldd logic preserved and upgraded.

use std::fs;
use std::process::Command;
use std::path::PathBuf;
use crate::config::{OsirisConfig, detect_arch};

pub fn harvest(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    let debian_root = &cfg.debian_root;

    // Locate binary in Debian proot
    let bin_path     = debian_root.join("usr/bin").join(name);
    let alt_bin_path = debian_root.join("bin").join(name);

    let actual_bin = if bin_path.exists() {
        bin_path
    } else if alt_bin_path.exists() {
        alt_bin_path
    } else {
        return Err(format!(
            "'{}' not found in Debian proot ({})",
            name,
            debian_root.display()
        ));
    };

    println!("[harvester] Found: {}", actual_bin.display());

    // Collect all dependencies — recursive ldd + dpkg cross-check
    let mut deps = get_all_deps(actual_bin.to_str().unwrap(), cfg);
    let dpkg_deps = get_dpkg_deps(name);
    deps.extend(dpkg_deps);
    deps.dedup();

    println!("[harvester] Dependencies found: {}", deps.len());

    // Build staging directory
    let staging = PathBuf::from(format!("/tmp/harvester-staging-{}", name));
    fs::create_dir_all(staging.join("usr/bin"))
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(staging.join("usr/lib").join(format!("{}-linux-gnu", detect_arch())))
        .map_err(|e| e.to_string())?;
    fs::create_dir_all(staging.join("lib"))
        .map_err(|e| e.to_string())?;

    // Copy binary into staging
    fs::copy(&actual_bin, staging.join("usr/bin").join(name))
        .map_err(|e| format!("Could not copy binary: {}", e))?;
    println!("[harvester] Copied binary: {}", name);

    // Copy each dependency into staging
    for dep in &deps {
        let libname = dep.split('/').last().unwrap_or(dep);
        if let Some(src) = find_lib(libname, cfg) {
            let dst = if libname.starts_with("ld-linux") {
                staging.join("lib").join(libname)
            } else {
                staging.join("usr/lib")
                    .join(format!("{}-linux-gnu", detect_arch()))
                    .join(libname)
            };
            match fs::copy(&src, &dst) {
                Ok(_)  => println!("[harvester]   + {}", libname),
                Err(e) => println!("[harvester]   ! {} (copy failed: {})", libname, e),
            }
        } else {
            println!("[harvester]   ? {} (not found — may already exist in Osiris)", libname);
        }
    }

    // Write manifest into staging
    let manifest = format!(
        "[package]\nname = \"{}\"\nversion = \"harvested\"\narch = \"{}\"\nsource = \"harvester\"\n",
        name,
        detect_arch()
    );
    fs::write(staging.join("manifest.toml"), manifest)
        .map_err(|e| format!("Could not write manifest: {}", e))?;

    // Pack staging into .osr (tar.gz under the hood, .osr extension)
    fs::create_dir_all(&cfg.pkg_cache)
        .map_err(|e| e.to_string())?;

    let osr_path = cfg.pkg_cache.join(format!("{}.osr", name));
    let osr_str  = osr_path.to_str().ok_or("Invalid cache path")?;
    let staging_str = staging.to_str().ok_or("Invalid staging path")?;

    let status = Command::new("tar")
        .args(&["-czf", osr_str, "-C", staging_str, "."])
        .status()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if !status.success() {
        return Err(format!("Failed to pack .osr for {}", name));
    }

    // Clean up staging directory
    let _ = fs::remove_dir_all(&staging);

    println!("[harvester] Package ready: {}", osr_path.display());
    Ok(())
}

/// Recursively collect all shared library dependencies via ldd
/// Original algorithm preserved — cycle detection via checked list
fn get_all_deps(binary: &str, cfg: &OsirisConfig) -> Vec<String> {
    let mut all_deps: Vec<String> = Vec::new();
    let mut to_check: Vec<String> = vec![binary.to_string()];
    let mut checked:  Vec<String> = Vec::new();

    while !to_check.is_empty() {
        let current = to_check.remove(0);

        if checked.contains(&current) {
            continue;
        }
        checked.push(current.clone());

        let output = Command::new("ldd")
            .arg(&current)
            .output();

        if let Ok(o) = output {
            let stdout = String::from_utf8_lossy(&o.stdout);
            for line in stdout.lines() {
                let lib_path = if line.contains("=>") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    parts.get(2).map(|s| s.to_string())
                } else if line.trim().starts_with('/') {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    parts.first().map(|s| s.trim().to_string())
                } else {
                    None
                };

                if let Some(path) = lib_path {
                    if path.starts_with('/') && !path.contains("not found") {
                        let libname = path.split('/')
                            .last()
                            .unwrap_or("")
                            .to_string();

                        if !libname.is_empty() && !all_deps.contains(&libname) {
                            all_deps.push(libname.clone());
                            // Recurse into this lib's dependencies
                            if let Some(found) = find_lib(&libname, cfg) {
                                if !checked.contains(&found) {
                                    to_check.push(found);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    all_deps
}

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

/// Search for a library in known Debian proot locations
fn find_lib(name: &str, cfg: &OsirisConfig) -> Option<String> {
    let debian_root = &cfg.debian_root;
    let arch        = detect_arch();

    let search_paths = vec![
        debian_root.join("lib").join(format!("{}-linux-gnu", arch)).join(name),
        debian_root.join("usr/lib").join(format!("{}-linux-gnu", arch)).join(name),
        debian_root.join("lib").join(name),
    ];

    for path in search_paths {
        if path.exists() {
            return path.to_str().map(|s| s.to_string());
        }
    }
    None
}
