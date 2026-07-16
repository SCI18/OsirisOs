// Osiris Package Repository
// Manages package index and search

use std::fs;
use std::path::Path;
use crate::config::OsirisConfig;

pub fn search(query: &str, cfg: &OsirisConfig) {
    println!("[opium] Searching for '{}'...\n", query);

    if !cfg.pkg_index.exists() {
        println!("[opium] No index found. Run: opium update");
        return;
    }

    match fs::read_to_string(&cfg.pkg_index) {
        Ok(content) => {
            let matches: Vec<&str> = content
                .lines()
                .filter(|l| l.to_lowercase().contains(&query.to_lowercase()))
                .collect();

            if matches.is_empty() {
                println!("[opium] No packages found matching '{}'", query);
            } else {
                for m in matches {
                    println!("  {}", m);
                }
            }
        }
        Err(e) => eprintln!("[opium] Error reading index: {}", e),
    }
}

pub fn info(name: &str, cfg: &OsirisConfig) {
    let manifest_path = cfg.pkg_repo.join(format!("{}.toml", name));

    match fs::read_to_string(&manifest_path) {
        Ok(content) => println!("{}", content),
        Err(_)      => println!("[opium] No info found for '{}'", name),
    }
}

pub fn update(cfg: &OsirisConfig) -> Result<(), String> {
    fs::create_dir_all(&cfg.pkg_repo)
        .map_err(|e| e.to_string())?;

    // Scan installed packages and build index
    let mut index = String::from("# Osiris Package Index\n\n");

    if cfg.pkg_db.exists() {
        if let Ok(entries) = fs::read_dir(&cfg.pkg_db) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy().replace(".toml", "");
                index.push_str(&format!("{}\n", name));
            }
        }
    }

    fs::write(&cfg.pkg_index, index)
        .map_err(|e| e.to_string())?;

    Ok(())
}
