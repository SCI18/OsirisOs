// Osiris Package Manifest
// Defines the structure of .osr and harvested packages

use serde::{Deserialize, Serialize};

/// Detect current architecture locally
/// Duplicated from config.rs intentionally —
/// manifest.rs is shared between opium and harvester
/// and should not create a cross-crate dependency
fn detect_arch() -> &'static str {
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

/// Package source — where did this package come from
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PackageSource {
    Osiris,      // Native .osr package from official Osiris repo
    Community,   // Community submitted .osr package
    Harvester,   // Harvested from Debian proot
    Local,       // Manually installed from local .osr file
}

impl PackageSource {
    pub fn as_str(&self) -> &str {
        match self {
            PackageSource::Osiris     => "osiris",
            PackageSource::Community  => "community",
            PackageSource::Harvester  => "harvester",
            PackageSource::Local      => "local",
        }
    }
}

impl std::fmt::Display for PackageSource {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub package: PackageInfo,
    pub files:   Option<FileList>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackageInfo {
    pub name:        String,
    pub version:     String,
    pub arch:        String,
    pub description: Option<String>,
    pub depends:     Option<Vec<String>>,
    pub source:      Option<String>, // stored as string for TOML simplicity
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileList {
    pub bin:   Option<Vec<String>>,
    pub lib:   Option<Vec<String>>,
    pub share: Option<Vec<String>>,
    pub etc:   Option<Vec<String>>,
}

impl Manifest {
    /// Create a new manifest — arch detected at runtime, not hardcoded
    pub fn new(name: &str, version: &str) -> Self {
        Manifest {
            package: PackageInfo {
                name:        name.to_string(),
                version:     version.to_string(),
                arch:        detect_arch().to_string(),
                description: None,
                depends:     None,
                source:      None,
            },
            files: None,
        }
    }

    /// Create a manifest with a known source
    pub fn new_with_source(name: &str, version: &str, source: PackageSource) -> Self {
        let mut m = Self::new(name, version);
        m.package.source = Some(source.to_string());
        m
    }

    /// Serialize to TOML string
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Deserialize from TOML string
    pub fn from_toml(content: &str) -> Result<Self, String> {
        toml::from_str(content).map_err(|e| e.to_string())
    }

    /// Read manifest from a .osr package directory or file
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read manifest at {}: {}", path.display(), e))?;
        Self::from_toml(&content)
    }

    /// Get source as a display string safely
    pub fn source_display(&self) -> &str {
        self.package.source.as_deref().unwrap_or("unknown")
    }
}
