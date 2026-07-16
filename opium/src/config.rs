// Osiris OS — Path Configuration
// Resolves runtime paths so no component ever hardcodes an environment.
// Works across Termux/proot, Framework, bare metal, and future targets.

use std::env;
use std::path::PathBuf;

/// The detected runtime environment
#[derive(Debug, Clone, PartialEq)]
pub enum OsirisEnv {
    Termux,       // Bare Termux on Android
    TermuxProot,  // Debian proot inside Termux
    Native,       // Bare metal / installed Osiris
    Framework,    // Framework laptop target
}

/// Central path config — resolved once at startup, passed everywhere
#[derive(Debug, Clone)]
pub struct OsirisConfig {
    pub env:          OsirisEnv,
    pub osiris_root:  PathBuf,
    pub debian_root:  PathBuf,
    pub pkg_cache:    PathBuf,
    pub pkg_db:       PathBuf,
    pub pkg_repo:     PathBuf,
    #[allow(dead_code)] // used by repo module in Stage 2
    pub pkg_index:    PathBuf,
    pub log_dir:      PathBuf,
}

impl OsirisConfig {
    /// Detect environment and resolve all paths
    pub fn resolve() -> Self {
        let env = detect_env();
        let osiris_root = resolve_osiris_root(&env);

        let pkg_base   = osiris_root.join("pkg");
        let pkg_cache  = pkg_base.join("cache");
        let pkg_db     = pkg_base.join("db").join("installed");
        let pkg_repo   = pkg_base.join("repo");
        let pkg_index  = pkg_repo.join("index.toml");
        let log_dir    = osiris_root.join("log");
        let debian_root = resolve_debian_root(&env);

        OsirisConfig {
            env,
            osiris_root,
            debian_root,
            pkg_cache,
            pkg_db,
            pkg_repo,
            pkg_index,
            log_dir,
        }
    }

    /// Ensure all critical directories exist
    pub fn init_dirs(&self) -> Result<(), String> {
        let dirs = [
            &self.pkg_cache,
            &self.pkg_db,
            &self.pkg_repo,
            &self.log_dir,
        ];
        for dir in &dirs {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;
        }
        Ok(())
    }

    /// Human readable environment name
    pub fn env_name(&self) -> &str {
        match self.env {
            OsirisEnv::Termux      => "Termux (bare)",
            OsirisEnv::TermuxProot => "Termux + Debian proot",
            OsirisEnv::Native      => "Osiris Native",
            OsirisEnv::Framework   => "Framework Laptop",
        }
    }
}

/// Detect which environment we're running in
fn detect_env() -> OsirisEnv {
    // Explicit override always wins
    if let Ok(env) = env::var("OSIRIS_ENV") {
        return match env.as_str() {
            "termux"       => OsirisEnv::Termux,
            "termux-proot" => OsirisEnv::TermuxProot,
            "native"       => OsirisEnv::Native,
            "framework"    => OsirisEnv::Framework,
            _              => OsirisEnv::Native,
        };
    }

    // Detect Termux by characteristic env var
    if env::var("TERMUX_VERSION").is_ok() {
        // Check if we're inside a proot by looking for debian markers
        if PathBuf::from("/etc/debian_version").exists()
            && env::var("PROOT_TMP_DIR").is_ok()
        {
            return OsirisEnv::TermuxProot;
        }
        return OsirisEnv::Termux;
    }

    // Check for Framework-specific DMI marker
    if PathBuf::from("/sys/class/dmi/id/board_vendor").exists() {
        if let Ok(vendor) = std::fs::read_to_string("/sys/class/dmi/id/board_vendor") {
            if vendor.trim().to_lowercase().contains("framework") {
                return OsirisEnv::Framework;
            }
        }
    }

    OsirisEnv::Native
}

/// Resolve the Osiris root directory for this environment
fn resolve_osiris_root(env: &OsirisEnv) -> PathBuf {
    // Explicit override
    if let Ok(root) = env::var("OSIRIS_ROOT") {
        return PathBuf::from(root);
    }

    match env {
        OsirisEnv::Termux | OsirisEnv::TermuxProot => {
            // Termux home + osiris-rootfs
            let termux_home = env::var("HOME")
                .unwrap_or_else(|_| "/data/data/com.termux/files/home".to_string());
            PathBuf::from(termux_home).join("osiris-rootfs").join("osiris")
        }
        OsirisEnv::Native | OsirisEnv::Framework => {
            PathBuf::from("/osiris")
        }
    }
}

/// Resolve the Debian proot root (Harvester needs this for harvesting)
fn resolve_debian_root(env: &OsirisEnv) -> PathBuf {
    if let Ok(root) = env::var("OSIRIS_DEBIAN_ROOT") {
        return PathBuf::from(root);
    }

    match env {
        OsirisEnv::Termux | OsirisEnv::TermuxProot => {
            let termux_home = env::var("HOME")
                .unwrap_or_else(|_| "/data/data/com.termux/files/home".to_string());
            // Standard proot-distro debian rootfs location
            PathBuf::from(termux_home)
                .join("../usr/var/lib/proot-distro/installed-rootfs/debian")
        }
        OsirisEnv::Native | OsirisEnv::Framework => {
            // On native Osiris, no Debian proot — Harvester uses system paths
            PathBuf::from("/")
        }
    }
}

/// Detect current architecture
pub fn detect_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "unknown"
    }
}
