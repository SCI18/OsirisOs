// Osiris Package Manifest
// Defines the structure of .osr and harvested packages
//
// Expansion pass 2026-07-17: adds per-file checksums (FileEntry), Scripts
// (pre/post install/remove hooks). Written as one complete, internally
// consistent rewrite — not an incremental append — specifically to avoid
// the duplicate-definition corruption from the previous expansion attempt.

use serde::{Deserialize, Serialize};

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

/// A single tracked file within a package: its path (relative to
/// osiris_root) and its blake3 checksum, computed at harvest time and
/// verified at install time before that file is trusted.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub path: String,
    /// Hex-encoded blake3 hash. Optional so older/manually-built manifests
    /// without checksums still deserialize — but install.rs treats a
    /// missing checksum as "cannot verify" and warns accordingly, rather
    /// than silently skipping verification.
    pub checksum: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct FileList {
    pub bin: Option<Vec<FileEntry>>,
    pub lib: Option<Vec<FileEntry>>,
    pub share: Option<Vec<FileEntry>>,
    pub etc: Option<Vec<FileEntry>>,
}

impl FileList {
    /// Flatten every tracked file across all categories — what
    /// install.rs/remove.rs actually need for verification and deletion,
    /// since the bin/lib/share/etc split is organizational only.
    pub fn all_entries(&self) -> Vec<&FileEntry> {
        let mut all = Vec::new();
        for group in [&self.bin, &self.lib, &self.share, &self.etc] {
            if let Some(entries) = group {
                all.extend(entries.iter());
            }
        }
        all
    }
}

/// Shell script hooks. Each field is literal script content (shebang +
/// commands), not a file path — install.rs/remove.rs write it to a temp
/// file and execute it.
///
/// SECURITY NOTE (flagged, not resolved here): executing arbitrary scripts
/// from a package is inherent to how dpkg/apt-style package managers work,
/// but it's a real trust boundary — a malicious or compromised package can
/// run anything the installing user can. No sandboxing is implemented in
/// this pass (deferred explicitly, per ph.md's "script sandbox" note).
/// Do not treat script execution as safe against untrusted packages
/// without that follow-up.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Scripts {
    pub pre_install: Option<String>,
    pub post_install: Option<String>,
    pub pre_remove: Option<String>,
    pub post_remove: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Manifest {
    pub package: PackageInfo,
    pub files: Option<FileList>,
    pub scripts: Option<Scripts>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub arch: String,
    pub description: Option<String>,
    pub depends: Option<Vec<String>>,
    pub source: Option<String>,
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
            scripts: None,
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

    pub fn all_file_entries(&self) -> Vec<&FileEntry> {
        self.files.as_ref().map(|f| f.all_entries()).unwrap_or_default()
    }
}

/// Compute a blake3 checksum for a file, hex-encoded.
/// Requires `blake3` as a dependency in harvester/Cargo.toml (blake3 = "1").
pub fn compute_checksum(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Could not read {} for checksum: {}", path.display(), e))?;
    let hash = blake3::hash(&bytes);
    Ok(hash.to_hex().to_string())
}
