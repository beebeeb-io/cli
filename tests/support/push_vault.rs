//! The mock vault and `bb` runners of the task 2334 push tests (`push_folder_destination`).
//!
//! Like the server, the mock keeps folders by parent, creates a file's row (with its parent) at init, never
//! changes that parent at completion, and lists a folder's children. Scratch `HOME`, loopback only.
#![allow(dead_code, unused_imports)]

pub use std::collections::{BTreeMap, HashMap};
pub use std::path::{Path, PathBuf};
pub use std::process::{Command, Output, Stdio};
pub use std::sync::{Arc, Mutex};
pub use std::time::{Duration, Instant};

pub use axum::body::Bytes;
pub use axum::extract::{Path as UrlPath, Query, State};
pub use axum::http::StatusCode;
pub use axum::response::{IntoResponse, Response};
pub use axum::routing::{get, post, put};
pub use axum::{Json, Router};
pub use beebeeb_core::chunk_stream::{ExpectedChunkFormat, SessionResume, chunk_format_for_pin, resume_policy};
pub use serde_json::{Value, json};

pub const MOVIE_DIR: &str = "A House of Dynamite (2025)";
pub const MOVIE: &str = "A House of Dynamite (2025) (2160p).mkv";

/// A row of the mock's `files` table: a folder, or a file.
pub struct Item {
    pub id: String,
    pub parent: Option<String>,
    pub name_encrypted: String,
    pub is_folder: bool,
    /// Creation order, served as `created_at`.
    pub seq: u64,
    /// A file whose upload is open (the row exists from init on).
    pub uploading: bool,
    /// 0 until a version completed.
    pub version: i64,
    pub chunk_count: u32,
    /// Plaintext size of the current version (the server's `size_bytes`).
    pub size: u64,
}

pub struct Session {
    pub file_id: String,
    pub base: Option<i64>,
    pub chunk_count: u32,
    pub frames: BTreeMap<u32, Vec<u8>>,
    /// The plaintext chunk size the init declared.
    pub chunk_size: u64,
    /// The plaintext size this session declared at init.
    pub size: u64,
}

#[derive(Default)]
pub struct Vault {
    pub items: BTreeMap<String, Item>,
    pub sessions: BTreeMap<String, Session>,
    pub seq: u64,
    /// While set, every chunk PUT past index 0 hangs (the test then kills `bb`).
    pub stall: bool,
    /// Every chunk PUT waits while this is set (a test changes the source file meanwhile), then goes on.
    pub hold_chunks: bool,
    /// `(n, status)`: every upload init after the first `n` answers `status`.
    pub init_status_after: Option<(usize, u16)>,
    /// The current version of every completed file: its plaintext chunk size and its frames, in order, as
    /// `GET /files/{id}/download` serves them.
    pub contents: HashMap<String, (u64, Vec<u8>)>,
    pub inits: usize,
    pub abandons: usize,
    /// Chunk PUTs answered `skipped` (the session already held that index).
    pub skipped: usize,
    /// `GET /shares/invites/sent` rows.
    pub invites: Vec<Value>,
    /// `GET /shares/folder-links` rows.
    pub links: Vec<Value>,
    /// `POST /files/check-conflict` answers this status instead.
    pub conflict_status: Option<u16>,
    /// `GET /shares/invites/sent` answers this status instead.
    pub invites_status: Option<u16>,
    /// A server without the invite and folder-link routes (both 404).
    pub shares_unsupported: bool,
    /// Shared folders switched off for the account: the folder-link route answers 501 `shared_folders_v1`.
    pub folder_links_off: bool,
    /// Listing rows carry `share_count: 0` (otherwise they carry none).
    pub listing_share_count: bool,
    /// `GET /files/{id}` answers this status instead.
    pub file_status: Option<u16>,
    /// `GET /files/{id}` answers this status for these ids only.
    pub file_status_for: HashMap<String, u16>,
    /// `GET /files` listings, by `parent_id` (`None` = the root).
    pub listings: HashMap<Option<String>, usize>,
    /// `GET /shares/invites/sent` requests.
    pub invite_reads: usize,
    /// `GET /shares/folder-links` requests (only a push's share check reads them).
    pub link_reads: usize,
    /// `GET /files/{id}` requests, by id.
    pub file_reads: HashMap<String, usize>,
    /// Rows `GET /files/{id}` serves as file-request uploads (the request fields set).
    pub request_rows: std::collections::HashSet<String>,
    /// `GET /file-requests` answers this status instead (otherwise: no requests).
    pub file_requests_status: Option<u16>,
    /// LD-0113: `system_role` by row id (the Backups root is `"backups_root"`).
    pub roles: HashMap<String, String>,
    /// LD-0113: listings and rows leave `system_role` out (a server from before LD-0113's wire field), while
    /// the server still refuses a change to the root.
    pub hide_roles: bool,
    /// Trashed rows: listings leave them out, `GET /files/{id}` says `is_trashed`.
    pub trashed: std::collections::HashSet<String>,
    /// Every request: method, path and its `X-Beebeeb-Origin` header.
    pub requests: Vec<(String, String, Option<String>)>,
    /// `POST /files/trash` and `PATCH /files/{id}` requests, refused ones included.
    pub trash_calls: usize,
    pub patch_calls: usize,
    /// `POST /files/folder` requests.
    pub folder_creates: usize,
}

pub type Shared = Arc<Mutex<Vault>>;

pub fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "message": msg }))).into_response()
}

/// A row in the server's shape (`GET /files/{id}` and the listing).
pub fn row(i: &Item) -> Value {
    json!({
        "id": i.id,
        "parent_id": i.parent,
        "name_encrypted": i.name_encrypted,
        "is_folder": i.is_folder,
        "is_trashed": false,
        "size_bytes": i.size,
        "chunk_count": i.chunk_count,
        "created_at": format!("2026-11-06T10:{:02}:{:02}Z", i.seq / 60, i.seq % 60),
        "is_uploading": i.uploading,
        "download_version": if !i.is_folder && i.version > 0 {
            json!({ "version_number": i.version, "chunk_count": i.chunk_count })
        } else {
            Value::Null
        },
    })
}

/// `row(i)` with what the server adds per row: `system_role` and `is_trashed` (LD-0113).
pub fn row_in(v: &Vault, i: &Item) -> Value {
    let mut r = row(i);
    if !v.hide_roles {
        r["system_role"] = v.roles.get(&i.id).map_or(Value::Null, |role| json!(role));
    }
    r["is_trashed"] = json!(v.trashed.contains(&i.id));
    r
}

/// The server's `409 protected_folder` (core's words are the CLI's to print; the server's text is its own).
pub fn protected() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "error": "protected_folder", "message": "refused by the mock server" })),
    )
        .into_response()
}

/// `id` and every row under it.
pub fn subtree(v: &Vault, id: &str) -> Vec<String> {
    let mut out = vec![id.to_string()];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].clone();
        out.extend(
            v.items
                .values()
                .filter(|c| c.parent.as_deref() == Some(&parent))
                .map(|c| c.id.clone()),
        );
        i += 1;
    }
    out
}

async fn record_request(
    State(v): State<Shared>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let origin = req
        .headers()
        .get("x-beebeeb-origin")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    v.lock()
        .unwrap()
        .requests
        .push((req.method().to_string(), req.uri().path().to_string(), origin));
    next.run(req).await
}

pub fn parent_is_live_folder(v: &Vault, parent: Option<&str>) -> bool {
    parent.is_none_or(|p| v.items.get(p).is_some_and(|i| i.is_folder))
}

pub async fn start_mock(vault: Shared) -> String {
    let app = Router::new()
        .route(
            "/api/v1/files/usage",
            get(|| async { Json(json!({ "used_bytes": 0 })) }),
        )
        .route(
            "/api/v1/files",
            // One folder's children (`parent_id` absent = the root), one page.
            get(
                |State(v): State<Shared>, Query(q): Query<HashMap<String, String>>| async move {
                    let mut v = v.lock().unwrap();
                    let parent = q.get("parent_id").cloned();
                    *v.listings.entry(parent.clone()).or_default() += 1;
                    let files: Vec<Value> = v
                        .items
                        .values()
                        .filter(|i| i.parent == parent && !v.trashed.contains(&i.id))
                        .map(|i| {
                            let mut r = row_in(&v, i);
                            if v.listing_share_count {
                                r["share_count"] = json!(0);
                            }
                            r
                        })
                        .collect();
                    Json(json!({ "files": files, "next_cursor": null }))
                },
            ),
        )
        .route(
            "/api/v1/shares/invites/sent",
            get(|State(v): State<Shared>| async move {
                let mut v = v.lock().unwrap();
                v.invite_reads += 1;
                if v.shares_unsupported {
                    return err(StatusCode::NOT_FOUND, "not_found");
                }
                match v.invites_status {
                    Some(code) => err(StatusCode::from_u16(code).unwrap(), "failed by mock"),
                    None => Json(json!({ "invites": v.invites })).into_response(),
                }
            }),
        )
        .route(
            "/api/v1/file-requests",
            get(|State(v): State<Shared>| async move {
                let v = v.lock().unwrap();
                match v.file_requests_status {
                    Some(code) => err(StatusCode::from_u16(code).unwrap(), "failed by mock"),
                    None => Json(json!({ "file_requests": [] })).into_response(),
                }
            }),
        )
        .route(
            "/api/v1/shares/folder-links",
            get(|State(v): State<Shared>| async move {
                let mut v = v.lock().unwrap();
                v.link_reads += 1;
                if v.shares_unsupported {
                    return err(StatusCode::NOT_FOUND, "not_found");
                }
                if v.folder_links_off {
                    return err(StatusCode::NOT_IMPLEMENTED, "shared_folders_v1");
                }
                Json(json!({ "links": v.links })).into_response()
            }),
        )
        .route(
            "/api/v1/files/folder",
            post(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let mut v = v.lock().unwrap();
                let parent = body.get("parent_id").and_then(Value::as_str).map(str::to_string);
                if !parent_is_live_folder(&v, parent.as_deref()) {
                    return err(StatusCode::NOT_FOUND, "not_found");
                }
                let id = body
                    .get("folder_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                v.folder_creates += 1;
                v.seq += 1;
                let item = Item {
                    id: id.clone(),
                    parent,
                    name_encrypted: body["name_encrypted"].as_str().unwrap_or("").to_string(),
                    is_folder: true,
                    seq: v.seq,
                    uploading: false,
                    version: 0,
                    chunk_count: 0,
                    size: 0,
                };
                let answer = row(&item);
                v.items.insert(id, item);
                (StatusCode::CREATED, Json(answer)).into_response()
            }),
        )
        .route(
            "/api/v1/files/check-conflict",
            // The server's shape: every FILE row in the folder, uploading ones too.
            post(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let v = v.lock().unwrap();
                if let Some(code) = v.conflict_status {
                    return err(StatusCode::from_u16(code).unwrap(), "failed by mock");
                }
                let parent = body.get("parent_id").and_then(Value::as_str).map(str::to_string);
                let files: Vec<Value> = v
                    .items
                    .values()
                    .filter(|i| !i.is_folder && i.parent == parent)
                    .map(|i| json!({ "id": i.id, "name_encrypted": i.name_encrypted }))
                    .collect();
                Json(json!({ "files": files })).into_response()
            }),
        )
        .route(
            "/api/v1/auth/me",
            get(|| async { Json(json!({ "user_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa" })) }),
        )
        .route(
            "/api/v1/backups/root",
            // As on the server (`routes/backups_root.rs`): the flagged root, else claim the named live root-level
            // folder, else create one with the client's id and sealed name.
            put(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let mut v = v.lock().unwrap();
                if let Some((id, _)) = v.roles.iter().find(|(_, r)| *r == "backups_root") {
                    return Json(json!({ "id": id, "outcome": "existing" })).into_response();
                }
                let id = body["folder_id"].as_str().unwrap_or_default().to_string();
                let claimable = v
                    .items
                    .get(&id)
                    .is_some_and(|i| i.is_folder && i.parent.is_none() && !v.trashed.contains(&id));
                if claimable {
                    v.roles.insert(id.clone(), "backups_root".into());
                    return Json(json!({ "id": id, "outcome": "claimed" })).into_response();
                }
                let Some(name) = body.get("name_encrypted").and_then(Value::as_str) else {
                    return err(StatusCode::CONFLICT, "backups_root_unclaimable");
                };
                v.seq += 1;
                let seq = v.seq;
                v.items.insert(
                    id.clone(),
                    Item {
                        id: id.clone(),
                        parent: None,
                        name_encrypted: name.to_string(),
                        is_folder: true,
                        seq,
                        uploading: false,
                        version: 0,
                        chunk_count: 0,
                        size: 0,
                    },
                );
                v.roles.insert(id.clone(), "backups_root".into());
                Json(json!({ "id": id, "outcome": "created" })).into_response()
            }),
        )
        .route(
            "/api/v1/files/trash",
            post(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let mut v = v.lock().unwrap();
                v.trash_calls += 1;
                let ids: Vec<String> = body["ids"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                if ids.iter().any(|id| v.roles.contains_key(id)) {
                    return protected();
                }
                let (mut trashed, mut missing) = (Vec::new(), Vec::new());
                for id in ids {
                    if v.items.contains_key(&id) {
                        for row in subtree(&v, &id) {
                            v.trashed.insert(row);
                        }
                        trashed.push(id);
                    } else {
                        missing.push(id);
                    }
                }
                Json(json!({ "trashed": trashed, "already_trashed": [], "missing": missing })).into_response()
            }),
        )
        .route(
            "/api/v1/files/:id",
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let mut v = v.lock().unwrap();
                *v.file_reads.entry(id.clone()).or_default() += 1;
                if let Some(code) = v.file_status.or_else(|| v.file_status_for.get(&id).copied()) {
                    return err(StatusCode::from_u16(code).unwrap(), "failed by mock");
                }
                match v.items.get(&id) {
                    Some(i) => {
                        let mut r = row_in(&v, i);
                        if v.request_rows.contains(&id) {
                            r["file_request_id"] = json!(uuid::Uuid::from_u128(77));
                            r["sender_ephemeral_pubkey"] = json!("AAAA");
                            r["wrapped_content_key"] = json!("AAAA");
                        }
                        Json(r).into_response()
                    }
                    None => err(StatusCode::NOT_FOUND, "not_found"),
                }
            })
            // A move and/or rename (`PATCH /files/{id}`); the root is refused, as on the server (§5.3 row 5).
            .patch(
                |State(v): State<Shared>, UrlPath(id): UrlPath<String>, Json(body): Json<Value>| async move {
                    let mut v = v.lock().unwrap();
                    v.patch_calls += 1;
                    if v.roles.contains_key(&id) {
                        return protected();
                    }
                    let parent = body.get("parent_id").map(|p| p.as_str().map(str::to_string));
                    let Some(item) = v.items.get_mut(&id) else {
                        return err(StatusCode::NOT_FOUND, "not_found");
                    };
                    if let Some(parent) = parent {
                        item.parent = parent;
                    }
                    if let Some(name) = body.get("name_encrypted").and_then(Value::as_str) {
                        item.name_encrypted = name.to_string();
                    }
                    let answer = row(item);
                    Json(answer).into_response()
                },
            ),
        )
        .route(
            "/api/v1/files/:id/download",
            // The current version's frames, as the server streams them.
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let v = v.lock().unwrap();
                match (v.items.get(&id), v.contents.get(&id)) {
                    (Some(item), Some((chunk_size, body))) => (
                        [
                            ("content-type", "application/octet-stream".to_string()),
                            ("x-chunk-size", chunk_size.to_string()),
                            ("x-original-size", item.size.to_string()),
                        ],
                        body.clone(),
                    )
                        .into_response(),
                    _ => err(StatusCode::NOT_FOUND, "not_found"),
                }
            }),
        )
        .route(
            "/api/v1/files/:id/upload/abandon",
            // As on the server: end the sessions; a never-completed first
            // upload's row goes with them.
            post(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let mut v = v.lock().unwrap();
                v.abandons += 1;
                let before = v.sessions.len();
                v.sessions.retain(|_, s| s.file_id != id);
                if v.sessions.len() == before {
                    return err(StatusCode::NOT_FOUND, "nothing to abandon");
                }
                let first_upload = v.items.get(&id).is_some_and(|i| i.version == 0);
                if first_upload {
                    v.items.remove(&id);
                } else if let Some(i) = v.items.get_mut(&id) {
                    i.uploading = false;
                }
                Json(json!({ "abandoned": true })).into_response()
            }),
        )
        .route(
            "/api/v1/uploads/init",
            // As on the server (`routes/uploads/init.rs`): a new file's row is
            // created HERE, under the `parent_id` this request names.
            post(|State(v): State<Shared>, Json(body): Json<Value>| async move {
                let mut v = v.lock().unwrap();
                v.inits += 1;
                if let Some((n, code)) = v.init_status_after
                    && v.inits > n
                {
                    return err(StatusCode::from_u16(code).unwrap(), "refused by mock");
                }
                let id = body.get("file_id").and_then(Value::as_str).unwrap_or("").to_string();
                let base = body.get("base_version_number").and_then(Value::as_i64);
                let parent = body.get("parent_id").and_then(Value::as_str).map(str::to_string);
                let chunk_size = body.get("chunk_size_bytes").and_then(Value::as_u64).unwrap_or(0);
                let chunk_count = body.get("chunk_count").and_then(Value::as_u64).unwrap_or(0) as u32;
                let size = body.get("file_size_bytes").and_then(Value::as_u64).unwrap_or(0);
                match v.items.get(&id).map(|i| (i.uploading, i.version, i.is_folder)) {
                    Some((_, _, true)) => return err(StatusCode::CONFLICT, "file id refers to a folder"),
                    Some((true, _, _)) => {
                        return err(StatusCode::CONFLICT, "upload is already in progress for this file");
                    }
                    Some((false, version, false)) => {
                        if base != Some(version) {
                            return err(StatusCode::CONFLICT, "stale base version for replacement upload");
                        }
                        v.items.get_mut(&id).unwrap().uploading = true;
                    }
                    None => {
                        if !parent_is_live_folder(&v, parent.as_deref()) {
                            return err(StatusCode::NOT_FOUND, "not_found");
                        }
                        v.seq += 1;
                        let seq = v.seq;
                        v.items.insert(
                            id.clone(),
                            Item {
                                id: id.clone(),
                                parent,
                                name_encrypted: body["file_name"].as_str().unwrap_or("").to_string(),
                                is_folder: false,
                                seq,
                                uploading: true,
                                version: 0,
                                chunk_count,
                                size,
                            },
                        );
                    }
                }
                let sid = uuid::Uuid::new_v4().to_string();
                v.sessions.insert(
                    sid.clone(),
                    Session {
                        file_id: id.clone(),
                        base,
                        chunk_count,
                        frames: BTreeMap::new(),
                        chunk_size,
                        size,
                    },
                );
                Json(json!({
                    "upload_session_id": sid,
                    "file_id": id,
                    "chunk_size_bytes": chunk_size,
                    "chunk_count": chunk_count,
                }))
                .into_response()
            }),
        )
        .route(
            "/api/v1/uploads/:sid/chunks/:idx",
            put(
                |State(v): State<Shared>, UrlPath((sid, idx)): UrlPath<(String, u32)>, body: Bytes| async move {
                    let stall = v.lock().unwrap().stall && idx > 0;
                    if stall {
                        std::future::pending::<()>().await;
                    }
                    while v.lock().unwrap().hold_chunks {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    let mut v = v.lock().unwrap();
                    let Some(s) = v.sessions.get_mut(&sid) else {
                        return err(StatusCode::NOT_FOUND, "not_found");
                    };
                    if s.frames.contains_key(&idx) {
                        v.skipped += 1;
                        return Json(json!({ "index": idx, "size": body.len(), "skipped": true })).into_response();
                    }
                    s.frames.insert(idx, body.to_vec());
                    Json(json!({ "index": idx, "size": body.len(), "skipped": false })).into_response()
                },
            ),
        )
        .route(
            "/api/v1/uploads/:sid/complete",
            // As on the server: the version completes in the row init created;
            // its parent is NOT taken from this request (there is none).
            post(|State(v): State<Shared>, UrlPath(sid): UrlPath<String>| async move {
                let mut v = v.lock().unwrap();
                let Some(s) = v.sessions.remove(&sid) else {
                    return err(StatusCode::NOT_FOUND, "not_found");
                };
                if s.frames.len() as u32 != s.chunk_count {
                    return err(StatusCode::BAD_REQUEST, "missing chunks");
                }
                let version = s.base.map_or(1, |b| b + 1);
                let Some(item) = v.items.get_mut(&s.file_id) else {
                    return err(StatusCode::NOT_FOUND, "not_found");
                };
                item.uploading = false;
                item.version = version;
                item.size = s.size;
                item.chunk_count = s.chunk_count;
                v.contents.insert(
                    s.file_id.clone(),
                    (s.chunk_size, s.frames.into_values().flatten().collect()),
                );
                Json(json!({ "id": s.file_id, "version_number": version })).into_response()
            }),
        )
        .fallback(|| async { err(StatusCode::NOT_FOUND, "not_found") })
        // A 4 MiB chunk frame exceeds axum's 2 MB default body limit.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(vault.clone(), record_request))
        .with_state(vault);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

// ── The vault as the user would see it ──────────────────────────────────────

/// The scratch config's master key: 32 zero bytes.
pub fn master() -> beebeeb_core::kdf::MasterKey {
    beebeeb_core::kdf::MasterKey::from_bytes([0u8; 32])
}

pub fn name_of(i: &Item) -> String {
    beebeeb_core::encrypt::decrypt_name(&master(), &i.id, &i.name_encrypted).unwrap_or_else(|_| "<unreadable>".into())
}

/// Ids of the folders named `name` directly in `parent` (`None` = the root).
pub fn folders_named(v: &Vault, parent: Option<&str>, name: &str) -> Vec<String> {
    v.items
        .values()
        .filter(|i| i.is_folder && i.parent.as_deref() == parent && name_of(i) == name)
        .map(|i| i.id.clone())
        .collect()
}

/// Ids of every folder named `name`, anywhere in the vault.
pub fn folders_named_anywhere(v: &Vault, name: &str) -> Vec<String> {
    v.items
        .values()
        .filter(|i| i.is_folder && name_of(i) == name)
        .map(|i| i.id.clone())
        .collect()
}

/// The one folder named `name` in `parent`, or a failure naming how many.
pub fn the_folder(v: &Vault, parent: Option<&str>, name: &str) -> String {
    let found = folders_named(v, parent, name);
    assert_eq!(
        found.len(),
        1,
        "exactly one folder named {name:?} in {parent:?}: {found:?}"
    );
    found[0].clone()
}

/// (id, parent, completed) of every file row named `name`.
pub fn files_named(v: &Vault, name: &str) -> Vec<(String, Option<String>, bool)> {
    v.items
        .values()
        .filter(|i| !i.is_folder && name_of(i) == name)
        .map(|i| (i.id.clone(), i.parent.clone(), i.version > 0 && !i.uploading))
        .collect()
}

/// A folder sealed as `bb mkdir` would seal it, put straight into the vault
/// (an older one than anything `bb` makes next).
pub fn seed_folder(vault: &Shared, parent: Option<&str>, name: &str) -> String {
    let item = beebeeb_core::item_id::NewItemId::owner(&master()).unwrap();
    let sealed =
        beebeeb_core::encrypt::encrypt_name_for_new_item(&master(), &item, name, None, ExpectedChunkFormat::Unrecorded)
            .unwrap();
    let mut v = vault.lock().unwrap();
    v.seq += 1;
    let id = item.as_str().to_string();
    let seq = v.seq;
    v.items.insert(
        id.clone(),
        Item {
            id: id.clone(),
            parent: parent.map(str::to_string),
            name_encrypted: sealed,
            is_folder: true,
            seq,
            uploading: false,
            version: 0,
            chunk_count: 0,
            size: 0,
        },
    );
    id
}

/// A finished file (or folder) in `parent` whose name is sealed under ANOTHER account's key, with no share
/// relation: this device reads the row (`GET /files/{id}`) and still cannot open it, on any run.
pub fn seed_unreadable(vault: &Shared, parent: Option<&str>, is_folder: bool) -> String {
    let stranger = beebeeb_core::kdf::MasterKey::from_bytes([9u8; 32]);
    let item = beebeeb_core::item_id::NewItemId::owner(&stranger).unwrap();
    let sealed = beebeeb_core::encrypt::encrypt_name_for_new_item(
        &stranger,
        &item,
        "someone else's.txt",
        None,
        ExpectedChunkFormat::Unrecorded,
    )
    .unwrap();
    let mut v = vault.lock().unwrap();
    v.seq += 1;
    let id = item.as_str().to_string();
    let seq = v.seq;
    v.items.insert(
        id.clone(),
        Item {
            id: id.clone(),
            parent: parent.map(str::to_string),
            name_encrypted: sealed,
            is_folder,
            seq,
            uploading: false,
            version: 1,
            chunk_count: 1,
            size: 5,
        },
    );
    id
}

/// Share `folder` with one person, as the web app's invite would.
pub fn share_with_someone(vault: &Shared, folder: &str) {
    vault.lock().unwrap().invites.push(json!({
        "id": uuid::Uuid::new_v4(),
        "file_id": folder,
        "recipient_email": "friend@beebeeb.io",
        "status": "approved",
        "is_folder_share": true,
    }));
}

/// The names of the files directly in `parent`.
pub fn file_names_in(v: &Vault, parent: &str) -> Vec<String> {
    let mut names: Vec<String> = v
        .items
        .values()
        .filter(|i| !i.is_folder && i.parent.as_deref() == Some(parent))
        .map(name_of)
        .collect();
    names.sort();
    names
}

// ── Running bb ──────────────────────────────────────────────────────────────

pub fn scratch_home(api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-2334-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "push-folder@beebeeb.io",
        "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    }
    home
}

/// Start `bb <args>` in the scratch `home`, loopback API only, its output to `stdout` / `stderr`. It hands back
/// the running child, never the `Command`, so no caller in another file can drop the guard
/// (`tests/api_guard.rs`).
fn spawn_bb(home: &Path, args: &[String], stdout: Stdio, stderr: Stdio) -> std::process::Child {
    spawn_bb_env(home, args, &[], stdout, stderr)
}

/// [`spawn_bb`] with extra environment (LD-0113: the backup's machine-id test seams).
fn spawn_bb_env(
    home: &Path,
    args: &[String],
    env: &[(&str, String)],
    stdout: Stdio,
    stderr: Stdio,
) -> std::process::Child {
    // The caller's `env` goes on first and the guard last, so nothing in `env` can switch the guard off or move the
    // scratch `HOME` (`tests/api_guard.rs` fails on an `.envs(` after the guard).
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .current_dir(home)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env("BB_REFUSE_NON_LOOPBACK_API", "1")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn bb")
}

pub fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

pub async fn bb(home: &Path, args: &[&str]) -> Output {
    let child = spawn_bb(home, &owned(args), Stdio::piped(), Stdio::piped());
    let shown = args.join(" ");
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().expect("run bb"))
        .await
        .unwrap();
    eprintln!(
        "[bb {} -> {:?}]\n--- stdout\n{}--- stderr\n{}",
        shown,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// [`bb`] with extra environment.
pub async fn bb_env(home: &Path, args: &[&str], env: &[(&str, String)]) -> Output {
    let child = spawn_bb_env(home, &owned(args), env, Stdio::piped(), Stdio::piped());
    let shown = args.join(" ");
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().expect("run bb"))
        .await
        .unwrap();
    eprintln!(
        "[bb {} -> {:?}]\n--- stdout\n{}--- stderr\n{}",
        shown,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run `bb <args>` with chunk PUTs stalled until the server holds chunk 0 of a
/// session that needs more, then SIGKILL it: an interrupted push.
pub async fn push_and_kill(vault: &Shared, home: &Path, args: &[&str]) {
    vault.lock().unwrap().stall = true;
    let mut child = spawn_bb(home, &owned(args), Stdio::null(), Stdio::null());
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let started = vault
            .lock()
            .unwrap()
            .sessions
            .values()
            .any(|s| s.chunk_count >= 2 && s.frames.contains_key(&0));
        if started {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!(
                "bb {} exited before it could be interrupted: {status:?}",
                args.join(" ")
            );
        }
        assert!(Instant::now() < deadline, "the upload never got chunk 0 to the server");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().expect("SIGKILL bb");
    let status = child.wait().unwrap();
    assert!(!status.success(), "the killed push must not report success");
    vault.lock().unwrap().stall = false;
}

/// Two chunks at the 4 MiB minimum chunk size; `k` varies the bytes.
pub fn two_chunks(k: u8) -> Vec<u8> {
    (0..4 * 1024 * 1024 + 1000usize)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(k).wrapping_add((i >> 12) as u8))
        .collect()
}

/// `<home>/<rel>` with `bytes`, its folders made first.
pub fn write_file(home: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let path = home.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

// ── Defect 1: a re-push makes a second same-name folder ─────────────────────

/// Push once, then share the folder the push made with someone.
pub async fn pushed_then_shared(vault: &Shared, api: &str, home: &Path) -> (String, String) {
    write_file(home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");
    let out = bb(home, &["--api", api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "first push: {}", stderr(&out));
    let (movies, shared) = {
        let v = vault.lock().unwrap();
        let movies = the_folder(&v, None, "Movies");
        let shared = the_folder(&v, Some(&movies), MOVIE_DIR);
        (movies, shared)
    };
    share_with_someone(vault, &shared);
    write_file(home, &format!("{MOVIE_DIR}/extras.mkv"), b"extras");
    (movies, shared)
}

#[cfg(unix)]
#[derive(Debug)]
pub struct TerminalRun {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Run `bb <args>` with a pty as its controlling terminal and type `typed` +
/// Enter into it (`tests/account_email_change.rs` does the same).
#[cfg(unix)]
pub async fn bb_at_terminal(home: &Path, args: &[&str], typed: &str) -> TerminalRun {
    let (home, args, typed) = (home.to_path_buf(), owned(args), typed.to_string());
    tokio::task::spawn_blocking(move || run_at_terminal(&home, &args, &typed))
        .await
        .unwrap()
}

#[cfg(unix)]
pub fn run_at_terminal(home: &Path, args: &[String], typed: &str) -> TerminalRun {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;

    let (mut master, slave) = unsafe {
        let (mut m, mut s) = (0, 0);
        let rc = libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        assert_eq!(rc, 0, "openpty failed");
        (std::fs::File::from_raw_fd(m), OwnedFd::from_raw_fd(s))
    };
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.env("BB_REFUSE_NON_LOOPBACK_API", "1");
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Become a session leader and adopt the pty as the controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn bb");
    drop(cmd);
    // Drain the terminal, or a session leader's exit blocks on closing it.
    let mut tty_out = master.try_clone().unwrap();
    let drain = std::thread::spawn(move || {
        let mut sink = [0u8; 1024];
        while matches!(tty_out.read(&mut sink), Ok(n) if n > 0) {}
    });
    master.write_all(format!("{typed}\n").as_bytes()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bb did not exit within 120s (stuck on a question?)");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    drop(master);
    let _ = drain.join();
    eprintln!(
        "[bb {} at a terminal, typed {typed:?} -> {:?}]\n--- stdout\n{stdout}--- stderr\n{stderr}",
        args.join(" "),
        status.code()
    );
    TerminalRun {
        code: status.code(),
        stdout,
        stderr,
    }
}
