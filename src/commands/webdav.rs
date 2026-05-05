//! `bb webdav` — serve the Beebeeb vault as a local WebDAV server.
//!
//! # Implemented (Day 1-5)
//!
//! Read:
//! - `OPTIONS /` → announces WebDAV class 1 compliance
//! - `PROPFIND /[path]` + `Depth: 0|1` → list files with decrypted names + ETags
//! - `GET /[path]` → download + decrypt → plaintext; handles If-None-Match, If-Modified-Since
//! - `HEAD /[path]` → Content-Length + ETag + Last-Modified
//!
//! Write (disabled by `--read-only`):
//! - `PUT /[path]` → encrypt + upload; handles If-Match optimistic lock
//! - `MKCOL /[path]` → create encrypted folder
//! - `DELETE /[path]` → soft-delete (trash); handles If-Match
//! - `MOVE /[src]` + `Destination:` header → rename or reparent
//!
//! Performance:
//! - `DirCache` — in-memory TTL cache (default 30s) keyed by parent path
//! - Write ops invalidate affected directory cache entries
//! - `--no-cache` disables caching; `--cache-ttl N` configures TTL
//!
//! # Planned (Day 6)
//!
//! - LOCK/UNLOCK stubs (Finder requires before writing)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use axum::routing::any;
use base64::Engine as _;
use beebeeb_types::EncryptedBlob;
use colored::Colorize;
use tokio::sync::Mutex;

use crate::api::ApiClient;
use crate::config::load_config;

/// 4 MiB chunk size for WebDAV uploads.
const CHUNK_SIZE: usize = 4 * 1024 * 1024;

// ─── Directory cache ─────────────────────────────────────────────────────────

/// One cached directory listing.
#[derive(Clone)]
struct CachedDir {
    /// Decoded + decrypted entries for this directory.
    children: Vec<ResolvedEntry>,
    /// When this entry was populated.
    cached_at: Instant,
}

/// Global directory-listing cache keyed by parent path (e.g. `"/"`, `"/Documents"`).
type DirCache = Mutex<HashMap<String, CachedDir>>;

// ─── Lock store ──────────────────────────────────────────────────────────────

/// An active WebDAV lock.  Stubs only — no distributed coordination.
/// Exists solely to satisfy Finder/LibreOffice lock expectations.
#[derive(Clone)]
struct LockEntry {
    /// `urn:uuid:<UUID>` token that the client echoes back on writes.
    token: String,
    /// Lock owner string (from the request XML, for display only).
    #[allow(dead_code)]
    owner: String,
    /// Whether the lock is exclusive (true) or shared (false).
    exclusive: bool,
    /// When the lock expires (Instant).
    expires_at: Instant,
}

/// Lock store: path → active lock.
type LockStore = Mutex<HashMap<String, LockEntry>>;

// ─── CLI entry point ─────────────────────────────────────────────────────────

pub async fn run(port: u16, read_only: bool, cache_ttl: u64, no_cache: bool) -> Result<(), String> {
    let config = load_config();

    if config.session_token.is_none() {
        return Err("Not logged in. Run `bb login` first.".to_string());
    }

    let mk_b64 = config
        .master_key
        .ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes = base64::engine::general_purpose::STANDARD
        .decode(&mk_b64)
        .map_err(|e| format!("invalid master key in config: {e}"))?;
    if mk_bytes.len() != 32 {
        return Err(format!(
            "master key must be 32 bytes, got {}",
            mk_bytes.len()
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    let master_key = beebeeb_core::kdf::MasterKey::from_bytes(arr);

    let effective_ttl = if no_cache || cache_ttl == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(cache_ttl)
    };

    let state = Arc::new(DavState {
        api: ApiClient::from_config(),
        master_key,
        read_only,
        dir_cache: Mutex::new(HashMap::new()),
        cache_ttl: effective_ttl,
        locks: Mutex::new(HashMap::new()),
    });

    let router = Router::new()
        .route("/", any(handle_webdav))
        .route("/*path", any(handle_webdav))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;

    let ro_label = if read_only { " read-only" } else { " read-write" };
    let cache_label = if no_cache || cache_ttl == 0 {
        "disabled".to_string()
    } else {
        format!("{cache_ttl}s TTL")
    };
    println!(
        "\n  {} {}",
        "◆".custom_color(crate::colors::AMBER_DARK),
        format!("WebDAV gateway started{ro_label}").custom_color(crate::colors::INK_WARM),
    );
    println!(
        "  {}  {}",
        "url".custom_color(crate::colors::INK_DIM),
        format!("http://localhost:{port}").custom_color(crate::colors::AMBER),
    );
    println!(
        "  {}  {}",
        "cache".custom_color(crate::colors::INK_DIM),
        cache_label.custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {}",
        "Connect Finder: Go → Connect to Server → http://localhost:7878"
            .custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {}",
        "Press Ctrl+C to stop.".custom_color(crate::colors::INK_DIM),
    );
    println!();

    axum::serve(listener, router)
        .await
        .map_err(|e| format!("server error: {e}"))
}

// ─── Shared state ─────────────────────────────────────────────────────────────

struct DavState {
    api: ApiClient,
    master_key: beebeeb_core::kdf::MasterKey,
    read_only: bool,
    dir_cache: DirCache,
    /// Zero duration = caching disabled.
    cache_ttl: Duration,
    /// In-memory WebDAV lock store (stub — single-instance only).
    locks: LockStore,
}

// ─── Request dispatcher ───────────────────────────────────────────────────────

async fn handle_webdav(
    State(state): State<Arc<DavState>>,
    req: Request,
) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();
    let path = uri.path().to_string();

    eprintln!("  WebDAV {} {}", method, path);

    // Guard write operations when --read-only is set
    let is_write = matches!(method, Method::PUT | Method::DELETE)
        || method.as_str() == "MKCOL"
        || method.as_str() == "MOVE";
    if is_write && state.read_only {
        return (StatusCode::METHOD_NOT_ALLOWED, "read-only mode — use bb webdav without --read-only").into_response();
    }

    // Extract the If: (<token>) header once (used by PUT/DELETE to verify locks)
    let if_token = headers
        .get("if")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_if_token);

    match method {
        Method::OPTIONS => options_response(&state),
        ref m if m.as_str() == "PROPFIND" => {
            let depth = headers
                .get("depth")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("1");
            propfind_response(&state, &path, depth).await
        }
        Method::GET => get_response(&state, &path, &headers).await,
        Method::HEAD => head_response(&state, &path).await,
        Method::PUT => {
            let if_match = headers
                .get("if-match")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            // Verify lock if the path is locked and client provides a token
            if let Err(r) = check_lock(&state, &path, if_token.as_deref()).await {
                return r;
            }
            let body_bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                Ok(b) => b.to_vec(),
                Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
            };
            put_response(&state, &path, body_bytes, if_match.as_deref()).await
        }
        Method::DELETE => {
            let if_match = headers
                .get("if-match")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            if let Err(r) = check_lock(&state, &path, if_token.as_deref()).await {
                return r;
            }
            delete_response(&state, &path, if_match.as_deref()).await
        }
        ref m if m.as_str() == "MKCOL" => mkcol_response(&state, &path).await,
        ref m if m.as_str() == "MOVE" => {
            let destination = headers
                .get("destination")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            move_response(&state, &path, &destination).await
        }
        ref m if m.as_str() == "LOCK" => {
            let timeout_secs = parse_timeout_header(&headers);
            let body_bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
                .await
                .map(|b| b.to_vec())
                .unwrap_or_default();
            lock_response(&state, &path, &body_bytes, timeout_secs).await
        }
        ref m if m.as_str() == "UNLOCK" => {
            let lock_token = headers
                .get("lock-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            unlock_response(&state, &path, &lock_token).await
        }
        _ => (StatusCode::METHOD_NOT_ALLOWED, "").into_response(),
    }
}

// ─── OPTIONS ─────────────────────────────────────────────────────────────────

fn options_response(state: &DavState) -> Response {
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

// ─── PROPFIND ────────────────────────────────────────────────────────────────

async fn propfind_response(state: &Arc<DavState>, path: &str, depth: &str) -> Response {
    // Resolve path to a vault entry
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

    // For Depth: 1 on a collection, also list children
    if depth == "1" && resolved.is_collection {
        let parent_id = resolved.file_id.as_deref();
        let cache_key = normalise_cache_key(path);

        // Try to serve from cache
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
                    files
                        .iter()
                        .filter_map(|f| decode_file_entry(f, &state.master_key, path))
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

// ─── GET ─────────────────────────────────────────────────────────────────────

async fn get_response(state: &Arc<DavState>, path: &str, req_headers: &HeaderMap) -> Response {
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

    // Build ETag and conditional-request headers
    let etag = make_etag(&file_id, resolved.modified.as_deref());

    // If-None-Match (for GET caching by client)
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

    // Download encrypted bytes
    let encrypted_bytes = match state.api.download_file(&file_id).await {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };

    // Decrypt all chunks
    let uuid: uuid::Uuid = match file_id.parse() {
        Ok(u) => u,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let file_key = beebeeb_core::kdf::derive_file_key(&state.master_key, uuid.as_bytes());

    let plaintext = match decrypt_chunks(&encrypted_bytes, &file_key, resolved.chunk_count) {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
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
    headers.insert(
        "Content-Disposition",
        format!("inline; filename=\"{}\"", resolved.display_name)
            .parse()
            .unwrap_or_else(|_| "inline".parse().unwrap()),
    );

    (StatusCode::OK, headers, Body::from(plaintext)).into_response()
}

// ─── HEAD ─────────────────────────────────────────────────────────────────────

async fn head_response(state: &Arc<DavState>, path: &str) -> Response {
    let resolved = match resolve_path(state, path).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };

    let file_id = resolved.file_id.as_deref().unwrap_or("");
    let etag = make_etag(file_id, resolved.modified.as_deref());

    let mut headers = HeaderMap::new();
    headers.insert("Content-Length", resolved.size_bytes.unwrap_or(0).to_string().parse().unwrap());
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

// ─── PUT (upload / overwrite) ─────────────────────────────────────────────────

async fn put_response(state: &Arc<DavState>, path: &str, body: Vec<u8>, if_match: Option<&str>) -> Response {
    // Derive filename from the last path segment
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
            Some(i) if i == 0 => "/",
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

    // Check if a file already exists at this path — handles overwrite + If-Match
    if let Ok(existing) = resolve_path(state, path).await {
        if let Some(ref eid) = existing.file_id {
            // If-Match precondition: client must supply current ETag
            if let Some(im) = if_match {
                let current_etag = make_etag(eid, existing.modified.as_deref());
                if im != "*" && im != current_etag {
                    return (StatusCode::PRECONDITION_FAILED, "ETag mismatch").into_response();
                }
            }
            let _ = state.api.trash_file(eid).await;
        }
    }

    // Invalidate parent directory cache so the new file appears immediately
    let parent_cache_key = normalise_cache_key(parent_path);
    invalidate_cache(state, &parent_cache_key).await;

    // Generate a new file UUID and derive the file key
    let file_uuid = uuid::Uuid::new_v4();
    let file_key =
        beebeeb_core::kdf::derive_file_key(&state.master_key, file_uuid.as_bytes());

    // Encrypt filename
    let name_blob = match beebeeb_core::encrypt::encrypt_metadata(&file_key, &filename) {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };
    let name_encrypted = match serde_json::to_string(&name_blob) {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    // Encrypt body in chunks
    let chunks_raw: Vec<&[u8]> = if body.is_empty() {
        vec![&[]]
    } else {
        body.chunks(CHUNK_SIZE).collect()
    };

    let mut encrypted_chunks: Vec<(u32, Vec<u8>)> = Vec::with_capacity(chunks_raw.len());
    let mut total_encrypted_size: i64 = 0;

    for (i, chunk) in chunks_raw.iter().enumerate() {
        let blob = match beebeeb_core::encrypt::encrypt_chunk(&file_key, chunk) {
            Ok(b) => b,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("encrypt chunk {i}: {e}"))
                    .into_response();
            }
        };
        let serialized = match serde_json::to_vec(&blob) {
            Ok(s) => s,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        };
        total_encrypted_size += serialized.len() as i64;
        encrypted_chunks.push((i as u32, serialized));
    }

    let mime = guess_mime(&filename).unwrap_or("application/octet-stream");
    let metadata = serde_json::json!({
        "name_encrypted": name_encrypted,
        "parent_id":      parent_id.as_deref().and_then(|s| s.parse::<uuid::Uuid>().ok()),
        "mime_type":      mime,
        "size_bytes":     total_encrypted_size,
    });
    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    match state.api.upload_encrypted(&metadata_json, &encrypted_chunks).await {
        Ok(_) => {
            let mut headers = HeaderMap::new();
            headers.insert("Location", path.parse().unwrap_or_else(|_| "/".parse().unwrap()));
            (StatusCode::CREATED, headers, "").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ─── MKCOL (create folder) ───────────────────────────────────────────────────

async fn mkcol_response(state: &Arc<DavState>, path: &str) -> Response {
    let folder_name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("folder")
        .to_string();
    if folder_name.is_empty() {
        return (StatusCode::METHOD_NOT_ALLOWED, "cannot MKCOL root").into_response();
    }

    // Resolve parent
    let parent_path = {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(i) if i == 0 => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };
    let parent_id: Option<uuid::Uuid> = if parent_path == "/" {
        None
    } else {
        match resolve_path(state, parent_path).await {
            Ok(e) if e.is_collection => {
                e.file_id.as_deref().and_then(|s| s.parse().ok())
            }
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

    // Encrypt folder name — needs a UUID key. Use nil UUID for folders (consistent
    // with the server's convention: folder name is encrypted with the folder's own UUID).
    let folder_uuid = uuid::Uuid::new_v4();
    let folder_key =
        beebeeb_core::kdf::derive_file_key(&state.master_key, folder_uuid.as_bytes());
    let name_blob = match beebeeb_core::encrypt::encrypt_metadata(&folder_key, &folder_name) {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };
    let name_encrypted = match serde_json::to_string(&name_blob) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    match state
        .api
        .create_folder(&name_encrypted, parent_id, Some(folder_uuid))
        .await
    {
        Ok(_) => {
            // Invalidate parent directory cache
            invalidate_cache(state, &normalise_cache_key(parent_path)).await;
            let mut headers = HeaderMap::new();
            headers.insert("Location", path.parse().unwrap_or_else(|_| "/".parse().unwrap()));
            (StatusCode::CREATED, headers, "").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ─── DELETE (soft-delete / trash) ────────────────────────────────────────────

async fn delete_response(state: &Arc<DavState>, path: &str, if_match: Option<&str>) -> Response {
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
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ─── MOVE (rename / reparent) ────────────────────────────────────────────────

async fn move_response(state: &Arc<DavState>, src_path: &str, destination: &str) -> Response {
    // Strip any http://host prefix from the Destination header to get a bare path
    let dst_path = if let Some(pos) = destination.find("://") {
        let after_scheme = &destination[pos + 3..];
        after_scheme
            .find('/')
            .map(|i| &after_scheme[i..])
            .unwrap_or("/")
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
            Some(i) if i == 0 => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };

    // Resolve source parent for comparison
    let src_parent_path = {
        let trimmed = src_path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(i) if i == 0 => "/",
            Some(i) => &trimmed[..i],
            None => "/",
        }
    };

    let same_parent = src_parent_path == dst_parent_path;

    // Compute new_name_encrypted if the name changed
    let file_uuid: uuid::Uuid = match file_id.parse() {
        Ok(u) => u,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let file_key = beebeeb_core::kdf::derive_file_key(&state.master_key, file_uuid.as_bytes());

    let new_name_encrypted = if new_name != src.display_name {
        let blob = match beebeeb_core::encrypt::encrypt_metadata(&file_key, &new_name) {
            Ok(b) => b,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
            }
        };
        match serde_json::to_string(&blob) {
            Ok(s) => Some(s),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    } else {
        None
    };

    // Resolve new parent if moved
    let new_parent_id: Option<uuid::Uuid> = if !same_parent {
        if dst_parent_path == "/" {
            // Moving to root: pass `null` parent.  Server interprets null as root.
            // We signal "move to root" by passing Some(uuid::Uuid::nil()) — but the
            // server PATCH endpoint treats parent_id as the new parent.
            // We'll pass None to the move_file call to set parent_id = null.
            None // will be handled specially below
        } else {
            match resolve_path(state, dst_parent_path).await {
                Ok(e) if e.is_collection => {
                    e.file_id.as_deref().and_then(|s| s.parse::<uuid::Uuid>().ok())
                }
                Ok(_) => {
                    return (StatusCode::CONFLICT, "destination parent is not a folder")
                        .into_response();
                }
                Err(e) => {
                    return (StatusCode::CONFLICT, format!("destination parent not found: {e}"))
                        .into_response();
                }
            }
        }
    } else {
        // Same parent — skip parent_id in the PATCH
        // We'll only send name_encrypted
        None
    };

    // Determine what to send to PATCH
    let patch_parent = if same_parent {
        None // don't change parent
    } else {
        Some(new_parent_id) // change parent (may be None = root)
    };

    match state
        .api
        .move_file(
            &file_id,
            new_name_encrypted.as_deref(),
            patch_parent.flatten(),
        )
        .await
    {
        Ok(_) => {
            // Invalidate both source and destination parent directories
            invalidate_cache(state, &normalise_cache_key(src_parent_path)).await;
            invalidate_cache(state, &normalise_cache_key(dst_parent_path)).await;
            (StatusCode::CREATED, "").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

// ─── LOCK ─────────────────────────────────────────────────────────────────────

/// Parse the lock scope from the LOCK request XML body.
/// Returns `(exclusive, owner_string)` — tolerant of malformed bodies.
fn parse_lock_body(body: &[u8]) -> (bool, String) {
    let text = std::str::from_utf8(body).unwrap_or("");
    // Extract exclusivity from the XML (simple string search — no full XML parse)
    let exclusive = !text.contains("shared");
    // Try to extract owner from <D:owner><D:href>...</D:href></D:owner>
    let owner = if let Some(start) = text.find("<D:href>") {
        let after = &text[start + 8..];
        if let Some(end) = after.find("</D:href>") {
            after[..end].trim().to_string()
        } else {
            "unknown".to_string()
        }
    } else if text.contains("owner") {
        "client".to_string()
    } else {
        "client".to_string()
    };
    (exclusive, owner)
}

/// Parse `Timeout: Second-N` or `Timeout: Infinite` header.
fn parse_timeout_header(headers: &HeaderMap) -> u64 {
    headers
        .get("timeout")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            if s.starts_with("Second-") {
                s[7..].parse::<u64>().ok()
            } else {
                None // Infinite → use default
            }
        })
        .unwrap_or(300) // 5 minutes default
}

/// Parse `If: (<urn:uuid:TOKEN>)` header and return the raw token string.
fn parse_if_token(if_header: &str) -> Option<String> {
    // Format: (<urn:uuid:...>) or (<urn:uuid:...> [etag])
    let inner = if_header.trim().trim_start_matches('(').trim_end_matches(')');
    if inner.is_empty() {
        return None;
    }
    // Take the first bracketed token
    let token = inner.split_whitespace().next()?;
    Some(token.trim_matches(|c: char| c == '<' || c == '>').to_string())
}

/// LOCK /path — issue a synthetic lock token.
async fn lock_response(state: &Arc<DavState>, path: &str, body: &[u8], timeout_secs: u64) -> Response {
    let (exclusive, owner) = parse_lock_body(body);
    let token_uuid = Uuid::new_v4();
    let token = format!("urn:uuid:{token_uuid}");
    let scope_label = if exclusive { "exclusive" } else { "shared" };

    // Clean up expired locks lazily
    {
        let mut locks = state.locks.lock().await;
        locks.retain(|_, v| v.expires_at > Instant::now());
        // Check for conflict: existing exclusive lock on same path
        if let Some(existing) = locks.get(path) {
            if existing.expires_at > Instant::now() && existing.exclusive {
                return (StatusCode::LOCKED, "resource is already exclusively locked").into_response();
            }
        }
        locks.insert(path.to_string(), LockEntry {
            token: token.clone(),
            owner: owner.clone(),
            exclusive,
            expires_at: Instant::now() + Duration::from_secs(timeout_secs),
        });
    }

    let href = xml_escape(path);
    let owner_escaped = xml_escape(&owner);
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <D:prop xmlns:D=\"DAV:\">\n\
           <D:lockdiscovery>\n\
             <D:activelock>\n\
               <D:locktype><D:write/></D:locktype>\n\
               <D:lockscope><D:{scope_label}/></D:lockscope>\n\
               <D:depth>0</D:depth>\n\
               <D:owner><D:href>{owner_escaped}</D:href></D:owner>\n\
               <D:timeout>Second-{timeout_secs}</D:timeout>\n\
               <D:locktoken><D:href>{token}</D:href></D:locktoken>\n\
               <D:lockroot><D:href>{href}</D:href></D:lockroot>\n\
             </D:activelock>\n\
           </D:lockdiscovery>\n\
         </D:prop>"
    );

    let lock_token_header = format!("<{token}>");
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", "application/xml; charset=utf-8".parse().unwrap());
    headers.insert("Lock-Token", lock_token_header.parse().unwrap_or_else(|_| "".parse().unwrap()));
    (StatusCode::OK, headers, body).into_response()
}

/// UNLOCK /path — remove the lock identified by the Lock-Token header.
async fn unlock_response(state: &Arc<DavState>, path: &str, lock_token_header: &str) -> Response {
    // Extract the URN from the angle-bracket wrapper: <urn:uuid:...>
    let token = lock_token_header.trim().trim_matches(|c| c == '<' || c == '>');

    let mut locks = state.locks.lock().await;
    if let Some(entry) = locks.get(path) {
        if entry.token == token {
            locks.remove(path);
            return (StatusCode::NO_CONTENT, "").into_response();
        }
        // Token mismatch — still return 204 (permissive stub)
    }
    // Lock not found — return 204 anyway (idempotent)
    (StatusCode::NO_CONTENT, "").into_response()
}

/// Verify that a locked resource's token matches what the client provided.
///
/// If the resource is not locked, the request proceeds normally (no lock needed).
/// If the resource IS locked and the client provides no/wrong token → 423 Locked.
/// If the resource IS locked and the client provides the correct token → OK.
///
/// This is permissive: if the lock has expired, we treat it as unlocked.
async fn check_lock(state: &Arc<DavState>, path: &str, client_token: Option<&str>) -> Result<(), Response> {
    let mut locks = state.locks.lock().await;
    let entry = match locks.get(path) {
        Some(e) => e.clone(),
        None => return Ok(()), // not locked
    };

    if entry.expires_at <= Instant::now() {
        locks.remove(path); // expired
        return Ok(());
    }

    match client_token {
        Some(tok) if tok == entry.token => Ok(()),
        Some(_) => Err((StatusCode::LOCKED, "lock token mismatch").into_response()),
        None => Err((StatusCode::LOCKED, "resource is locked — provide If: (<token>) header").into_response()),
    }
}

// ─── Path cache helpers ───────────────────────────────────────────────────────

fn normalise_cache_key(path: &str) -> String {
    let t = path.trim_end_matches('/');
    if t.is_empty() { "/".to_string() } else { t.to_string() }
}

fn parent_of(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) if i == 0 => "/",
        Some(i) => &trimmed[..i],
        None => "/",
    }
}

async fn get_from_cache(state: &DavState, key: &str) -> Option<Vec<ResolvedEntry>> {
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

async fn store_in_cache(state: &DavState, key: String, children: Vec<ResolvedEntry>) {
    if state.cache_ttl.is_zero() {
        return;
    }
    let mut cache = state.dir_cache.lock().await;
    cache.insert(key, CachedDir { children, cached_at: Instant::now() });
}

async fn invalidate_cache(state: &DavState, key: &str) {
    let mut cache = state.dir_cache.lock().await;
    cache.remove(key);
}

// ─── ETag helpers ─────────────────────────────────────────────────────────────

/// Produce a quoted ETag string for a file.
///
/// Format: `"<file_id>-<modified_epoch>"` — changes on every overwrite
/// (new file_id) and also when modified_at changes.
fn make_etag(file_id: &str, modified: Option<&str>) -> String {
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

// ─── Path resolution ─────────────────────────────────────────────────────────

/// A resolved vault entry (file or collection).
#[derive(Clone)]
struct ResolvedEntry {
    file_id: Option<String>,
    display_name: String,
    is_collection: bool,
    size_bytes: Option<u64>,
    chunk_count: u32,
    modified: Option<String>,
}

/// Decode a single API file object into a `ResolvedEntry`.
fn decode_file_entry(
    file: &serde_json::Value,
    master_key: &beebeeb_core::kdf::MasterKey,
    _parent_href: &str,
) -> Option<ResolvedEntry> {
    let file_id = file.get("id")?.as_str()?.to_string();
    let is_folder = file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
    let size_bytes = file.get("size_bytes").and_then(|v| v.as_u64());
    let chunk_count = file.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    let modified = file.get("updated_at").and_then(|v| v.as_str()).map(|s| s.to_string());

    // Decrypt name
    let name_encrypted = file.get("name_encrypted").and_then(|v| v.as_str())?;
    let display_name = decrypt_name(master_key, &file_id, name_encrypted)
        .unwrap_or_else(|| format!("[{}]", &file_id[..8]));

    Some(ResolvedEntry {
        file_id: Some(file_id),
        display_name,
        is_collection: is_folder,
        size_bytes,
        chunk_count,
        modified,
    })
}

/// Resolve a WebDAV path like `/Documents/2025/report.pdf` to a vault entry.
///
/// Walks the vault tree one segment at a time, decrypting folder names at each
/// level to find the matching child. O(depth × breadth) — no cache yet (Day 4).
async fn resolve_path(state: &Arc<DavState>, path: &str) -> Result<ResolvedEntry, String> {
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
                if entry.display_name.to_lowercase() == segment.to_lowercase() {
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

// ─── Decryption helpers ───────────────────────────────────────────────────────

fn decrypt_name(
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: &str,
    name_encrypted: &str,
) -> Option<String> {
    let uuid: uuid::Uuid = file_id.parse().ok()?;
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, uuid.as_bytes());
    let blob: EncryptedBlob = serde_json::from_str(name_encrypted).ok()?;
    beebeeb_core::encrypt::decrypt_metadata(&file_key, &blob).ok()
}

fn decrypt_chunks(
    data: &[u8],
    file_key: &beebeeb_core::kdf::FileKey,
    chunk_count: u32,
) -> Result<Vec<u8>, String> {
    let mut plaintext = Vec::new();
    let mut offset = 0;

    for i in 0..chunk_count {
        if offset >= data.len() {
            return Err(format!(
                "unexpected end at chunk {i}/{chunk_count} (offset {offset}, total {})",
                data.len()
            ));
        }

        let remaining = &data[offset..];
        let mut de = serde_json::Deserializer::from_slice(remaining).into_iter::<EncryptedBlob>();

        let blob = match de.next() {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(format!("parse error at chunk {i}: {e}")),
            None => return Err(format!("no data for chunk {i}/{chunk_count}")),
        };

        offset += de.byte_offset();

        let decrypted = beebeeb_core::encrypt::decrypt_chunk(file_key, &blob)
            .map_err(|e| format!("decrypt chunk {i}: {e}"))?;
        plaintext.extend_from_slice(&decrypted);
    }

    Ok(plaintext)
}

// ─── XML helpers ─────────────────────────────────────────────────────────────

fn child_href(parent: &str, name: &str, is_dir: bool) -> String {
    let parent = parent.trim_end_matches('/');
    let suffix = if is_dir { "/" } else { "" };
    // URL-encode the name for safe inclusion in hrefs
    let encoded: String = name
        .chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || "._-~".contains(c) {
                vec![c]
            } else if c == ' ' {
                vec!['%', '2', '0']
            } else {
                // percent-encode: for Day 1 just pass through printable ASCII
                vec![c]
            }
        })
        .collect();
    format!("{parent}/{encoded}{suffix}")
}

fn prop_entry(href: &str, entry: &ResolvedEntry) -> String {
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

    // Content-Type for non-folders
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

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Resolve a MIME type for a filename using the mime_guess crate.
/// Falls back to application/octet-stream for unknown extensions.
fn resolve_mime(filename: &str) -> String {
    mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string()
}

fn guess_mime(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit('.').next()?.to_lowercase();
    Some(match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "md" | "rs" | "toml" | "yaml" | "yml" | "json" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "ts" => "application/javascript",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "zip" => "application/zip",
        "gz" | "tar" => "application/x-tar",
        _ => "application/octet-stream",
    })
}
