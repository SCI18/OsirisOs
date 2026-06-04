// Harvester — Osiris Package Workhorse
// dpkg equivalent + APK bridge
// "The reaper takes what is needed and installs it cleanly."
//
// OPIUM calls Harvester. Users can also call Harvester directly (Netjeru).
// Ramesses users never see this — OPIUM handles everything for them.

mod config;
mod harvest;
mod install;
mod remove;

use std::env;
use config::OsirisConfig;

const VERSION: &str = "0.2.0-alpha";

fn print_help() {
    println!("Harvester — Osiris Package Workhorse");
    println!("Version {}\n", VERSION);
    println!("Usage: harvester <command> [package]\n");
    println!("Commands:");
    println!("  harvest  <pkg>    Extract package from Debian proot → .osr");
    println!("  install  <pkg>    Install a .osr package directly");
    println!("  remove   <pkg>    Remove an installed package");
    println!("  list              List installed packages");
    println!("  env               Show detected Osiris environment");
    println!("  help              Show this help\n");
    println!("Note: For dependency resolution and repo management, use OPIUM.");
}

fn main() {
    let cfg = OsirisConfig::resolve();

    if let Err(e) = cfg.init_dirs() {
        eprintln!("[harvester] Warning: could not init dirs: {}", e);
    }

    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_help();
        return;
    }

    let command = &args[1];
    let package = args.get(2).map(|s| s.as_str()).unwrap_or("");

    match command.as_str() {
        "harvest" => {
            if package.is_empty() {
                eprintln!("Usage: harvester harvest <package>");
                return;
            }
            println!("[harvester] Harvesting {}...", package);
            match harvest::harvest(package, &cfg) {
                Ok(_)  => println!(
                    "[harvester] {} harvested successfully. Run: opium install {}",
                    package, package
                ),
                Err(e) => eprintln!("[harvester] Error: {}", e),
            }
        }

        "install" => {
            if package.is_empty() {
                eprintln!("Usage: harvester install <package>");
                return;
            }
            println!("[harvester] Installing {}...", package);
            match install::install(package, &cfg) {
                Ok(_)  => println!("[harvester] {} installed", package),
                Err(e) => eprintln!("[harvester] Error: {}", e),
            }
        }

        "remove" => {
            if package.is_empty() {
                eprintln!("Usage: harvester remove <package>");
                return;
            }
            println!("[harvester] Removing {}...", package);
            match remove::remove(package, false, &cfg) {
                Ok(_)  => println!("[harvester] {} removed", package),
                Err(e) => eprintln!("[harvester] Error: {}", e),
            }
        }

        "list" => {
            install::list_installed(&cfg);
        }

        "env" => {
            println!("[harvester] Environment : {}", cfg.env_name());
            println!("[harvester] Osiris root : {}", cfg.osiris_root.display());
            println!("[harvester] Debian root : {}", cfg.debian_root.display());
            println!("[harvester] Cache       : {}", cfg.pkg_cache.display());
            println!("[harvester] Package DB  : {}", cfg.pkg_db.display());
        }

        "help" | "--help" | "-h" => print_help(),

        _ => {
            eprintln!("[harvester] Unknown command: {}", command);
            print_help();
        }
    }
}
