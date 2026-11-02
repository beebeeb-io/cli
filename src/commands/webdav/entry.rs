//! `entry` items split out of `webdav.rs`.

use super::*;

/// SF-0002: the one write check for a PUT-overwrite or a rename of `entry`.
/// `Err` is the response: 403 for a read-only row, 503 when the check could not
/// decide (retryable; the row is not marked read-only).
pub(super) async fn write_refusal(state: &Arc<DavState>, entry: &ResolvedEntry) -> Result<(), Response> {
    let Some(file_id) = entry.file_id.as_deref() else {
        return Ok(());
    };
    let target = crate::owned_keys::WriteTarget {
        file_id,
        is_folder: entry.is_collection,
        name_read_only: entry.read_only,
        // Rows here open under the owner's own conventions (the master key).
        row_key: None,
        size_bytes: entry.size_bytes,
        chunk_count: entry.chunk_count,
    };
    match crate::owned_keys::write_check(&state.api, &state.master_key, &target).await {
        crate::owned_keys::WriteCheck::Writable => Ok(()),
        crate::owned_keys::WriteCheck::UpperRepair => {
            Err((StatusCode::FORBIDDEN, crate::crypto::OWNER_REPAIR_READ_ONLY).into_response())
        }
        crate::owned_keys::WriteCheck::Unknown => {
            Err((StatusCode::SERVICE_UNAVAILABLE, crate::owned_keys::WRITE_CHECK_UNKNOWN).into_response())
        }
    }
}

/// A resolved vault entry (file or collection).
#[derive(Clone)]
pub(super) struct ResolvedEntry {
    pub(super) file_id: Option<String>,
    pub(super) display_name: String,
    pub(super) is_collection: bool,
    pub(super) size_bytes: Option<u64>,
    pub(super) chunk_count: u32,
    pub(super) modified: Option<String>,
    /// The content's read pin from the file's authenticated metadata
    /// (`crypto::content_pin`, task 1943). `Err` for a collection, or for a file
    /// whose metadata cannot be read (its content is then refused, never guessed).
    pub(super) pin: Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String>,
    /// SF-0002: the name resolved under the read-only uppercase repair
    /// convention. PUT-overwrite and rename are refused; move-only is allowed.
    pub(super) read_only: bool,
}

/// What a collection carries in [`ResolvedEntry::pin`].
pub(super) fn no_content() -> Result<beebeeb_core::chunk_stream::ExpectedChunkFormat, String> {
    Err("a collection has no content".to_string())
}

/// Decode a single API file object into a `ResolvedEntry`.
pub(super) fn decode_file_entry(
    file: &serde_json::Value,
    master_key: &beebeeb_core::kdf::MasterKey,
    _parent_href: &str,
) -> Option<ResolvedEntry> {
    let file_id = file.get("id")?.as_str()?.to_string();
    let is_folder = file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
    let size_bytes = file.get("size_bytes").and_then(|v| v.as_u64());
    let chunk_count = file.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let modified = file.get("updated_at").and_then(|v| v.as_str()).map(|s| s.to_string());

    let name_encrypted = file.get("name_encrypted").and_then(|v| v.as_str())?;
    let (display_name, read_only) =
        match crate::crypto::decrypt_name_with_convention(master_key, &file_id, name_encrypted) {
            Some((name, convention)) => (name, convention == beebeeb_core::kdf::OwnerKeyConvention::UpperRepair),
            None => (format!("[{}]", &file_id[..8]), false),
        };
    let pin = if is_folder {
        no_content()
    } else {
        crate::crypto::content_pin(master_key, &file_id, name_encrypted).map_err(String::from)
    };

    Some(ResolvedEntry {
        file_id: Some(file_id),
        display_name,
        is_collection: is_folder,
        size_bytes,
        chunk_count,
        modified,
        pin,
        read_only,
    })
}

/// Resolve a WebDAV path like `/Documents/2025/report.pdf` to a vault entry.
///
/// Walks the vault tree one segment at a time, decrypting folder names at each
/// level to find the matching child. O(depth × breadth) — no cache yet (Day 4).
pub(super) async fn resolve_path(state: &Arc<DavState>, path: &str) -> Result<ResolvedEntry, String> {
    // Normalise: strip leading slash, trim trailing slash
    let trimmed = path.trim_start_matches('/').trim_end_matches('/');

    if trimmed.is_empty() {
        // Root
        return Ok(ResolvedEntry {
            file_id: None,
            display_name: "vault".to_string(),
            is_collection: true,
            size_bytes: None,
            chunk_count: 0,
            modified: None,
            pin: no_content(),
            read_only: false,
        });
    }

    let segments: Vec<&str> = trimmed.split('/').collect();
    let mut current_parent: Option<String> = None; // None = root

    for (i, segment) in segments.iter().enumerate() {
        let is_last = i == segments.len() - 1;

        // List children of current_parent
        let resp = state
            .api
            .list_files(current_parent.as_deref())
            .await
            .map_err(|e| format!("list failed: {e}"))?;

        let files = resp
            .get("files")
            .and_then(|f| f.as_array())
            .ok_or("malformed files response")?;

        // Find the child whose decrypted name matches the path segment
        let mut matched: Option<ResolvedEntry> = None;
        for file in files {
            if let Some(entry) = decode_file_entry(file, &state.master_key, "") {
                let decoded_segment = crate::path::percent_decode(segment);
                if entry.display_name.to_lowercase() == decoded_segment.to_lowercase() {
                    matched = Some(entry);
                    break;
                }
            }
        }

        let entry = matched.ok_or_else(|| format!("'{segment}' not found"))?;

        if is_last {
            return Ok(entry);
        }

        // Must be a folder to descend into
        if !entry.is_collection {
            return Err(format!("'{segment}' is a file, not a folder"));
        }
        current_parent = entry.file_id;
    }

    Err("empty path after trimming".to_string())
}

pub(super) fn child_href(parent: &str, name: &str, is_dir: bool) -> String {
    let parent = parent.trim_end_matches('/');
    let suffix = if is_dir { "/" } else { "" };
    let encoded = percent_encode(name.as_bytes(), NON_ALPHANUMERIC).to_string();
    format!("{parent}/{encoded}{suffix}")
}

pub(super) fn prop_entry(href: &str, entry: &ResolvedEntry) -> String {
    let display_name = xml_escape(&entry.display_name);
    let resource_type = if entry.is_collection {
        "<D:resourcetype><D:collection/></D:resourcetype>".to_string()
    } else {
        "<D:resourcetype/>".to_string()
    };

    let content_length = entry
        .size_bytes
        .map(|s| format!("<D:getcontentlength>{s}</D:getcontentlength>"))
        .unwrap_or_default();

    let last_modified = entry
        .modified
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| {
            format!(
                "<D:getlastmodified>{}</D:getlastmodified>",
                dt.format("%a, %d %b %Y %H:%M:%S GMT")
            )
        })
        .unwrap_or_default();

    // ETag for PROPFIND responses (helps clients detect modifications)
    let etag_prop = entry
        .file_id
        .as_deref()
        .map(|fid| {
            let etag = make_etag(fid, entry.modified.as_deref());
            format!("<D:getetag>{}</D:getetag>", xml_escape(&etag))
        })
        .unwrap_or_default();

    let content_type = if !entry.is_collection {
        let mime = resolve_mime(&entry.display_name);
        format!("<D:getcontenttype>{mime}</D:getcontenttype>")
    } else {
        "<D:getcontenttype>httpd/unix-directory</D:getcontenttype>".to_string()
    };

    let href_escaped = xml_escape(href);

    format!(
        "  <D:response>\n\
             <D:href>{href_escaped}</D:href>\n\
             <D:propstat>\n\
               <D:prop>\n\
                 <D:displayname>{display_name}</D:displayname>\n\
                 {resource_type}\n\
                 {content_length}\n\
                 {last_modified}\n\
                 {etag_prop}\n\
                 {content_type}\n\
               </D:prop>\n\
               <D:status>HTTP/1.1 200 OK</D:status>\n\
             </D:propstat>\n\
           </D:response>"
    )
}

pub(super) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Resolve a MIME type for a filename using the mime_guess crate.
/// Falls back to application/octet-stream for unknown extensions.
pub(super) fn resolve_mime(filename: &str) -> String {
    mime_guess::from_path(filename).first_or_octet_stream().to_string()
}
