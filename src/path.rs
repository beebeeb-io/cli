//! Shared path resolution: plaintext paths → encrypted file UUIDs.
//!
//! Commands like `ls`, `pull`, `push`, `sync`, and WebDAV all need to resolve
//! human-readable paths ("Music/Old") to the server's encrypted file IDs.
//! This module centralises that logic with an LRU-style cache (30 s TTL) to
//! avoid hammering the API during PROPFIND storms or tab-completion bursts.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use beebeeb_core::kdf::MasterKey;
use serde_json::Value;

use crate::api::ApiClient;

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// Cached `list_files` response with a timestamp.
struct CacheEntry {
    value: Value,
    fetched_at: Instant,
}

/// Global directory-listing cache keyed by `parent_id` (empty string = root).
static DIR_CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// How long a cached listing stays valid.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// Fetch (or return cached) directory listing for `parent_id`.
async fn list_files_cached(api: &ApiClient, parent_id: Option<&str>) -> Result<Value, String> {
    let key = parent_id.unwrap_or("").to_string();

    // Check cache first.
    {
        let cache = DIR_CACHE.lock().unwrap();
        if let Some(entry) = cache.get(&key) {
            if entry.fetched_at.elapsed() < CACHE_TTL {
                return Ok(entry.value.clone());
            }
        }
    }

    // Cache miss or stale — fetch from API.
    let value = api.list_files(parent_id).await?;

    {
        let mut cache = DIR_CACHE.lock().unwrap();
        cache.insert(
            key,
            CacheEntry {
                value: value.clone(),
                fetched_at: Instant::now(),
            },
        );
    }

    Ok(value)
}

/// Drop every cached entry. Call after mutations (upload, mkdir, move, delete).
pub fn invalidate_cache() {
    DIR_CACHE.lock().unwrap().clear();
}

// ---------------------------------------------------------------------------
// Percent-decoding (WebDAV compat)
// ---------------------------------------------------------------------------

/// Decode percent-encoded UTF-8 sequences (`%20` → ` `, `%C3%A9` → `é`, etc.).
///
/// Invalid sequences are passed through verbatim so the function never fails.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|e| {
        // Best-effort: lossy conversion if percent-encoded bytes aren't valid UTF-8.
        String::from_utf8_lossy(e.as_bytes()).into_owned()
    })
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// The result of resolving a plaintext path against the encrypted vault tree.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// Server file UUID. `None` means the vault root (has no ID).
    pub file_id: Option<String>,
    /// Decrypted display name of the final segment (or `"/"` for root).
    pub name: String,
    /// Whether the resolved entry is a folder.
    pub is_folder: bool,
}

/// Resolve a human-readable slash-separated path to an encrypted file entry.
///
/// - Empty / all-slashes path → vault root.
/// - Each segment is percent-decoded, then case-insensitively matched against
///   decrypted children of the current parent.
/// - Returns `Err` if any segment cannot be found.
pub async fn resolve_path(api: &ApiClient, master_key: &MasterKey, path: &str) -> Result<ResolvedPath, String> {
    let trimmed = path.trim_matches('/');

    // Empty path → root.
    if trimmed.is_empty() {
        return Ok(ResolvedPath {
            file_id: None,
            name: "/".to_string(),
            is_folder: true,
        });
    }

    let segments: Vec<&str> = trimmed.split('/').collect();

    let mut current = ResolvedPath {
        file_id: None,
        name: "/".to_string(),
        is_folder: true,
    };

    for segment in &segments {
        let decoded = percent_decode(segment);

        if decoded.is_empty() {
            continue; // skip double-slashes
        }

        let listing = list_files_cached(api, current.file_id.as_deref()).await?;
        let files = listing
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "unexpected API response: missing files array".to_string())?;

        let mut found = false;

        for entry in files {
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let name_enc = entry.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or_default();
            let is_folder = entry.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);

            let decrypted =
                crate::crypto::decrypt_name(master_key, id, name_enc).unwrap_or_else(|| name_enc.to_string());

            if decrypted.eq_ignore_ascii_case(&decoded) {
                current = ResolvedPath {
                    file_id: Some(id.to_string()),
                    name: decrypted,
                    is_folder,
                };
                found = true;
                break;
            }
        }

        if !found {
            return Err(format!("not found: \"{decoded}\" in {}", current.name));
        }
    }

    Ok(current)
}

/// Look up an immediate child by plaintext name. Used by `mkdir` to detect a
/// pre-existing folder or file before POSTing. Returns `None` when there is no
/// match; returns an error only if the listing itself fails. Matching is
/// case-insensitive, consistent with `resolve_path`.
pub async fn find_child_by_name(
    api: &ApiClient,
    master_key: &MasterKey,
    parent_id: Option<&str>,
    name: &str,
) -> Result<Option<ResolvedPath>, String> {
    let listing = list_files_cached(api, parent_id).await?;
    let files = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "unexpected API response: missing files array".to_string())?;

    for entry in files {
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        let name_enc = entry.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or_default();
        let is_folder = entry.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);

        let decrypted = crate::crypto::decrypt_name(master_key, id, name_enc).unwrap_or_else(|| name_enc.to_string());

        if decrypted.eq_ignore_ascii_case(name) {
            return Ok(Some(ResolvedPath {
                file_id: Some(id.to_string()),
                name: decrypted,
                is_folder,
            }));
        }
    }

    Ok(None)
}
