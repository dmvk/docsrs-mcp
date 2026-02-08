use std::path::PathBuf;

/// On-disk cache for raw zstd-compressed rustdoc JSON bytes from docs.rs.
///
/// File layout: `{cache_dir}/docsrs-mcp/{crate_name}/{version}.json.zst`
///
/// All disk errors are non-fatal — logged as warnings and treated as cache misses.
pub struct DiskCache {
    base_dir: PathBuf,
}

impl DiskCache {
    /// Platform-appropriate cache base directory: `{cache_dir}/docsrs-mcp/`
    fn base_dir() -> Option<PathBuf> {
        Some(dirs::cache_dir()?.join("docsrs-mcp"))
    }

    /// Create a new DiskCache using the platform-appropriate cache directory.
    /// Returns `None` if no cache directory can be determined.
    pub fn new() -> Option<Self> {
        let base_dir = Self::base_dir()?;
        migrate_old_cache_dir(&base_dir);
        Some(Self { base_dir })
    }

    #[cfg(test)]
    fn with_base_dir(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Read cached raw bytes for a crate version. Returns `None` on miss or error.
    pub async fn read(&self, crate_name: &str, version: &str) -> Option<Vec<u8>> {
        let path = self.cache_path(crate_name, version);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                tracing::info!("Disk cache hit for {crate_name} v{version}");
                Some(bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                tracing::warn!("Disk cache read failed for {crate_name} v{version}: {e}");
                None
            }
        }
    }

    /// Write raw bytes to cache using temp-file-then-rename for atomicity.
    pub async fn write(&self, crate_name: &str, version: &str, bytes: &[u8]) {
        let path = self.cache_path(crate_name, version);

        let Some(parent) = path.parent() else {
            return;
        };

        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!("Failed to create cache dir {}: {e}", parent.display());
            return;
        }

        // Write to a temp file in the same directory, then rename for atomicity
        let tmp_path = path.with_extension("tmp");
        if let Err(e) = tokio::fs::write(&tmp_path, bytes).await {
            tracing::warn!("Failed to write cache file {}: {e}", tmp_path.display());
            return;
        }

        if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
            tracing::warn!("Failed to rename cache file: {e}");
            // Clean up the temp file on failure
            let _ = tokio::fs::remove_file(&tmp_path).await;
        } else {
            tracing::info!("Cached {crate_name} v{version} to disk");
        }
    }

    /// Remove a corrupted cache entry.
    pub async fn remove(&self, crate_name: &str, version: &str) {
        let path = self.cache_path(crate_name, version);
        if let Err(e) = tokio::fs::remove_file(&path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("Failed to remove cache entry {}: {e}", path.display());
        }
    }

    /// Delete the entire cache directory.
    pub async fn clear() {
        let Some(base_dir) = Self::base_dir() else {
            return;
        };
        match tokio::fs::remove_dir_all(&base_dir).await {
            Ok(()) => tracing::info!("Cleared disk cache at {}", base_dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("Failed to clear disk cache: {e}"),
        }
    }

    fn cache_path(&self, crate_name: &str, version: &str) -> PathBuf {
        self.base_dir
            .join(sanitize_path_component(crate_name))
            .join(format!("{}.json.zst", sanitize_path_component(version)))
    }
}

/// One-time migration: rename the old `rust-docs-mcp` cache directory to `docsrs-mcp`.
/// Only acts when the old directory exists and the new one does not.
fn migrate_old_cache_dir(new_base: &std::path::Path) {
    let Some(old_base) = new_base.parent().map(|p| p.join("rust-docs-mcp")) else {
        return;
    };
    if old_base.is_dir() && !new_base.exists() {
        match std::fs::rename(&old_base, new_base) {
            Ok(()) => tracing::info!(
                "Migrated disk cache from {} to {}",
                old_base.display(),
                new_base.display()
            ),
            Err(e) => tracing::warn!(
                "Failed to migrate cache from {} to {}: {e}",
                old_base.display(),
                new_base.display()
            ),
        }
    }
}

/// Sanitize a string for use as a single path component.
/// Rejects path separators and traversal sequences to prevent directory escape.
fn sanitize_path_component(s: &str) -> &str {
    // Reject anything that could escape the cache directory
    if s.is_empty()
        || s == "."
        || s == ".."
        || s.contains('/')
        || s.contains('\\')
        || s.contains('\0')
    {
        "_invalid"
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== sanitize_path_component tests ==========

    #[test]
    fn sanitize_allows_normal_crate_names() {
        assert_eq!(sanitize_path_component("serde"), "serde");
        assert_eq!(sanitize_path_component("tokio-rt"), "tokio-rt");
        assert_eq!(sanitize_path_component("my_crate"), "my_crate");
    }

    #[test]
    fn sanitize_allows_normal_versions() {
        assert_eq!(sanitize_path_component("1.0.0"), "1.0.0");
        assert_eq!(sanitize_path_component("0.12.3-alpha.1"), "0.12.3-alpha.1");
    }

    #[test]
    fn sanitize_rejects_path_traversal() {
        assert_eq!(sanitize_path_component(".."), "_invalid");
        assert_eq!(sanitize_path_component("../../../etc"), "_invalid");
        assert_eq!(sanitize_path_component("foo/bar"), "_invalid");
        assert_eq!(sanitize_path_component("foo\\bar"), "_invalid");
    }

    #[test]
    fn sanitize_rejects_empty_and_dot() {
        assert_eq!(sanitize_path_component(""), "_invalid");
        assert_eq!(sanitize_path_component("."), "_invalid");
    }

    #[test]
    fn sanitize_rejects_null_bytes() {
        assert_eq!(sanitize_path_component("foo\0bar"), "_invalid");
    }

    // ========== cache_path layout tests ==========

    #[test]
    fn cache_path_has_expected_structure() {
        let cache = DiskCache::with_base_dir(PathBuf::from("/tmp/test-cache"));
        let path = cache.cache_path("serde", "1.0.200");
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-cache/serde/1.0.200.json.zst")
        );
    }

    #[test]
    fn cache_path_sanitizes_traversal_in_crate_name() {
        let cache = DiskCache::with_base_dir(PathBuf::from("/tmp/test-cache"));
        let path = cache.cache_path("../../etc", "1.0.0");
        // Should use "_invalid" instead of the traversal path
        assert!(path.starts_with("/tmp/test-cache/_invalid"));
        // Must not escape base dir
        assert!(!path.to_string_lossy().contains("../../"));
    }

    #[test]
    fn cache_path_sanitizes_traversal_in_version() {
        let cache = DiskCache::with_base_dir(PathBuf::from("/tmp/test-cache"));
        let path = cache.cache_path("serde", "../../../etc/passwd");
        assert!(path.starts_with("/tmp/test-cache/serde"));
        assert!(!path.to_string_lossy().contains("../"));
    }

    // ========== DiskCache read/write/remove integration tests ==========

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        let data = b"fake zstd compressed data";
        cache.write("my-crate", "1.0.0", data).await;

        let result = cache.read("my-crate", "1.0.0").await;
        assert_eq!(result.as_deref(), Some(data.as_slice()));
    }

    #[tokio::test]
    async fn read_miss_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        assert!(cache.read("nonexistent", "0.0.0").await.is_none());
    }

    #[tokio::test]
    async fn write_creates_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        cache.write("deeply-nested-crate", "2.0.0", b"bytes").await;

        // The crate subdirectory should have been created
        let expected_dir = dir.path().join("deeply-nested-crate");
        assert!(expected_dir.is_dir());
    }

    #[tokio::test]
    async fn write_is_atomic_no_tmp_file_remains() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        cache.write("test-crate", "1.0.0", b"data").await;

        // After successful write, no .tmp file should remain
        let tmp_path = dir.path().join("test-crate").join("1.0.0.tmp");
        assert!(!tmp_path.exists());

        // The actual file should exist
        let cache_path = dir.path().join("test-crate").join("1.0.0.json.zst");
        assert!(cache_path.exists());
    }

    #[tokio::test]
    async fn remove_deletes_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        cache.write("my-crate", "1.0.0", b"data").await;
        assert!(cache.read("my-crate", "1.0.0").await.is_some());

        cache.remove("my-crate", "1.0.0").await;
        assert!(cache.read("my-crate", "1.0.0").await.is_none());
    }

    #[tokio::test]
    async fn remove_nonexistent_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        // Should not panic or error
        cache.remove("nonexistent", "0.0.0").await;
    }

    #[tokio::test]
    async fn write_overwrite_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        cache.write("my-crate", "1.0.0", b"old data").await;
        cache.write("my-crate", "1.0.0", b"new data").await;

        let result = cache.read("my-crate", "1.0.0").await;
        assert_eq!(result.as_deref(), Some(b"new data".as_slice()));
    }

    #[tokio::test]
    async fn different_versions_are_separate_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::with_base_dir(dir.path().to_path_buf());

        cache.write("serde", "1.0.0", b"v1 data").await;
        cache.write("serde", "2.0.0", b"v2 data").await;

        assert_eq!(
            cache.read("serde", "1.0.0").await.as_deref(),
            Some(b"v1 data".as_slice())
        );
        assert_eq!(
            cache.read("serde", "2.0.0").await.as_deref(),
            Some(b"v2 data".as_slice())
        );
    }

    // ========== migrate_old_cache_dir tests ==========

    #[test]
    fn migrate_renames_old_dir_to_new() {
        let parent = tempfile::tempdir().unwrap();
        let old_dir = parent.path().join("rust-docs-mcp");
        let new_dir = parent.path().join("docsrs-mcp");

        std::fs::create_dir(&old_dir).unwrap();
        std::fs::write(old_dir.join("data.txt"), b"cached").unwrap();

        migrate_old_cache_dir(&new_dir);

        assert!(!old_dir.exists(), "old dir should be gone after migration");
        assert!(new_dir.exists(), "new dir should exist after migration");
        assert_eq!(std::fs::read(new_dir.join("data.txt")).unwrap(), b"cached");
    }

    #[test]
    fn migrate_skips_when_new_dir_already_exists() {
        let parent = tempfile::tempdir().unwrap();
        let old_dir = parent.path().join("rust-docs-mcp");
        let new_dir = parent.path().join("docsrs-mcp");

        std::fs::create_dir(&old_dir).unwrap();
        std::fs::write(old_dir.join("old.txt"), b"old").unwrap();
        std::fs::create_dir(&new_dir).unwrap();
        std::fs::write(new_dir.join("new.txt"), b"new").unwrap();

        migrate_old_cache_dir(&new_dir);

        // Both directories should remain untouched
        assert!(
            old_dir.exists(),
            "old dir should remain when new dir exists"
        );
        assert_eq!(std::fs::read(new_dir.join("new.txt")).unwrap(), b"new");
    }

    #[test]
    fn migrate_noop_when_no_old_dir() {
        let parent = tempfile::tempdir().unwrap();
        let new_dir = parent.path().join("docsrs-mcp");

        migrate_old_cache_dir(&new_dir);

        assert!(
            !new_dir.exists(),
            "new dir should not be created from nothing"
        );
    }
}
