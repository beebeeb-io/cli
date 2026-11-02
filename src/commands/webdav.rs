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
//! Write (refused unless `--read-write` is passed; read-only is the default):
//! - `PUT /[path]` → encrypt + upload; handles If-Match optimistic lock
//! - `MKCOL /[path]` → create encrypted folder
//! - `DELETE /[path]` → soft-delete (trash); handles If-Match
//! - `MOVE /[src]` + `Destination:` header → rename or reparent
//!
//! Access control (task 1790, M31) — see `webdav_guard`: Host must be this gateway, no
//! `Origin` header, HTTP Basic with a per-run random password printed at startup, and
//! writes (PUT/DELETE/MKCOL/MOVE/COPY/PROPPATCH) refused unless `--read-write`.
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

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use base64::Engine as _;
use colored::Colorize;
use percent_encoding::{NON_ALPHANUMERIC, percent_encode};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::webdav_guard::{self, Guard};
use crate::api::ApiClient;
use crate::commands::move_target;
use crate::config::load_config;

// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

// ─── Directory cache ─────────────────────────────────────────────────────────



// ─── Lock store ──────────────────────────────────────────────────────────────



// ─── CLI entry point ─────────────────────────────────────────────────────────






// ─── Shared state ─────────────────────────────────────────────────────────────


// ─── Request dispatcher ───────────────────────────────────────────────────────


// ─── OPTIONS ─────────────────────────────────────────────────────────────────


// ─── PROPFIND ────────────────────────────────────────────────────────────────


// ─── GET ─────────────────────────────────────────────────────────────────────


// ─── HEAD ─────────────────────────────────────────────────────────────────────


// ─── PUT (upload / overwrite) ─────────────────────────────────────────────────


// ─── MKCOL (create folder) ───────────────────────────────────────────────────


// ─── DELETE (soft-delete / trash) ────────────────────────────────────────────


// ─── MOVE (rename / reparent) ────────────────────────────────────────────────



// ─── LOCK ─────────────────────────────────────────────────────────────────────







// ─── Path cache helpers ───────────────────────────────────────────────────────







// ─── ETag helpers ─────────────────────────────────────────────────────────────



// ─── Path resolution ─────────────────────────────────────────────────────────





// ─── Decryption helpers ───────────────────────────────────────────────────────

// ─── XML helpers ─────────────────────────────────────────────────────────────





#[cfg(test)]
mod parent_target_tests {
    use super::*;
    use crate::commands::move_target::ParentError;

    fn folder(id: Option<&str>) -> ResolvedEntry {
        ResolvedEntry {
            file_id: id.map(str::to_owned),
            display_name: "Target".into(),
            is_collection: true,
            size_bytes: None,
            chunk_count: 0,
            modified: None,
            pin: no_content(),
            read_only: false,
        }
    }

    #[test]
    fn webdav_parent_target_root_folder_unchanged() {
        let id = Uuid::new_v4();
        assert_eq!(move_parent_target(false, "/", None), Ok(Some(None)));
        assert_eq!(
            move_parent_target(false, "/Target", Some(&folder(Some(&id.to_string())))),
            Ok(Some(Some(id)))
        );
        assert_eq!(move_parent_target(true, "/Target", None), Ok(None));
    }

    #[test]
    fn webdav_parent_target_unknown_folder() {
        assert_eq!(move_parent_target(false, "/Target", None), Err(ParentError::NotFound));
    }

    #[test]
    fn webdav_parent_target_missing_id() {
        assert_eq!(
            move_parent_target(false, "/Target", Some(&folder(None))),
            Err(ParentError::MissingId)
        );
    }

    #[test]
    fn webdav_parent_target_invalid_id() {
        assert_eq!(
            move_parent_target(false, "/Target", Some(&folder(Some("invalid")))),
            Err(ParentError::InvalidId)
        );
    }

    #[test]
    fn webdav_parent_target_file() {
        let mut e = folder(None);
        e.is_collection = false;
        assert_eq!(
            move_parent_target(false, "/Target", Some(&e)),
            Err(ParentError::NotFolder)
        );
    }
    #[tokio::test]
    async fn webdav_move_rejects_invalid_parent_without_patch() {
        use axum::{
            Json,
            routing::{get, patch},
        };
        use serde_json::json;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for (id, status) in [
            (json!("not-a-uuid"), StatusCode::INTERNAL_SERVER_ERROR),
            (json!(""), StatusCode::INTERNAL_SERVER_ERROR),
            (serde_json::Value::Null, StatusCode::CONFLICT),
        ] {
            let patches = Arc::new(AtomicUsize::new(0));
            let reads = Arc::new(AtomicUsize::new(0));
            let get_reads = reads.clone();
            let patch_count = patches.clone();
            let app = Router::new()
                .route("/api/v1/files", get(move || {
                    let id = id.clone();
                    let reads = get_reads.clone();
                    async move {
                        reads.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"files": [
                            {"id": "33333333-3333-4333-8333-333333333333", "name_encrypted": "file.txt", "is_folder": false},
                            {"id": id, "name_encrypted": "Target", "is_folder": true}
                        ]}))
                    }
                }))
                .route("/api/v1/files/:id", patch(move || {
                    let count = patch_count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"ok": true}))
                    }
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let state = Arc::new(DavState {
                api: ApiClient::new_for_test(format!("http://{}", listener.local_addr().unwrap())),
                master_key: beebeeb_core::kdf::MasterKey::from_bytes([0; 32]),
                read_only: false,
                dir_cache: Mutex::new(HashMap::new()),
                cache_ttl: Duration::ZERO,
                locks: Mutex::new(HashMap::new()),
                request_count: std::sync::atomic::AtomicU64::new(0),
                error_count: std::sync::atomic::AtomicU64::new(0),
                verbose: false,
            });
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let response = move_response(&state, "/file.txt", "/Target/file.txt").await;
            server.abort();
            assert_eq!(
                response.status(),
                status,
                "invalid destination must fail the WebDAV MOVE"
            );
            let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
            assert!(!body.is_empty(), "WebDAV error must have an explanation");
            assert_eq!(reads.load(Ordering::SeqCst), 2, "must resolve source and destination");
            assert_eq!(
                patches.load(Ordering::SeqCst),
                0,
                "invalid WebDAV parent must send no PATCH"
            );
        }
    }
}

#[cfg(test)]
mod windows_hint_tests {
    use super::*;

    #[test]
    fn hint_names_every_alternative_and_only_shows_on_windows() {
        for needle in [
            "Explorer",
            "not supported",
            "desktop app",
            "rclone",
            "Cyberduck",
            "WinSCP",
        ] {
            assert!(WINDOWS_EXPLORER_HINT.contains(needle), "hint lacks {needle:?}");
        }
        assert_eq!(windows_explorer_hint().is_some(), cfg!(windows));
    }
}

/// SF-0002 round 2: WebDAV asks the one write check before a PUT-overwrite or a
/// rename. These run the real handlers against an in-process API mock that
/// counts every request that is not a GET (a write).
#[cfg(test)]
mod sf_0002_write_check_tests {
    use super::*;
    use axum::{Json, routing::get};
    use serde_json::json;
    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

    const OWNED: &str = "11111111-1111-4111-8111-111111111111";

    fn master() -> beebeeb_core::kdf::MasterKey {
        beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32])
    }

    /// A root-level file row: `name` and its single legacy frame sealed under `key`.
    fn file_row(id: &str, name: &str, key: &beebeeb_core::kdf::FileKey, plaintext: &[u8]) -> serde_json::Value {
        let sealed = beebeeb_core::encrypt::encrypt_name_for(
            key,
            name,
            Some("text/plain"),
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        json!({"id": id, "parent_id": null, "name_encrypted": sealed, "is_folder": false,
               "size_bytes": plaintext.len(), "chunk_count": 1, "updated_at": "2026-10-31T10:00:00Z"})
    }

    struct Mock {
        state: Arc<DavState>,
        download_status: Arc<AtomicU16>,
        downloads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
        server: tokio::task::JoinHandle<()>,
    }

    async fn mock(rows: Vec<serde_json::Value>, frame: Vec<u8>) -> Mock {
        let download_status = Arc::new(AtomicU16::new(200));
        let downloads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let (status, served, written) = (download_status.clone(), downloads.clone(), writes.clone());
        let app = Router::new()
            .route(
                "/api/v1/files",
                get(move || {
                    let rows = rows.clone();
                    async move { Json(json!({ "files": rows })) }
                }),
            )
            .route(
                "/api/v1/files/:id/download",
                get(move || {
                    let (frame, status, served) = (frame.clone(), status.clone(), served.clone());
                    async move {
                        served.fetch_add(1, Ordering::SeqCst);
                        match status.load(Ordering::SeqCst) {
                            200 => frame.into_response(),
                            s => StatusCode::from_u16(s).unwrap().into_response(),
                        }
                    }
                }),
            )
            .fallback(move |req: Request| {
                let written = written.clone();
                async move {
                    if req.method() != Method::GET {
                        written.fetch_add(1, Ordering::SeqCst);
                    }
                    (StatusCode::INTERNAL_SERVER_ERROR, "mock: unhandled")
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = Arc::new(DavState {
            api: ApiClient::new_for_test(format!("http://{}", listener.local_addr().unwrap())),
            master_key: master(),
            read_only: false,
            dir_cache: Mutex::new(HashMap::new()),
            cache_ttl: Duration::ZERO,
            locks: Mutex::new(HashMap::new()),
            request_count: std::sync::atomic::AtomicU64::new(0),
            error_count: std::sync::atomic::AtomicU64::new(0),
            verbose: false,
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Mock {
            state,
            download_status,
            downloads,
            writes,
            server,
        }
    }

    async fn body_text(r: Response) -> String {
        String::from_utf8_lossy(&axum::body::to_bytes(r.into_body(), 4096).await.unwrap()).into_owned()
    }

    /// C1: a 5xx probe refuses a PUT-overwrite and a rename with 503 and the
    /// retryable message, and no write is sent. With the version readable, the
    /// same PUT gets past the check (it reaches the upload init).
    #[tokio::test]
    async fn put_and_rename_refuse_with_503_when_the_check_cannot_read_the_version() {
        let key = beebeeb_core::kdf::derive_file_key(&master(), OWNED.as_bytes());
        let plaintext = b"an ordinary owner file";
        let frame = beebeeb_core::encrypt::encrypt_chunk_raw(&key, plaintext).unwrap();
        let m = mock(vec![file_row(OWNED, "owned.txt", &key, plaintext)], frame).await;
        m.download_status.store(503, Ordering::SeqCst);

        let put = put_response(&m.state, "/owned.txt", b"edited".to_vec(), None).await;
        assert_eq!(put.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body_text(put).await, crate::owned_keys::WRITE_CHECK_UNKNOWN);
        let rename = move_response(&m.state, "/owned.txt", "/renamed.txt").await;
        assert_eq!(rename.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(m.writes.load(Ordering::SeqCst), 0, "a refused write sent a request");
        assert_eq!(m.downloads.load(Ordering::SeqCst), 2, "each write asked the check once");

        m.download_status.store(200, Ordering::SeqCst);
        let put = put_response(&m.state, "/owned.txt", b"edited".to_vec(), None).await;
        assert_ne!(put.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_ne!(put.status(), StatusCode::FORBIDDEN);
        assert!(
            m.writes.load(Ordering::SeqCst) >= 1,
            "a readable ordinary row must get past the check to the upload"
        );
        m.server.abort();
    }

    /// C5: the owner-wrap-of-uppercase row `bb mv` refuses
    /// (`owner_read_repair::an_owner_wrap_of_the_uppercase_key_is_read_only`) is
    /// refused here as well: 403 by its name, no probe, no write.
    #[tokio::test]
    async fn an_owner_wrap_of_the_uppercase_key_is_read_only() {
        const UPPER: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
        let upper = beebeeb_core::kdf::derive_file_key(&master(), UPPER.to_ascii_uppercase().as_bytes());
        let plaintext = b"SF-0002 read-only repair content";
        let mut row = file_row(UPPER, "uppercase-repair.pdf", &upper, plaintext);
        let wrap = beebeeb_core::owned_keys::wrap_owner_file_key(&master(), UPPER, &upper).unwrap();
        row["owner_wrapped_file_key"] = json!(base64::engine::general_purpose::STANDARD.encode(wrap));
        let frame = beebeeb_core::encrypt::encrypt_chunk_raw(&upper, plaintext).unwrap();
        let m = mock(vec![row], frame).await;
        m.download_status.store(503, Ordering::SeqCst);

        let put = put_response(&m.state, "/uppercase-repair.pdf", b"edited".to_vec(), None).await;
        assert_eq!(put.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_text(put).await, crate::crypto::OWNER_REPAIR_READ_ONLY);
        let rename = move_response(&m.state, "/uppercase-repair.pdf", "/renamed.pdf").await;
        assert_eq!(rename.status(), StatusCode::FORBIDDEN);
        assert_eq!(m.writes.load(Ordering::SeqCst), 0, "a refused write sent a request");
        assert_eq!(m.downloads.load(Ordering::SeqCst), 0, "the name decides; no probe");
        m.server.abort();
    }
}

#[cfg(test)]
mod item_id_echo_tests {
    use super::*;
    use crate::test_server::{Mock, Shared, shared, spawn};

    fn state_for(base: String) -> Arc<DavState> {
        Arc::new(DavState {
            api: ApiClient::new_for_test(base),
            master_key: beebeeb_core::kdf::MasterKey::from_bytes([0; 32]),
            read_only: false,
            dir_cache: Mutex::new(HashMap::new()),
            cache_ttl: Duration::ZERO,
            locks: Mutex::new(HashMap::new()),
            request_count: std::sync::atomic::AtomicU64::new(0),
            error_count: std::sync::atomic::AtomicU64::new(0),
            verbose: false,
        })
    }

    async fn dav(mock: Mock) -> (Arc<DavState>, Shared) {
        let mock = shared(mock);
        (state_for(spawn(mock.clone()).await), mock)
    }

    async fn body_text(response: Response) -> String {
        let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        String::from_utf8_lossy(&body).into_owned()
    }

    /// A8 (webdav MKCOL): a folder create whose answer echoes a different id
    /// aborts the request instead of listing/writing into it.
    #[tokio::test]
    async fn mkcol_echoing_a_different_id_aborts() {
        let other = Uuid::new_v4();
        let (state, mock) = dav(Mock {
            folder_echo: Some(other.to_string()),
            ..Default::default()
        })
        .await;
        let response = mkcol_response(&state, "/Folder").await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_text(response).await, crate::item_id::CHANGED_ID_MESSAGE);
        let m = mock.lock().unwrap();
        assert_eq!(m.folder_requests.len(), 1);
        assert_ne!(
            m.folder_requests[0],
            other.to_string(),
            "the client proposed its own id"
        );
    }

    /// A7 + L4 (webdav PUT): the upload init answers a different id. Abort
    /// before a single chunk is PUT under it, and end the session it opened,
    /// once, by the id this client minted.
    #[tokio::test]
    async fn put_init_echoing_a_different_id_aborts_before_any_chunk() {
        let (state, mock) = dav(Mock {
            init_echo: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        })
        .await;
        let response = put_response(&state, "/file.txt", b"hello".to_vec(), None).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_text(response).await, crate::item_id::CHANGED_ID_MESSAGE);
        let m = mock.lock().unwrap();
        assert_eq!(m.inits.len(), 1);
        assert!(m.chunk_puts.is_empty(), "no chunk may be PUT under the echoed id");
        assert!(m.completed.is_empty());
        assert_eq!(m.abandons, m.inits, "exactly one abandon, by the id this client minted");
    }

    /// L4 (webdav PUT): the completion answer names another item. Abort and end
    /// the session it opened, once, by the id this client minted.
    #[tokio::test]
    async fn put_completion_echoing_a_different_id_abandons_once() {
        let (state, mock) = dav(Mock {
            complete_echo: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        })
        .await;
        let response = put_response(&state, "/file.txt", b"hello".to_vec(), None).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_text(response).await, crate::item_id::CHANGED_ID_MESSAGE);
        let m = mock.lock().unwrap();
        assert_eq!(m.inits.len(), 1);
        assert_eq!(m.completed.len(), 1, "the check runs on the completion answer");
        assert_eq!(m.abandons, m.inits, "exactly one abandon, by the id this client minted");
    }

    /// Normal flow: every answer echoes the minted id; nothing is abandoned.
    #[tokio::test]
    async fn put_echoing_the_clients_id_creates_the_file() {
        let (state, mock) = dav(Mock::default()).await;
        let response = put_response(&state, "/file.txt", b"hello".to_vec(), None).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let m = mock.lock().unwrap();
        assert_eq!(m.inits.len(), 1);
        let minted = Uuid::parse_str(&m.inits[0]).expect("a uuid was proposed");
        assert_eq!(minted.get_version(), Some(uuid::Version::Random));
        assert_eq!(m.completed.len(), 1);
        assert!(m.abandons.is_empty());
    }
}

mod server;
use server::*;
pub use server::{run};

mod verbs;
use verbs::*;

mod lock;
use lock::*;

mod cache;
use cache::*;

mod entry;
use entry::*;
