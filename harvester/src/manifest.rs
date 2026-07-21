// Osiris Package Manifest
// Defines the structure of .osr and harvested packages
//
// Scope note: this is deliberately the minimal, dict3.md-scoped version —
// name/version/arch/description/depends/source/files only. Checksums,
// pre/post-install Scripts, and per-file FileEntry metadata are a
// deliberate follow-up expansion, not included here, to keep this a clean,
// verified checkpoint before that scope increase.

use serde::{Deserialize, Serialize};

/// Detect current architecture locally.
/// Duplicated from config.rs intentionally — manifest.rs is shared between
/// opium and harvester and should not create a cross-crate dependency.
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
    Osiris,
    Community,
    Harvester,
    Local,
}

impl PackageSource {
    pub fn as_str(&self) -> &str {
        match self {
            PackageSource::Osiris => "osiris",
            PackageSource::Community => "community",
            PackageSource::Harvester => "harvester",
            PackageSource::Local => "local",
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
    pub files: Option<FileList>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub arch: String,
    pub description: Option<String>,
    /// FIX (dict3.md #4): now actually populated by harvest.rs going
    /// forward, rather than existing unused.
    pub depends: Option<Vec<String>>,
    pub source: Option<String>,
}

/// FIX (dict3.md #2): file list needed for real removal. Kept as simple
/// String paths for now (relative to osiris_root) — richer per-file
/// metadata (checksums, individual file entries) is the planned follow-up
/// expansion, not included in this checkpoint.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct FileList {
    pub bin: Option<Vec<String>>,
    pub lib: Option<Vec<String>>,
    pub share: Option<Vec<String>>,
    pub etc: Option<Vec<String>>,
}

impl Manifest {
    pub fn new(name: &str, version: &str) -> Self {
        Manifest {
            package: PackageInfo {
                name: name.to_string(),
                version: version.to_string(),
                arch: detect_arch().to_string(),
                description: None,
                depends: None,
                source: None,
            },
            files: None,
        }
    }

    pub fn new_with_source(name: &str, version: &str, source: PackageSource) -> Self {
        let mut m = Self::new(name, version);
        m.package.source = Some(source.to_string());
        m
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    pub fn from_toml(content: &str) -> Result<Self, String> {
        toml::from_str(content).map_err(|e| e.to_string())
    }

    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read manifest at {}: {}", path.display(), e))?;
        Self::from_toml(&content)
    }

    pub fn source_display(&self) -> &str {
        self.package.source.as_deref().unwrap_or("unknown")
    }
}
