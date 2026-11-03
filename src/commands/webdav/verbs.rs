//! `verbs` items split out of `webdav.rs`.

use super::*;

pub(super) fn options_response(state: &DavState) -> Response {
    let mut headers = HeaderMap::new();
    // DAV: 1, 2 — class 2 requires LOCK/UNLOCK, which we stub
    headers.insert("DAV", "1, 2".parse().unwrap());
    let allow = if state.read_only {
        "OPTIONS, GET, HEAD, PROPFIND, LOCK, UNLOCK"
    } else {
        "OPTIONS, GET, HEAD, PROPFIND, PUT, DELETE, MKCOL, MOVE, LOCK, UNLOCK"
    };
    headers.insert("Allow", allow.parse().unwrap());
    // MS-Author-Via tells some clients (Office, SharePoint) this is WebDAV
    headers.insert("MS-Author-Via", "DAV".parse().unwrap());
    (StatusCode::OK, headers, "").into_response()
}

pub(super) async fn propfind_response(state: &Arc<DavState>, path: &str, depth: &str) -> Response {
    let resolved = match resolve_path(state, path).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  PROPFIND {path}: {e}");
            return (StatusCode::NOT_FOUND, format!("not found: {e}")).into_response();
        }
    };

    let mut xml_entries: Vec<String> = Vec::new();

    // Always include the resource itself
    xml_entries.push(prop_entry(path, &resolved));

    if depth == "1" && resolved.is_collection {
        let parent_id = resolved.file_id.as_deref();
        let cache_key = normalise_cache_key(path);

        // A cache hit skips the server round-trip; a miss falls through.
        let cached_children = get_from_cache(state, &cache_key).await;

        let children = if let Some(entries) = cached_children {
            entries
        } else {
            // Cache miss — fetch from API and populate
            let entries = match state.api.list_files(parent_id).await {
                Ok(resp) => {
                    let files = resp
                        .get("files")
                        .and_then(|f| f.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let mut requests =
                        crate::commands::request::RequestKeyResolver::load(&state.api).await.ok();
                    files
                        .iter()
                        .filter_map(|f| decode_file_entry(f, &state.master_key, path, requests.as_mut()))
                        .collect::<Vec<_>>()
                }
                Err(e) => {
                    eprintln!("  PROPFIND list error: {e}");
                    vec![]
                }
            };
            store_in_cache(state, cache_key, entries.clone()).await;
            entries
        };

        for entry in &children {
            xml_entries.push(prop_entry(
                &child_href(path, &entry.display_name, entry.is_collection),
                entry,
            ));
        }
    }

    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <D:multistatus xmlns:D=\"DAV:\">\n\
         {}\n\
         </D:multistatus>",
        xml_entries.join("\n")
    );

    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", "application/xml; charset=utf-8".parse().unwrap());
    (StatusCode::MULTI_STATUS, headers, body).into_response()
}

pub(super) async fn get_response(state: &Arc<DavState>, path: &str, req_headers: &HeaderMap) -> Response {
    let resolved = match resolve_path(state, path).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };

    if resolved.is_collection {
        return (StatusCode::METHOD_NOT_ALLOWED, "is a directory").into_response();
    }

    let file_id = match &resolved.file_id {
        Some(id) => id.clone(),
        None => return (StatusCode::NOT_FOUND, "no file id").into_response(),
    };

    let etag = make_etag(&file_id, resolved.modified.as_deref());

    if let Some(inm) = req_headers.get("if-none-match").and_then(|v| v.to_str().ok()) {
        if inm == etag || inm == "*" {
            let mut h = HeaderMap::new();
            h.insert("ETag", etag.parse().unwrap());
            return (StatusCode::NOT_MODIFIED, h, "").into_response();
        }
    }

    // If-Modified-Since (RFC 7232)
    let modified_dt = resolved
        .modified
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    if let Some(ims_val) = req_headers.get("if-modified-since").and_then(|v| v.to_str().ok()) {
        if let Some(ref mdt) = modified_dt {
            if let Ok(ims_dt) = chrono::DateTime::parse_from_rfc2822(ims_val) {
                if mdt <= &ims_dt {
                    let mut h = HeaderMap::new();
                    h.insert("ETag", etag.parse().unwrap());
                    return (StatusCode::NOT_MODIFIED, h, "").into_response();
                }
            }
        }
    }

    // The read pin from the file's authenticated metadata (task 1943): refuse
    // before downloading anything if it could not be read.
    let pin = match &resolved.pin {
        Ok(pin) => *pin,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.clone()).into_response(),
    };

    let encrypted_bytes = match state.api.download_file(&file_id).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response(),
    };

    // Decrypt all chunks using the shared helper that handles both
    // CLI-format (JSON) and web-app-format (raw binary) chunks, and
    // both UUID key derivation methods (binary and string).
    let plaintext = match crate::crypto::decrypt_file_chunks(
        &state.master_key,
        &file_id,
        pin,
        &encrypted_bytes,
        resolved.chunk_count,
    ) {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut headers = HeaderMap::new();
    let mime = resolve_mime(&resolved.display_name);
    if let Ok(hv) = mime.parse() {
        headers.insert("Content-Type", hv);
    }
    headers.insert("Content-Length", plaintext.len().to_string().parse().unwrap());
    headers.insert("ETag", etag.parse().unwrap_or_else(|_| "\"\"".parse().unwrap()));
    if let Some(ref mdt) = modified_dt {
        let lm = mdt.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        if let Ok(hv) = lm.parse() {
            headers.insert("Last-Modified", hv);
        }
    }
    // RFC 6266: percent-encode the filename to protect against injection of
    // `"`, `\r`, `\n`, or other characters that would break the header.
    // `filename=` carries an ASCII fallback (non-ASCII → `_`); `filename*=`
    // carries the full Unicode name percent-encoded as UTF-8.
    let ascii_fallback: String = resolved
        .display_name
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' && c != '\r' && c != '\n' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded = percent_encode(resolved.display_name.as_bytes(), NON_ALPHANUMERIC).to_string();
    let header_value = format!("inline; filename=\"{}\"; filename*=UTF-8''{}", ascii_fallback, encoded);
    headers.insert(
        "Content-Disposition",
        header_value.parse().unwrap_or_else(|_| "inline".parse().unwrap()),
    );

    (StatusCode::OK, headers, Body::from(plaintext)).into_response()
}

pub(super) async fn head_response(state: &Arc<DavState>, path: &str) -> Response {
    let resolved = match resolve_path(state, path).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };

    let file_id = resolved.file_id.as_deref().unwrap_or("");
    let etag = make_etag(file_id, resolved.modified.as_deref());

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Length",
        resolved.size_bytes.unwrap_or(0).to_string().parse().unwrap(),
    );
    let mime = resolve_mime(&resolved.display_name);
    if let Ok(hv) = mime.parse() {
        headers.insert("Content-Type", hv);
    }
    headers.insert("ETag", etag.parse().unwrap_or_else(|_| "\"\"".parse().unwrap()));
    if let Some(ref m) = resolved.modified {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(m) {
            let lm = dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
            if let Ok(hv) = lm.parse() {
                headers.insert("Last-Modified", hv);
            }
        }
    }

    (StatusCode::OK, headers, "").into_response()
}

pub(super) async fn put_response(state: &Arc<DavState>, path: &str, body: Vec<u8>, if_match: Option<&str>) -> Response {
    let filename = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("file")
        .to_string();
    if filename.is_empty() {
        return (StatusCode::METHOD_NOT_ALLOWED, "cannot PUT on a collection").into_response();
    }

    // Resolve the parent directory (everything before the last segment)
    let parent_path = {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(0) => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };

    let parent_id: Option<String> = if parent_path == "/" {
        None
    } else {
        match resolve_path(state, parent_path).await {
            Ok(e) if e.is_collection => e.file_id,
            Ok(_) => {
                return (StatusCode::CONFLICT, "parent is not a folder").into_response();
            }
            Err(e) => {
                return (StatusCode::CONFLICT, format!("parent not found: {e}")).into_response();
            }
        }
    };

    // Check if a file already exists at this path — handles overwrite + If-Match.
    // For versioning: reuse the existing file_id so the server creates a version
    // entry rather than a new file (keeps share links valid, builds history).
    let existing: Option<ResolvedEntry> = resolve_path(state, path).await.ok();
    let existing_file_id: Option<uuid::Uuid> = if let Some(ref existing) = existing {
        // SF-0002: never version-replace a file sealed by the old iOS app (its
        // name or its CURRENT version), and never one the check could not read.
        // L13/A9b: a request row's content is not replaced at all — the edit
        // becomes a new owner item below — so there is no version to check.
        if existing.origin != beebeeb_core::owned_keys::KeyOrigin::RequestContent {
            if let Err(refusal) = write_refusal(state, existing).await {
                return refusal;
            }
        }
        if let Some(ref eid) = existing.file_id {
            // If-Match precondition: client must supply current ETag
            if let Some(im) = if_match {
                let current_etag = make_etag(eid, existing.modified.as_deref());
                if im != "*" && im != current_etag {
                    return (StatusCode::PRECONDITION_FAILED, "ETag mismatch").into_response();
                }
            }
            // Parse existing ID — if valid UUID, reuse for versioning
            eid.parse::<uuid::Uuid>().ok()
        } else {
            None
        }
    } else {
        None
    };
    // L13: a request row's content is read-only. An edit is saved as a new
    // owner item — never sealed under `C`, never published.
    let existing_file_id = match existing.as_ref() {
        Some(e) if e.origin == beebeeb_core::owned_keys::KeyOrigin::RequestContent => None,
        _ => existing_file_id,
    };

    // Invalidate parent directory cache so the new file appears immediately
    let parent_cache_key = normalise_cache_key(parent_path);
    invalidate_cache(state, &parent_cache_key).await;

    // Use existing UUID for versioning; mint a fresh one for new files. L13
    // (SF-0001): every answer below must echo exactly this id.
    let proposed = match existing_file_id {
        Some(listed) => crate::item_id::Proposed::Listed(listed),
        None => match crate::item_id::mint_owner(&state.master_key) {
            Ok(minted) => crate::item_id::Proposed::Minted(minted.id()),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
    };
    let file_uuid = proposed.id();
    // L13: a replacement seals through the resolver's write target (key and
    // chunk-format pin). A fresh item uses the string convention. A listed row
    // this client cannot resolve is never overwritten under a guessed key.
    let (file_key, name_pin) = match existing.as_ref().and_then(|e| e.write_target.clone()) {
        Some(target) => (crate::crypto::key_copy(target.key()), target.pin()),
        None => {
            if existing_file_id.is_some() {
                return (StatusCode::CONFLICT, crate::owned_keys::UNREADABLE).into_response();
            }
            (
                beebeeb_core::kdf::derive_file_key(&state.master_key, file_uuid.to_string().as_bytes()),
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            )
        }
    };

    // Encrypt filename (MIME type is now part of the encrypted envelope)
    let mime = beebeeb_core::media::guess_mime_type(&filename);
    let name_encrypted = match beebeeb_core::encrypt::encrypt_name_for(&file_key, &filename, mime, name_pin) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    let plan = beebeeb_types::plan_chunks(body.len() as u64, beebeeb_types::ChunkProfile::Cli);
    let chunk_size = plan.chunk_size_bytes as usize;

    let chunks_raw: Vec<&[u8]> = if body.is_empty() {
        vec![&[]]
    } else {
        body.chunks(chunk_size).collect()
    };

    let mut encrypted_chunks: Vec<(u32, Vec<u8>)> = Vec::with_capacity(chunks_raw.len());
    let mut total_encrypted_size: i64 = 0;

    for (i, chunk) in chunks_raw.iter().enumerate() {
        let bytes = match beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, chunk) {
            Ok(b) => b,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("encrypt chunk {i}: {e}")).into_response();
            }
        };
        total_encrypted_size += bytes.len() as i64;
        encrypted_chunks.push((i as u32, bytes));
    }

    let parent_uuid = parent_id.as_deref().and_then(|s| s.parse::<uuid::Uuid>().ok());
    let is_media = beebeeb_core::media::is_media(mime);
    let effective_file_id = Some(file_uuid);
    let _ = total_encrypted_size; // v2 init takes plaintext size; server recomputes encrypted total

    // On a replace (the path already had a file), pass the existing file's
    // current version_number as the v2 optimistic-concurrency base so the server
    // versions by file_id (stale-version → 409).
    let base_version_number: Option<i32> = if let Some(eid) = existing_file_id {
        match state.api.get_file(&eid.to_string()).await {
            // Task 1951: the current version sits in `download_version`.
            Ok(meta) => Some(crate::api::current_version_number(&meta).unwrap_or(1) as i32),
            Err(_) => None,
        }
    } else {
        None
    };

    // V2 upload: init (open session) → chunks (by session) → complete (by session).
    let init = match state
        .api
        .upload_init(
            effective_file_id,
            &name_encrypted,
            parent_uuid,
            body.len() as i64,
            chunk_size as i64,
            encrypted_chunks.len() as i32,
            is_media,
            base_version_number,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response(),
    };

    // L13 (SF-0001): the init must echo the id this client chose (a fresh one
    // for a new file, the listed row's for a replace). A different id aborts
    // before a single chunk is PUT under it.
    if let Err(e) = proposed.verify(Some(&init.file_id)) {
        // The session it just opened is not a session for our item: end it
        // (best effort, by the id WE chose) so it cannot hold a lease over it.
        let _ = state.api.upload_abandon(&file_uuid.to_string()).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    let session_id = init.upload_session_id.clone();

    for (idx, data) in encrypted_chunks {
        if let Err(e) = state.api.upload_chunk(&session_id, idx, bytes::Bytes::from(data)).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response();
        }
    }

    match state.api.upload_complete(&session_id).await {
        Ok(answer) => {
            // L13 (SF-0001): the completion answer must name the same item.
            if let Err(e) = proposed.verify(crate::item_id::echoed_id(&answer)) {
                // End the mislabelled session once, by the id this client chose.
                let _ = state.api.upload_abandon(&file_uuid.to_string()).await;
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
            // L15 = A (P2-11): a write into a shared folder stays owner-only.
            crate::owned_keys::announce_owner_only(&state.api, &state.master_key, parent_id.as_deref()).await;
            let mut headers = HeaderMap::new();
            headers.insert("Location", path.parse().unwrap_or_else(|_| "/".parse().unwrap()));
            (StatusCode::CREATED, headers, "").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response(),
    }
}

pub(super) async fn mkcol_response(state: &Arc<DavState>, path: &str) -> Response {
    let folder_name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("folder")
        .to_string();
    if folder_name.is_empty() {
        return (StatusCode::METHOD_NOT_ALLOWED, "cannot MKCOL root").into_response();
    }

    let parent_path = {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(0) => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };
    let parent_id: Option<uuid::Uuid> = if parent_path == "/" {
        None
    } else {
        match resolve_path(state, parent_path).await {
            Ok(e) if e.is_collection => e.file_id.as_deref().and_then(|s| s.parse().ok()),
            Ok(_) => return (StatusCode::CONFLICT, "parent is not a folder").into_response(),
            Err(e) => {
                return (StatusCode::CONFLICT, format!("parent not found: {e}")).into_response();
            }
        }
    };

    // Check folder doesn't already exist
    if resolve_path(state, path).await.is_ok() {
        return (StatusCode::METHOD_NOT_ALLOWED, "already exists").into_response();
    }

    // The folder name is sealed under the folder's own id. L13 (SF-0001): this
    // client mints that id; a different echo aborts before the folder is listed
    // or written into.
    match crate::item_id::create_owned_folder(&state.api, &state.master_key, &folder_name, parent_id).await {
        Ok(_) => {
            invalidate_cache(state, &normalise_cache_key(parent_path)).await;
            crate::owned_keys::announce_owner_only(
                &state.api,
                &state.master_key,
                parent_id.map(|p| p.to_string()).as_deref(),
            )
            .await;
            let mut headers = HeaderMap::new();
            headers.insert("Location", path.parse().unwrap_or_else(|_| "/".parse().unwrap()));
            (StatusCode::CREATED, headers, "").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

pub(super) async fn delete_response(state: &Arc<DavState>, path: &str, if_match: Option<&str>) -> Response {
    let resolved = match resolve_path(state, path).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };

    let file_id = match &resolved.file_id {
        Some(id) => id.clone(),
        None => return (StatusCode::METHOD_NOT_ALLOWED, "cannot delete root").into_response(),
    };

    // If-Match precondition check
    if let Some(im) = if_match {
        let current_etag = make_etag(&file_id, resolved.modified.as_deref());
        if im != "*" && im != current_etag {
            return (StatusCode::PRECONDITION_FAILED, "ETag mismatch").into_response();
        }
    }

    // Invalidate parent directory in cache
    let parent_path = parent_of(path);
    invalidate_cache(state, &normalise_cache_key(parent_path)).await;

    match state.api.trash_file(&file_id).await {
        Ok(_) => (StatusCode::NO_CONTENT, "").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response(),
    }
}

pub(super) async fn move_response(state: &Arc<DavState>, src_path: &str, destination: &str) -> Response {
    // Strip any http://host prefix from the Destination header to get a bare path
    let dst_path = if let Some(pos) = destination.find("://") {
        let after_scheme = &destination[pos + 3..];
        after_scheme.find('/').map(|i| &after_scheme[i..]).unwrap_or("/")
    } else {
        destination
    };

    let src = match resolve_path(state, src_path).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };
    let file_id = match &src.file_id {
        Some(id) => id.clone(),
        None => return (StatusCode::FORBIDDEN, "cannot move root").into_response(),
    };

    // Determine new name and new parent
    let new_name = dst_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(&src.display_name)
        .to_string();

    let dst_parent_path = {
        let trimmed = dst_path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(0) => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };

    // Resolve source parent for comparison
    let src_parent_path = {
        let trimmed = src_path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(0) => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };

    let same_parent = src_parent_path == dst_parent_path;

    // SF-0002: reparenting is a move, but renaming rewrites the name envelope.
    if new_name != src.display_name {
        if let Err(refusal) = write_refusal(state, &src).await {
            return refusal;
        }
    }

    let new_name_encrypted = if new_name != src.display_name {
        let mime_for_rename = if src.is_collection {
            None
        } else {
            beebeeb_core::media::guess_mime_type(&new_name)
        };
        // L13: the rename seals through the resolver's write target. A resolved
        // request row has no target: A9 keeps it on the string convention, with
        // the pin its resolved name records (P2-9). Any other row without a
        // target is refused (P2-6).
        let (file_key, rename_pin) = match &src.write_target {
            Some(target) => (crate::crypto::key_copy(target.key()), target.pin()),
            None if src.origin == beebeeb_core::owned_keys::KeyOrigin::RequestContent => (
                // A9: a request row keeps the string convention, with the pin
                // its resolved name records (P2-9), never a guessed Unrecorded.
                beebeeb_core::kdf::derive_file_key(&state.master_key, file_id.as_bytes()),
                src.name_pin
                    .unwrap_or(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
            ),
            None => {
                return (StatusCode::CONFLICT, crate::owned_keys::RENAME_UNKNOWN_KEY).into_response();
            }
        };
        match beebeeb_core::encrypt::encrypt_name_for(&file_key, &new_name, mime_for_rename, rename_pin) {
            Ok(s) => Some(s),
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        }
    } else {
        None
    };

    // Resolve only changed, non-root destinations; lookup failure cannot mean root.
    let destination_parent = if !same_parent && !dst_parent_path.trim_matches('/').is_empty() {
        match resolve_path(state, dst_parent_path).await {
            Ok(entry) => Some(entry),
            Err(e) => {
                return (StatusCode::CONFLICT, format!("destination parent not found: {e}")).into_response();
            }
        }
    } else {
        None
    };
    let patch_parent = match move_parent_target(same_parent, dst_parent_path, destination_parent.as_ref()) {
        Ok(target) => target,
        Err(e) => {
            use super::move_target::ParentError;
            let status = match e {
                ParentError::NotFound | ParentError::NotFolder => StatusCode::CONFLICT,
                ParentError::MissingId | ParentError::InvalidId => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return (status, e.to_string()).into_response();
        }
    };

    // L12/A19: a MOVE that takes an item out of a shared root adopts its foreign
    // subtree first, confirmed by a readback. An ancestry the server will not
    // answer for refuses the move (fail closed).
    let mut resolver = crate::owned_keys::OwnedKeyResolver::new();
    if let Err(e) = resolver
        .adopt_before_move(
            &state.api,
            &state.master_key,
            &file_id,
            src.parent_id.as_deref(),
            src.is_collection,
        )
        .await
    {
        return (StatusCode::CONFLICT, e).into_response();
    }

    match state
        .api
        .move_file(&file_id, new_name_encrypted.as_deref(), patch_parent)
        .await
    {
        Ok(_) => {
            // Invalidate both source and destination parent directories
            invalidate_cache(state, &normalise_cache_key(src_parent_path)).await;
            invalidate_cache(state, &normalise_cache_key(dst_parent_path)).await;
            let moved_into = patch_parent.flatten().map(|p| p.to_string());
            crate::owned_keys::announce_owner_only(&state.api, &state.master_key, moved_into.as_deref()).await;
            (StatusCode::CREATED, "").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.message).into_response(),
    }
}

pub(super) fn move_parent_target(
    same_parent: bool,
    parent_path: &str,
    entry: Option<&ResolvedEntry>,
) -> Result<Option<Option<Uuid>>, super::move_target::ParentError> {
    use super::move_target::{DestinationParent, parent_target};
    let target = if parent_path.trim_matches('/').is_empty() {
        DestinationParent::Root
    } else {
        match entry {
            Some(e) if e.is_collection => DestinationParent::Folder(e.file_id.as_deref()),
            Some(_) => DestinationParent::NotFolder,
            None => DestinationParent::Missing,
        }
    };
    parent_target(!same_parent, target)
}
