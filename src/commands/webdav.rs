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

use super::webdav_guard::{self, Guard, GuardOptions};
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
            parent_id: None,
            plaintext_name: false,
            display_name: "Target".into(),
            is_collection: true,
            size_bytes: None,
            chunk_count: 0,
            modified: None,
            pin: no_content(),
            read_only: false,
            key: None,
            name_pin: None,
            write_target: None,
            origin: beebeeb_core::owned_keys::KeyOrigin::Derived,
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
        // 2275 L12: only a canonical id resolves, so a destination row with an
        // invalid id is an unresolvable parent - 409 "destination parent not
        // found", never a 500 and never a PATCH.
        for (id, status) in [
            (json!("not-a-uuid"), StatusCode::CONFLICT),
            (json!(""), StatusCode::CONFLICT),
            (serde_json::Value::Null, StatusCode::CONFLICT),
        ] {
            let patches = Arc::new(AtomicUsize::new(0));
            let reads = Arc::new(AtomicUsize::new(0));
            let get_reads = reads.clone();
            let patch_count = patches.clone();
            // 2275 L12: an owner (v4) file's name is sealed under the key its id
            // derives; a plaintext name would make the row unreadable.
            let src = "33333333-3333-4333-8333-33333333333c";
            let src_key =
                beebeeb_core::kdf::derive_file_key(&beebeeb_core::kdf::MasterKey::from_bytes([0; 32]), src.as_bytes());
            let src_name = beebeeb_core::encrypt::encrypt_name_for(
                &src_key,
                "file.txt",
                None,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            )
            .unwrap();
            let app = Router::new()
                .route(
                    "/api/v1/files",
                    get(move || {
                        let id = id.clone();
                        let reads = get_reads.clone();
                        let src_name = src_name.clone();
                        async move {
                            reads.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"files": [
                                {"id": src, "name_encrypted": src_name, "is_folder": false},
                                {"id": id, "name_encrypted": "Target", "is_folder": true}
                            ]}))
                        }
                    }),
                )
                .route(
                    "/api/v1/files/:id",
                    patch(move || {
                        let count = patch_count.clone();
                        async move {
                            count.fetch_add(1, Ordering::SeqCst);
                            Json(json!({"ok": true}))
                        }
                    }),
                );
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

    /// N3 (P3): a request row whose name was sealed under the legacy BINARY
    /// convention lists by its real name. Re-deriving the string convention
    /// would lose it and show `[xxxxxxxx]`.
    #[tokio::test]
    async fn a_request_row_named_under_the_binary_convention_lists_its_real_name() {
        use beebeeb_core::chunk_stream::ExpectedChunkFormat;
        const REQUEST: &str = "33333333-3333-4333-8333-333333333333";
        let binary = beebeeb_core::kdf::derive_file_key(&master(), Uuid::parse_str(REQUEST).unwrap().as_bytes());
        let sealed =
            beebeeb_core::encrypt::encrypt_name_for(&binary, "renamed.txt", None, ExpectedChunkFormat::Unrecorded)
                .unwrap();
        let row = json!({"id": REQUEST, "parent_id": null, "name_encrypted": sealed, "is_folder": false,
               "size_bytes": 12, "chunk_count": 1, "updated_at": "2026-10-31T10:00:00Z",
               "file_request_id": REQUEST, "sender_ephemeral_pubkey": "AA", "wrapped_content_key": "AA"});
        let entry = decode_file_entry(&row, &master(), "", None).expect("the row decodes");
        assert_eq!(entry.display_name, "renamed.txt");
        let key = entry.key.expect("core resolved the name key");
        assert_eq!(
            beebeeb_core::encrypt::decrypt_name_metadata_with_key(&key, &sealed)
                .unwrap()
                .name,
            "renamed.txt"
        );
    }

    /// N1 (P2): a v2-pinned file whose name envelope the server swapped for
    /// plain text carries no readable pin. Reading it as `Unrecorded` would let
    /// the server re-enable frame-0 detection for a v2 file (core round 3,
    /// P1-B); pull refuses such a row, so a WebDAV GET refuses it too.
    #[tokio::test]
    async fn a_v2_file_whose_name_is_plain_text_is_refused_on_read() {
        use beebeeb_core::chunk_stream::{ChunkEncryptor, ExpectedChunkFormat};
        const PLAINTEXT_NAME: &str = "owned.txt";
        let key = beebeeb_core::kdf::derive_file_key(&master(), OWNED.as_bytes());
        let plaintext = b"v2 content under an owner key";
        let mut row = file_row(OWNED, PLAINTEXT_NAME, &key, plaintext);
        // The server swapped the authenticated name for plain text.
        row["name_encrypted"] = json!(PLAINTEXT_NAME);
        let mut enc = ChunkEncryptor::from_reader_with_file_key(
            key,
            OWNED,
            plaintext.len() as u64,
            plaintext.len() as u64,
            std::io::Cursor::new(plaintext.to_vec()),
        )
        .unwrap()
        .with_recorded_pin(ExpectedChunkFormat::V2)
        .unwrap();
        let mut frame = Vec::new();
        while let Some(chunk) = enc.next_chunk().unwrap() {
            frame.extend_from_slice(&chunk.data);
        }
        enc.finish().unwrap();
        let m = mock(vec![row], frame).await;

        let response = get_response(&m.state, &format!("/{PLAINTEXT_NAME}"), &HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = body_text(response).await;
        assert!(
            body.contains("refusing to guess") && body.contains(OWNED),
            "the plaintext name carries no pin, so the read refuses: {body}"
        );
        assert_eq!(m.downloads.load(Ordering::SeqCst), 0, "refused before downloading");
        m.server.abort();
    }

    /// C5: the owner-wrap-of-uppercase row `bb mv` refuses
    /// (`owner_read_repair::an_owner_wrap_of_the_uppercase_key_is_read_only`) is
    /// refused here as well: 403 by its name, no probe, no write.
    ///
    /// 2275 L12: an owner item's key comes from its id, so a served L8 envelope
    /// for an owner id is ignored (and core refuses to build one); the row is
    /// read-only because its name is sealed under the uppercase repair
    /// convention, not because of a wrap.
    #[tokio::test]
    async fn an_owner_wrap_of_the_uppercase_key_is_read_only() {
        const UPPER: &str = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
        let upper = beebeeb_core::kdf::derive_file_key(&master(), UPPER.to_ascii_uppercase().as_bytes());
        let plaintext = b"SF-0002 read-only repair content";
        let mut row = file_row(UPPER, "uppercase-repair.pdf", &upper, plaintext);
        row["owner_wrapped_file_key"] = serde_json::Value::Null;
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
pub use server::{WebdavOptions, run};

mod verbs;
use verbs::*;

mod lock;
use lock::*;

mod cache;
use cache::*;

mod entry;
use entry::*;

/// A19 (P2-5): WebDAV MOVE takes the same adopt-then-move path. When the
/// source's shared ancestry cannot be read, the move is REFUSED (fail closed) and
/// no PATCH is sent — an unreadable ancestor is never "not shared".
#[cfg(test)]
mod a19_move_tests {
    use super::*;
    use axum::{Json, routing::get};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ROOT: &str = "22222222-2222-4222-8222-222222222222";
    const SRC: &str = "33333333-3333-4333-8333-333333333333";

    fn master() -> beebeeb_core::kdf::MasterKey {
        beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32])
    }

    async fn body_text(r: Response) -> String {
        String::from_utf8_lossy(&axum::body::to_bytes(r.into_body(), 4096).await.unwrap()).into_owned()
    }

    #[tokio::test]
    async fn a_move_refuses_when_shared_ancestry_cannot_be_read() {
        let patches = Arc::new(AtomicUsize::new(0));
        let patch_count = patches.clone();
        let key = beebeeb_core::kdf::derive_file_key(&master(), SRC.as_bytes());
        let name = beebeeb_core::encrypt::encrypt_name_for(
            &key,
            "file.txt",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        let root_key = beebeeb_core::kdf::derive_file_key(&master(), ROOT.as_bytes());
        let root_name = beebeeb_core::encrypt::encrypt_name_for(
            &root_key,
            "Shared",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        let app = Router::new()
            .route(
                "/api/v1/files",
                get(move || async move {
                    Json(json!({"files": [
                        {"id": SRC, "parent_id": ROOT, "name_encrypted": name, "is_folder": false},
                        {"id": ROOT, "parent_id": null, "name_encrypted": root_name, "is_folder": true}
                    ]}))
                }),
            )
            // Custody is unreadable: doubt, never an owner folder.
            .route(
                "/api/v1/shared-folders/:id",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "custody store down") }),
            )
            .route(
                "/api/v1/files/:id",
                axum::routing::patch(move || {
                    let patches = patch_count.clone();
                    async move {
                        patches.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"ok": true}))
                    }
                }),
            );
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
        let response = move_response(&state, "/Shared/file.txt", "/file.txt").await;
        server.abort();
        let status = response.status();
        let body = body_text(response).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.starts_with("move refused:"), "the refusal says why: {body}");
        assert_eq!(patches.load(Ordering::SeqCst), 0, "an unreadable ancestry sent a move");
    }
}

/// P2-6 (L13): renaming needs the key the name authenticates under. A guest's
/// folder is listed but not resolvable on this device: a rename is refused with
/// 409 and an explanation, never sealed under a derived guess and PATCHed.
#[cfg(test)]
mod rename_key_tests {
    use super::*;
    use axum::{Json, routing::get};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ROOT: &str = "22222222-2222-4222-8222-222222222222";
    const GUEST: &str = "33333333-3333-8333-8333-333333333333";

    #[tokio::test]
    async fn renaming_a_guest_folder_is_refused_without_a_patch() {
        let master = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
        let root_key = beebeeb_core::kdf::derive_file_key(&master, ROOT.as_bytes());
        let root_name = beebeeb_core::encrypt::encrypt_name_for(
            &root_key,
            "Shared",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        // The guest folder's name is sealed under a key this device never holds.
        let guest_name = beebeeb_core::encrypt::encrypt_name_for(
            &beebeeb_core::kdf::FileKey::from_bytes([160; 32]),
            "Guest",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        let patches = Arc::new(AtomicUsize::new(0));
        let patch_count = patches.clone();
        let rows = Arc::new((
            json!({"id": ROOT, "parent_id": null, "name_encrypted": root_name, "is_folder": true}),
            json!({"id": GUEST, "parent_id": ROOT, "name_encrypted": guest_name, "is_folder": true}),
        ));
        let listing_rows = rows.clone();
        let id_rows = rows.clone();
        let app = Router::new()
            // No approved legacy invites: the ancestry walk ends at the root.
            .route(
                "/api/v1/shares/invites/sent",
                get(|| async { Json(json!({"invites": []})) }),
            )
            .route(
                "/api/v1/files",
                get(move || {
                    let rows = listing_rows.clone();
                    async move { Json(json!({"files": [rows.0.clone(), rows.1.clone()]})) }
                }),
            )
            .route(
                "/api/v1/files/:id",
                get(move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let rows = id_rows.clone();
                    async move { Json(if id == ROOT { rows.0.clone() } else { rows.1.clone() }) }
                })
                .patch(move || {
                    let patches = patch_count.clone();
                    async move {
                        patches.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"ok": true}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = Arc::new(DavState {
            api: ApiClient::new_for_test(format!("http://{}", listener.local_addr().unwrap())),
            master_key: master,
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
        // The unresolved row keeps its place in the listing under a short id.
        let listed = format!("/Shared/[{}]", &GUEST[..8]);
        let response = move_response(&state, &listed, "/Shared/Guest2").await;
        server.abort();
        let status = response.status();
        let body =
            String::from_utf8_lossy(&axum::body::to_bytes(response.into_body(), 4096).await.unwrap()).into_owned();
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body, crate::owned_keys::RENAME_UNKNOWN_KEY);
        assert_eq!(patches.load(Ordering::SeqCst), 0, "a refused rename sent a PATCH");
    }
}

/// P2-7 (A9b): a request row is recognised by its request fields, so a WebDAV
/// PUT over it is saved as a NEW owner item — never a new version of the request
/// row, never sealed under the request row's id.
#[cfg(test)]
mod a9b_webdav_tests {
    use super::*;
    use axum::{
        Json,
        routing::{get, post, put},
    };
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ROOT: &str = "22222222-2222-4222-8222-222222222222";
    const REQUEST: &str = "33333333-3333-4333-8333-333333333333";

    #[tokio::test]
    async fn a_put_over_a_request_row_creates_a_new_owner_item() {
        let master = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
        let root_key = beebeeb_core::kdf::derive_file_key(&master, ROOT.as_bytes());
        let root_name = beebeeb_core::encrypt::encrypt_name_for(
            &root_key,
            "Shared",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        // P2-9: the row must RESOLVE as a request row, not merely carry the
        // fields. Build the unbound (pre-2236) envelope the R5 reader opens.
        let content = crate::crypto::key_from_bytes(&[160u8; 32]);
        let r_priv = [31u8; 32];
        let r_pub = beebeeb_core::opaque::derive_x25519_public(&r_priv);
        let (wrapped, nonce) = beebeeb_core::file_request::wrap_request_private(&master, b"", &r_priv).unwrap();
        let sealed = beebeeb_core::file_request::seal_to_request(&r_pub, b"", content.as_bytes()).unwrap();
        let request_name = beebeeb_core::encrypt::encrypt_name_for(
            &content,
            "request.txt",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let (wrapped_b64, nonce_b64) = (b64(&wrapped), b64(&nonce));
        let (e_pub_b64, wrapped_key_b64) = (b64(&sealed.e_pub), b64(&sealed.wrapped_key));
        let rows = Arc::new((
            json!({"id": ROOT, "parent_id": null, "name_encrypted": root_name, "is_folder": true}),
            json!({
                "id": REQUEST, "parent_id": ROOT, "name_encrypted": request_name, "is_folder": false,
                "size_bytes": 12, "chunk_count": 1,
                "file_request_id": REQUEST, "sender_ephemeral_pubkey": e_pub_b64,
                "wrapped_content_key": wrapped_key_b64
            }),
        ));
        let inits: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let requested: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let patches = Arc::new(AtomicUsize::new(0));
        let (listing_rows, listing_inits, init_requested) = (rows.clone(), inits.clone(), requested.clone());
        let (chunk_requested, complete_requested) = (requested.clone(), requested.clone());
        let patch_count = patches.clone();
        let app = Router::new()
            .route(
                "/api/v1/files",
                get(move || {
                    let rows = listing_rows.clone();
                    async move { Json(json!({"files": [rows.0.clone(), rows.1.clone()]})) }
                }),
            )
            .route(
                "/api/v1/uploads/init",
                post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let inits = listing_inits.clone();
                    let requested = init_requested.clone();
                    async move {
                        let id = body["file_id"].as_str().unwrap_or_default().to_string();
                        inits.lock().unwrap().push(body.clone());
                        *requested.lock().unwrap() = Some(id.clone());
                        Json(json!({
                            "upload_session_id": "sess-1",
                            "file_id": id,
                            "chunk_size_bytes": body["chunk_size_bytes"],
                            "chunk_count": body["chunk_count"],
                        }))
                    }
                }),
            )
            .route(
                "/api/v1/uploads/:session/chunks/:idx",
                put(
                    move |axum::extract::Path((_s, idx)): axum::extract::Path<(String, u32)>| {
                        let _ = chunk_requested.clone();
                        async move { Json(json!({"index": idx, "size": 1, "skipped": false})) }
                    },
                ),
            )
            .route(
                "/api/v1/uploads/:session/complete",
                post(move || {
                    let requested = complete_requested.clone();
                    async move {
                        let id = requested.lock().unwrap().clone().unwrap_or_default();
                        Json(json!({"id": id, "size_bytes": 8, "chunk_count": 1}))
                    }
                }),
            )
            .route(
                "/api/v1/files/:id/upload/abandon",
                post(|| async { Json(json!({"status": "abandoned"})) }),
            )
            .route(
                "/api/v1/shares/invites/sent",
                get(|| async { Json(json!({"invites": []})) }),
            )
            .route(
                "/api/v1/file-requests",
                get(move || {
                    let (wrapped, nonce) = (wrapped_b64.clone(), nonce_b64.clone());
                    async move {
                        Json(json!({"file_requests": [{
                            "id": REQUEST,
                            "wrapped_private_key": wrapped,
                            "wrap_nonce": nonce,
                        }]}))
                    }
                }),
            )
            .route(
                "/api/v1/files/:id",
                axum::routing::patch(move || {
                    let patches = patch_count.clone();
                    async move {
                        patches.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"ok": true}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = Arc::new(DavState {
            api: ApiClient::new_for_test(format!("http://{}", listener.local_addr().unwrap())),
            master_key: master,
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
        let response = put_response(&state, "/Shared/request.txt", b"edited body".to_vec(), None).await;
        server.abort();
        let status = response.status();
        let body =
            String::from_utf8_lossy(&axum::body::to_bytes(response.into_body(), 4096).await.unwrap()).into_owned();
        assert_eq!(status, StatusCode::CREATED, "{body}");

        let init = inits.lock().unwrap()[0].clone();
        assert_ne!(
            init["file_id"].as_str(),
            Some(REQUEST),
            "the edit is a new item, not a version of the request row"
        );
        assert_eq!(
            init["base_version_number"],
            json!(null),
            "no version of the request row: {init}"
        );
        assert_eq!(patches.load(Ordering::SeqCst), 0, "the request row was renamed");
    }
}
