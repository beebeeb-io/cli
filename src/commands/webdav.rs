//! `bb webdav` — serve the Beebeeb vault as a local WebDAV server.
//!
//! # Day 1 scope (read-only)
//!
//! - `OPTIONS /` → announces WebDAV class 1 compliance
//! - `PROPFIND /[path]` + `Depth: 0|1` → list files with decrypted names
//! - `GET /[path]` → download + decrypt → plaintext response
//! - `HEAD /[path]` → Content-Length (encrypted blob size → plaintext size approx.)
//!
//! Write support (PUT/MKCOL/DELETE/MOVE), locking (LOCK/UNLOCK), and caching
//! are planned for Day 2–6.
//!
//! # Architecture
//!
//! ```
//! WebDAV client (Finder/rclone/Cyberduck)
//!        │  HTTP/WebDAV on localhost:7878
//!        ▼
//!  bb webdav handler (this module)
//!        │  reqwest + session token
//!        ▼
//!  Beebeeb API  ─►  decrypt with master_key
//! ```
//!
//! Path resolution walks the vault tree on every request — no cache yet.
//! Each path segment is decrypted and matched against folder names so
//! `/Documents/2025/report.pdf` resolves to the file UUID correctly.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use axum::routing::any;
use base64::Engine as _;
use beebeeb_types::EncryptedBlob;
use colored::Colorize;

use crate::api::ApiClient;
use crate::config::load_config;

// ─── CLI entry point ─────────────────────────────────────────────────────────

pub async fn run(port: u16, read_only: bool) -> Result<(), String> {
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

    let state = Arc::new(DavState {
        api: ApiClient::from_config(),
        master_key,
    });

    let router = Router::new()
        .route("/", any(handle_webdav))
        .route("/*path", any(handle_webdav))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("failed to bind {addr}: {e}"))?;

    let ro_label = if read_only { " (read-only)" } else { "" };
    println!(
        "\n  {} {}",
        "◆".truecolor(212, 168, 67),
        format!("WebDAV gateway started{ro_label}").truecolor(208, 200, 154),
    );
    println!(
        "  {}  {}",
        "url".truecolor(106, 101, 91),
        format!("http://localhost:{port}").truecolor(245, 184, 0),
    );
    println!(
        "  {}",
        "Connect Finder: Go → Connect to Server → http://localhost:7878"
            .truecolor(106, 101, 91),
    );
    println!(
        "  {}",
        "Press Ctrl+C to stop.".truecolor(106, 101, 91),
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

    match method {
        Method::OPTIONS => options_response(),
        ref m if m.as_str() == "PROPFIND" => {
            let depth = headers
                .get("depth")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("1");
            propfind_response(&state, &path, depth).await
        }
        Method::GET => get_response(&state, &path).await,
        Method::HEAD => head_response(&state, &path).await,
        _ => (StatusCode::METHOD_NOT_ALLOWED, "").into_response(),
    }
}

// ─── OPTIONS ─────────────────────────────────────────────────────────────────

fn options_response() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert("DAV", "1, 2".parse().unwrap());
    headers.insert("Allow", "OPTIONS, GET, HEAD, PROPFIND".parse().unwrap());
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
        match state.api.list_files(parent_id).await {
            Ok(resp) => {
                let files = resp
                    .get("files")
                    .and_then(|f| f.as_array())
                    .cloned()
                    .unwrap_or_default();

                for file in &files {
                    if let Some(entry) = decode_file_entry(file, &state.master_key, path) {
                        xml_entries.push(prop_entry(
                            &child_href(path, &entry.display_name, entry.is_collection),
                            &entry,
                        ));
                    }
                }
            }
            Err(e) => {
                eprintln!("  PROPFIND list error: {e}");
            }
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

async fn get_response(state: &Arc<DavState>, path: &str) -> Response {
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
    if let Some(mime) = guess_mime(&resolved.display_name) {
        headers.insert("Content-Type", mime.parse().unwrap_or_else(|_| "application/octet-stream".parse().unwrap()));
    }
    headers.insert("Content-Length", plaintext.len().to_string().parse().unwrap());
    headers.insert(
        "Content-Disposition",
        format!("attachment; filename=\"{}\"", resolved.display_name)
            .parse()
            .unwrap_or_else(|_| "attachment".parse().unwrap()),
    );

    (StatusCode::OK, headers, Body::from(plaintext)).into_response()
}

// ─── HEAD ─────────────────────────────────────────────────────────────────────

async fn head_response(state: &Arc<DavState>, path: &str) -> Response {
    let resolved = match resolve_path(state, path).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::NOT_FOUND, e).into_response(),
    };

    let size_str = resolved.size_bytes.unwrap_or(0).to_string();
    let mut headers = HeaderMap::new();
    headers.insert("Content-Length", size_str.parse().unwrap());
    if let Some(mime) = guess_mime(&resolved.display_name) {
        headers.insert("Content-Type", mime.parse().unwrap_or_else(|_| "application/octet-stream".parse().unwrap()));
    }

    (StatusCode::OK, headers, "").into_response()
}

// ─── Path resolution ─────────────────────────────────────────────────────────

/// A resolved vault entry (file or collection).
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
