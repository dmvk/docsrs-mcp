use cargo_lock::Lockfile;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parsed Cargo.lock data with fast crate name → version lookup.
pub struct CargoLockIndex {
    /// Map from crate name to version string (e.g. "1.0.210").
    /// If multiple versions exist, keeps the latest.
    versions: HashMap<String, String>,
}

impl CargoLockIndex {
    /// Walk up from `start_dir` looking for Cargo.lock, then parse it.
    pub fn find_and_parse(start_dir: &Path) -> Option<Self> {
        let lock_path = find_cargo_lock(start_dir)?;
        tracing::info!("Found Cargo.lock at {}", lock_path.display());
        Self::from_path(&lock_path).ok()
    }

    /// Parse a Cargo.lock file at the given path.
    pub fn from_path(path: &Path) -> Result<Self, crate::error::Error> {
        let lockfile = Lockfile::load(path)?;
        let mut versions = HashMap::new();

        for package in &lockfile.packages {
            let name = package.name.as_str().to_string();
            let version = package.version.to_string();
            // If multiple versions of the same crate exist, keep the latest
            versions
                .entry(name)
                .and_modify(|existing: &mut String| {
                    if version > *existing {
                        *existing = version.clone();
                    }
                })
                .or_insert(version);
        }

        Ok(Self { versions })
    }

    /// Look up the version of a crate.
    pub fn get_version(&self, crate_name: &str) -> Option<&str> {
        self.versions.get(crate_name).map(|s| s.as_str())
    }
}

/// Walk up the directory tree looking for Cargo.lock.
fn find_cargo_lock(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("Cargo.lock");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}
