//! `cache` items split out of `webdav.rs`.

use super::*;

/// One cached directory listing.
#[derive(Clone)]
pub(super) struct CachedDir {
    /// Decoded + decrypted entries for this directory.
    pub(super) children: Vec<ResolvedEntry>,
    /// When this entry was populated.
    pub(super) cached_at: Instant,
}

/// Global directory-listing cache keyed by parent path (e.g. `"/"`, `"/Documents"`).
pub(super) type DirCache = Mutex<HashMap<String, CachedDir>>;

/// Check whether a WebDAV path targets a macOS/Windows metadata file that
/// should never be stored in the vault.
pub(super) fn is_os_metadata_path(path: &str) -> bool {
    let filename = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    if filename.is_empty() {
        return false;
    }
    matches!(
        filename,
        ".DS_Store"
            | ".Spotlight-V100"
            | ".Trashes"
            | "Thumbs.db"
            | "desktop.ini"
            | ".hidden"
            | ".metadata_never_index"
            | ".metadata_never_index_unless_rootfs"
            | ".metadata_direct_scope_only"
            | ".ql_disablethumbnails"
            | ".ql_disablecache"
    ) || filename.starts_with("._")
}

pub(super) fn normalise_cache_key(path: &str) -> String {
    let t = path.trim_end_matches('/');
    if t.is_empty() { "/".to_string() } else { t.to_string() }
}

pub(super) fn parent_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/",
        Some(i) => &trimmed[..i],
        None => "/",
    }
}

pub(super) async fn get_from_cache(state: &DavState, key: &str) -> Option<Vec<ResolvedEntry>> {
    if state.cache_ttl.is_zero() {
        return None;
    }
    let cache = state.dir_cache.lock().await;
    if let Some(entry) = cache.get(key) {
        if entry.cached_at.elapsed() < state.cache_ttl {
            return Some(entry.children.clone());
        }
    }
    None
}

pub(super) async fn store_in_cache(state: &DavState, key: String, children: Vec<ResolvedEntry>) {
    if state.cache_ttl.is_zero() {
        return;
    }
    let mut cache = state.dir_cache.lock().await;
    cache.insert(
        key,
        CachedDir {
            children,
            cached_at: Instant::now(),
        },
    );
}

pub(super) async fn invalidate_cache(state: &DavState, key: &str) {
    let mut cache = state.dir_cache.lock().await;
    cache.remove(key);
}

/// Produce a quoted ETag string for a file.
///
/// Format: `"<file_id>-<modified_epoch>"` — changes on every overwrite
/// (new file_id) and also when modified_at changes.
pub(super) fn make_etag(file_id: &str, modified: Option<&str>) -> String {
    let suffix = modified
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp().to_string())
        .unwrap_or_default();
    if suffix.is_empty() {
        format!("\"{file_id}\"")
    } else {
        format!("\"{file_id}-{suffix}\"")
    }
}
