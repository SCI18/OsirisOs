// Harvester — Osiris Package Workhorse
// dpkg equivalent + APK bridge

mod config;
mod harvest;
mod install;
mod remove;
mod manifest;

use std::env;
use config::OsirisConfig;

const VERSION: &str = "0.2.0-alpha";

fn print_help() {
    println!("Harvester — Osiris Package Workhorse");
    println!("Version {}\n", VERSION);
    println!("Usage: harvester <command> [package] [flags]\n");
    println!("Commands:");
    println!("  harvest  <pkg>          Extract package from Debian proot → .osr");
    println!("  install  <pkg> [--force] Install a .osr package directly");
    println!("  remove   <pkg> [--purge] [--force]  Remove an installed package");
    println!("  list                    List installed packages");
    println!("  env                     Show detected Osiris environment");
    println!("  help                    Show this help\n");
    println!("Flags:");
    println!("  --force   Override file-conflict / reverse-dependency checks");
    println!("  --purge   (remove only) Also delete config files under etc/<pkg>/");
    println!("\nNote: For dependency resolution and repo management, use OPIUM.");
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
    let force = args.iter().any(|a| a == "--force");
    let purge = args.iter().any(|a| a == "--purge");

    match command.as_str() {
        "harvest" => {
            if package.is_empty() {
                eprintln!("Usage: harvester harvest <package>");
                return;
            }
            println!("[harvester] Harvesting {}...", package);
            match harvest::harvest(package, &cfg) {
                Ok(_) => println!(
                    "[harvester] {} harvested successfully. Run: opium install {}",
                    package, package
                ),
                Err(e) => eprintln!("[harvester] Error: {}", e),
            }
        }

        "install" => {
            if package.is_empty() {
                eprintln!("Usage: harvester install <package> [--force]");
                return;
            }
            println!("[harvester] Installing {}...", package);
            match install::install(package, force, &cfg) {
                Ok(_) => println!("[harvester] {} installed", package),
                Err(e) => eprintln!("[harvester] Error: {}", e),
            }
        }

        "remove" => {
            if package.is_empty() {
                eprintln!("Usage: harvester remove <package> [--purge] [--force]");
                return;
            }
            println!("[harvester] Removing {}...", package);
            match remove::remove(package, purge, force, &cfg) {
                Ok(_) => println!("[harvester] {} removed", package),
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
