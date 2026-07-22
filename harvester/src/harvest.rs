// Harvester — harvest.rs
// Extracts packages from Debian proot into .osr format.

use std::fs;
use std::process::Command;
use crate::config::{OsirisConfig, detect_arch};
use crate::install::validate_package_name;
use crate::manifest::{Manifest, PackageSource, FileList, FileEntry, compute_checksum};

pub fn harvest(name: &str, cfg: &OsirisConfig) -> Result<(), String> {
    validate_package_name(name)?;

    let debian_root = &cfg.debian_root;

    let bin_path = debian_root.join("usr/bin").join(name);
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

    // NOTE (security, flagged not fixed): ldd can execute the target
    // binary via dynamic-loader env tricks on some systems. Accepted risk
    // today since harvest only targets trusted local Debian proot binaries
    // — do not point this at untrusted binaries without switching to
    // static analysis (e.g. `readelf -d` NEEDED entries) first.
    let mut lib_deps = get_lib_deps(actual_bin.to_str().unwrap(), cfg);
    let package_deps = get_package_deps(name);

    println!("[harvester] Library dependencies found: {}", lib_deps.len());
    println!("[harvester] Package dependencies found: {}", package_deps.len());

    let staging = std::env::temp_dir().join(format!("harvester-staging-{}", name));
    let bin_dir = staging.join("usr/bin");
    let lib_dir = staging.join("usr/lib").join(format!("{}-linux-gnu", detect_arch()));
    let ld_dir = staging.join("lib");

    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&lib_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&ld_dir).map_err(|e| e.to_string())?;

    let bin_dst = bin_dir.join(name);
    fs::copy(&actual_bin, &bin_dst)
        .map_err(|e| format!("Could not copy binary: {}", e))?;
    println!("[harvester] Copied binary: {}", name);

    // FIX: checksum computed for every file actually placed in staging,
    // and its path recorded relative to osiris_root (i.e. "usr/bin/<name>",
    // not an absolute staging path) — this is the path install.rs will
    // check the file against after the package is eventually installed.
    let bin_checksum = compute_checksum(&bin_dst).ok();
    let mut bin_entries = vec![FileEntry {
        path: format!("usr/bin/{}", name),
        checksum: bin_checksum,
    }];

    lib_deps.dedup();
    let mut lib_entries: Vec<FileEntry> = Vec::new();

    for dep in &lib_deps {
        let libname = dep.split('/').last().unwrap_or(dep);
        if let Some(src) = find_lib(libname, cfg) {
            let (dst, rel_path) = if libname.starts_with("ld-linux") {
                (ld_dir.join(libname), format!("lib/{}", libname))
            } else {
                (
                    lib_dir.join(libname),
                    format!("usr/lib/{}-linux-gnu/{}", detect_arch(), libname),
                )
            };
            match fs::copy(&src, &dst) {
                Ok(_) => {
                    println!("[harvester]   + {}", libname);
                    let checksum = compute_checksum(&dst).ok();
                    lib_entries.push(FileEntry { path: rel_path, checksum });
                }
                Err(e) => println!("[harvester]   ! {} (copy failed: {})", libname, e),
            }
        } else {
            println!("[harvester]   ? {} (not found — may already exist in Osiris)", libname);
        }
    }

    // bin_entries currently holds only the main binary; if in future
    // multiple binaries are harvested per package, extend this vec rather
    // than reassigning.
    let _ = &mut bin_entries;

    let mut manifest = Manifest::new_with_source(name, "harvested", PackageSource::Harvester);
    if !package_deps.is_empty() {
        manifest.package.depends = Some(package_deps);
    }
    manifest.files = Some(FileList {
        bin: Some(bin_entries),
        lib: if lib_entries.is_empty() { None } else { Some(lib_entries) },
        share: None,
        etc: None,
    });
    // Scripts intentionally left None here — harvest.rs harvests an
    // existing Debian-proot binary, which has no natural pre/post-install
    // hook source. Scripts are populated by whatever authors a package
    // directly (e.g. a future `harvester package` command for
    // hand-authored .osr packages), not by the harvest-from-proot path.

    fs::write(staging.join("manifest.toml"), manifest.to_toml())
        .map_err(|e| format!("Could not write manifest: {}", e))?;

    fs::create_dir_all(&cfg.pkg_cache).map_err(|e| e.to_string())?;

    let osr_path = cfg.pkg_cache.join(format!("{}.osr", name));
    let osr_str = osr_path.to_str().ok_or("Invalid cache path")?;
    let staging_str = staging.to_str().ok_or("Invalid staging path")?;

    let status = Command::new("tar")
        .args(&["-czf", osr_str, "-C", staging_str, "."])
        .status()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if !status.success() {
        return Err(format!("Failed to pack .osr for {}", name));
    }

    let _ = fs::remove_dir_all(&staging);

    println!("[harvester] Package ready: {} (checksums recorded)", osr_path.display());
    Ok(())
}

fn get_lib_deps(binary: &str, cfg: &OsirisConfig) -> Vec<String> {
    let mut all_deps: Vec<String> = Vec::new();
    let mut to_check: Vec<String> = vec![binary.to_string()];
    let mut checked: Vec<String> = Vec::new();

    while !to_check.is_empty() {
        let current = to_check.remove(0);
        if checked.contains(&current) {
            continue;
        }
        checked.push(current.clone());

        let output = Command::new("ldd").arg(&current).output();

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
                        let libname = path.split('/').last().unwrap_or("").to_string();
                        if !libname.is_empty() && !all_deps.contains(&libname) {
                            all_deps.push(libname.clone());
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

/// Queries actual package dependencies via dpkg's status database, not its
/// file list. Strips version constraints and takes the first alternative
/// in "a | b" syntax.
fn get_package_deps(name: &str) -> Vec<String> {
    let output = Command::new("dpkg-query")
        .args(&["-W", "-f=${Depends}", name])
        .output();

    let raw = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return vec![],
    };

    raw.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let first_alt = entry.split('|').next().unwrap_or(entry).trim();
            let name_only = first_alt.split_whitespace().next().unwrap_or(first_alt);
            if name_only.is_empty() { None } else { Some(name_only.to_string()) }
        })
        .collect()
}

fn find_lib(name: &str, cfg: &OsirisConfig) -> Option<String> {
    let debian_root = &cfg.debian_root;
    let arch = detect_arch();

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
