// OPIUM - Official Package Manager In Use for Osiris
// "What the god needs, the god receives."
//
// Architecture:
//   OPIUM  = apt equivalent  — dependency resolution, repo management, user interface
//   Harvester = dpkg equivalent — low level install/remove, calls made by OPIUM

mod config;
mod db;
mod manifest;
mod repo;
// harvest is handled by the Harvester binary — opium delegates via call_harvester()

use std::env;
use std::process::Command;
use config::OsirisConfig;

const VERSION: &str = "0.2.0-alpha";

fn print_help() {
    println!("OPIUM - Official Package Manager In Use for Osiris");
    println!("Version {}\n", VERSION);
    println!("Usage: opium <command> [package]\n");
    println!("Commands:");
    println!("  install, -i <pkg>    Install a package and its dependencies");
    println!("  remove,  -r <pkg>    Remove a package");
    println!("  purge,   -p <pkg>    Remove package and config files");
    println!("  list,    -l          List installed packages");
    println!("  search,  -s <query>  Search available packages");
    println!("  info,    -I <pkg>    Show package details");
    println!("  update               Refresh package index");
    println!("  harvest  <pkg>       Harvest package from Debian proot via Harvester");
    println!("  env                  Show detected Osiris environment");
}

fn call_harvester(args: &[&str]) -> Result<(), String> {
    let status = Command::new("harvester")
        .args(args)
        .status()
        .map_err(|e| format!(
            "Could not launch Harvester: {}. Is it installed?", e
        ))?;

    if status.success() {
        Ok(())
    } else {
        Err("Harvester exited with an error".to_string())
    }
}

fn main() {
    let cfg = OsirisConfig::resolve();

    if let Err(e) = cfg.init_dirs() {
        eprintln!("[opium] Warning: could not init dirs: {}", e);
    }

    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_help();
        return;
    }

    let command = &args[1];
    let package = args.get(2).map(|s| s.as_str()).unwrap_or("");

    match command.as_str() {
        "install" | "-i" => {
            if package.is_empty() {
                eprintln!("Usage: opium install <package>");
                return;
            }
            println!("[opium] Installing {}...", package);
            match db::install(package, &cfg) {
                Ok(_)  => println!("[opium] {} installed successfully", package),
                Err(e) => eprintln!("[opium] Error: {}", e),
            }
        }

        "remove" | "-r" => {
            if package.is_empty() {
                eprintln!("Usage: opium remove <package>");
                return;
            }
            println!("[opium] Removing {}...", package);
            match db::remove(package, false, &cfg) {
                Ok(_)  => println!("[opium] {} removed successfully", package),
                Err(e) => eprintln!("[opium] Error: {}", e),
            }
        }

        "purge" | "-p" => {
            if package.is_empty() {
                eprintln!("Usage: opium purge <package>");
                return;
            }
            println!("[opium] Purging {}...", package);
            match db::remove(package, true, &cfg) {
                Ok(_)  => println!("[opium] {} purged successfully", package),
                Err(e) => eprintln!("[opium] Error: {}", e),
            }
        }

        "list" | "-l" => {
            db::list_installed(&cfg);
        }

        "search" | "-s" => {
            if package.is_empty() {
                eprintln!("Usage: opium search <query>");
                return;
            }
            repo::search(package, &cfg);
        }

        "info" | "-I" => {
            if package.is_empty() {
                eprintln!("Usage: opium info <package>");
                return;
            }
            repo::info(package, &cfg);
        }

        "update" => {
            println!("[opium] Refreshing package index...");
            match repo::update(&cfg) {
                Ok(_)  => println!("[opium] Index updated"),
                Err(e) => eprintln!("[opium] Error: {}", e),
            }
        }

        // Harvest delegates entirely to the Harvester binary
        // OPIUM is the orchestrator. Harvester is the workhorse.
        "harvest" => {
            if package.is_empty() {
                eprintln!("Usage: opium harvest <package>");
                return;
            }
            println!("[opium] Delegating to Harvester for {}...", package);
            match call_harvester(&["harvest", package]) {
                Ok(_)  => println!("[opium] Harvest complete. Run: opium install {}", package),
                Err(e) => eprintln!("[opium] Error: {}", e),
            }
        }

        "env" => {
            println!("[opium] Environment : {}", cfg.env_name());
            println!("[opium] Osiris root : {}", cfg.osiris_root.display());
            println!("[opium] Debian root : {}", cfg.debian_root.display());
            println!("[opium] Package DB  : {}", cfg.pkg_db.display());
            println!("[opium] Cache       : {}", cfg.pkg_cache.display());
        }

        "help" | "--help" | "-h" => print_help(),

        _ => {
            eprintln!("[opium] Unknown command: {}", command);
            print_help();
        }
    }
}
