//! Tasks 1915 + 1916 (deletion-safety spec 2026-10-17, rules R4/R5): `bb sync`
//! never deletes a local file just because it is absent from a remote listing,
//! and a download that fails to decrypt never trashes the remote copy.
//!
//! Black box: the real `bb` binary against an in-process mock API, with a
//! scratch `HOME` (the developer's real config holds a live session).
//!
//! What the mock lets a test control:
//! - the listing of the synced folder (`listing`),
//! - the per-ID record behind `GET /files/{id}` (`records`; absent => 404). A
//!   record with `is_trashed: true` is the server's positive deletion record,
//! - what `GET /files/{id}/download` returns,
//!
//! The mock also records every `DELETE /files/{id}` (a remote trash) and every download.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SYNC_FOLDER_ID: &str = "22222222-2222-4222-8222-222222222222";
const LONG_AGO: &str = "2020-01-01T00:00:00+00:00";

#[derive(Clone, Default)]
struct Mock {
    /// Children of the synced folder, as `GET /files?parent_id=<sync folder>` returns them.
    listing: Arc<Mutex<Vec<Value>>>,
    /// `GET /files/{id}`: present => 200 with this body, absent => 404.
    records: Arc<Mutex<HashMap<String, Value>>>,
    /// Every id the client sent `DELETE /files/{id}` for (a remote trash).
    trash_calls: Arc<Mutex<Vec<String>>>,
    /// Every id the client asked `/download` for.
    downloads: Arc<Mutex<Vec<String>>>,
    /// Body served by `/download` (garbage by default: cannot be decrypted).
    download_body: Arc<Mutex<Vec<u8>>>,
}

impl Mock {
    fn set_listing(&self, items: Vec<Value>) {
        *self.listing.lock().unwrap() = items;
    }
    fn add_record(&self, id: &str, trashed: bool) {
        self.records.lock().unwrap().insert(
            id.to_string(),
            json!({ "id": id, "is_folder": false, "is_trashed": trashed }),
        );
    }
    fn trashed(&self) -> Vec<String> {
        self.trash_calls.lock().unwrap().clone()
    }
}

fn remote_file(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name_encrypted": name,
        "is_folder": false,
        "size_bytes": 5,
        "chunk_count": 1,
        "updated_at": LONG_AGO,
    })
}

async fn start_mock(mock: Mock) -> String {
    let app = Router::new()
        .route(
            "/api/v1/files",
            get(
                |State(m): State<Mock>, Query(q): Query<HashMap<String, String>>| async move {
                    match q.get("parent_id").map(String::as_str) {
                        None => Json(json!({ "files": [
                            { "id": SYNC_FOLDER_ID, "is_folder": true, "name_encrypted": "SyncUp" }
                        ] })),
                        Some(SYNC_FOLDER_ID) => Json(json!({ "files": m.listing.lock().unwrap().clone() })),
                        Some(_) => Json(json!({ "files": [] })),
                    }
                },
            ),
        )
        .route(
            "/api/v1/files/:id",
            get(|State(m): State<Mock>, UrlPath(id): UrlPath<String>| async move {
                match m.records.lock().unwrap().get(&id) {
                    Some(v) => Json(v.clone()).into_response(),
                    None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))).into_response(),
                }
            })
            .delete(|State(m): State<Mock>, UrlPath(id): UrlPath<String>| async move {
                m.trash_calls.lock().unwrap().push(id);
                Json(json!({ "ok": true }))
            }),
        )
        .route(
            "/api/v1/files/:id/download",
            get(|State(m): State<Mock>, UrlPath(id): UrlPath<String>| async move {
                m.downloads.lock().unwrap().push(id);
                m.download_body.lock().unwrap().clone()
            }),
        )
        .fallback(|| async {
            let r: Response = (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_found", "message": "not found" })),
            )
                .into_response();
            r
        })
        .with_state(mock);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn scratch_home(api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-nodel-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "sync-safety@beebeeb.io",
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

fn sha_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// One previously-synced file: written to disk AND recorded in `.bb-sync.json`.
struct Synced {
    rel: String,
    id: String,
    bytes: Vec<u8>,
}

/// A sync dir that last synced `n` files (`f0.txt`..) against `SYNC_FOLDER_ID`.
/// The mock gets a record for each id (`trashed` says which are tombstoned).
fn synced_dir(home: &Path, mock: &Mock, n: usize, trashed: impl Fn(usize) -> Option<bool>) -> (PathBuf, Vec<Synced>) {
    let dir = home.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    let mut manifest = serde_json::Map::new();
    for i in 0..n {
        let rel = format!("f{i}.txt");
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = format!("content of file number {i}").into_bytes();
        std::fs::write(dir.join(&rel), &bytes).unwrap();
        manifest.insert(
            rel.clone(),
            json!({
                "remote_id": id,
                "last_mtime": 0,
                "last_size": bytes.len(),
                "last_sync": "2026-10-01T00:00:00Z",
                "content_hash": sha_hex(&bytes),
            }),
        );
        // None => no record at all (the server answers 404).
        if let Some(t) = trashed(i) {
            mock.add_record(&id, t);
        }
        files.push(Synced { rel, id, bytes });
    }
    let state = json!({
        "remote_path": "/SyncUp",
        "remote_folder_id": SYNC_FOLDER_ID,
        "last_sync": "2026-10-01T00:00:00Z",
        "files": Value::Object(manifest),
    });
    std::fs::write(dir.join(".bb-sync.json"), state.to_string()).unwrap();
    (dir, files)
}

fn manifest_keys(dir: &Path) -> Vec<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap();
    let mut keys: Vec<String> = v["files"].as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

async fn bb(home: &Path, args: &[&str]) -> Output {
    let home = home.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(&args)
            .current_dir(&home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run bb")
    })
    .await
    .unwrap();
    eprintln!(
        "[exit {:?}]\n--- stdout\n{}--- stderr\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

async fn sync_once(home: &Path, api: &str, dir: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["--api", api, "sync", "--once"];
    args.extend_from_slice(extra);
    args.push(dir.to_str().unwrap());
    args.push("/SyncUp");
    bb(home, &args).await
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// Every file under `<dir>/.bb-trash/**` (the recoverable folder), as (name, bytes).
fn recovered(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let root = dir.join(".bb-trash");
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

fn assert_all_present(dir: &Path, files: &[Synced], ctx: &str) {
    for f in files {
        let p = dir.join(&f.rel);
        assert!(p.exists(), "{} was deleted locally ({ctx})", f.rel);
        assert_eq!(std::fs::read(&p).unwrap(), f.bytes, "{} content changed ({ctx})", f.rel);
    }
}

// ───────────────────────────── 1915 ─────────────────────────────

/// Even when the server says every file is trashed, an EMPTY listing is never
/// propagated as "delete everything" (spec R4/R5).
#[tokio::test(flavor = "multi_thread")]
async fn empty_listing_deletes_nothing_locally() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    mock.set_listing(vec![]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert_all_present(&dir, &files, "empty remote listing");
    assert_eq!(manifest_keys(&dir).len(), 3, "manifest must keep its entries");
    assert!(mock.trashed().is_empty(), "sync must not trash anything remotely");
    assert!(
        text(&out).contains("empty"),
        "the held-back deletion must be explained, got: {}",
        text(&out)
    );
}

/// A listing in which entries could not be decrypted is not a complete listing:
/// the names we cannot read might be exactly the files we think are gone.
#[tokio::test(flavor = "multi_thread")]
async fn listing_with_unreadable_entries_deletes_nothing() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    // Two entries whose names do not decrypt (wrong-key shape), none of ours.
    mock.set_listing(vec![
        json!({ "id": uuid::Uuid::new_v4().to_string(), "name_encrypted": "{not-a-blob", "is_folder": false, "chunk_count": 1, "updated_at": LONG_AGO }),
        json!({ "id": uuid::Uuid::new_v4().to_string(), "name_encrypted": "{also-not", "is_folder": false, "chunk_count": 1, "updated_at": LONG_AGO }),
    ]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert_all_present(&dir, &files, "listing with undecryptable names");
    assert!(mock.trashed().is_empty());
    assert!(
        text(&out).contains("could not be read"),
        "must say why nothing was removed, got: {}",
        text(&out)
    );
}

/// A listing that is merely missing entries (a short or paginated-but-incomplete
/// response) proves nothing: the server still has those files, or does not know
/// them at all (404 - e.g. another account). Neither is a deletion record.
#[tokio::test(flavor = "multi_thread")]
async fn partial_listing_deletes_nothing() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    // f0: present in the listing. f1: alive but missing from it. f2: unknown (404).
    let (dir, files) = synced_dir(&home, &mock, 3, |i| match i {
        1 => Some(false),
        _ => None,
    });
    mock.set_listing(vec![remote_file(&files[0].id, "f0.txt")]);

    let _ = sync_once(&home, &api, &dir, &[]).await;

    assert_all_present(&dir, &files, "partial listing");
    assert_eq!(manifest_keys(&dir).len(), 3, "no manifest entry may be dropped");
    assert!(mock.trashed().is_empty());
    assert!(recovered(&dir).is_empty(), "nothing may be moved aside either");
}

/// Only the file with a positive server tombstone goes, and it goes to a
/// recoverable folder with its bytes intact - never a plain unlink.
#[tokio::test(flavor = "multi_thread")]
async fn tombstone_removes_only_that_file_and_keeps_it_recoverable() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    // f0: tombstoned. f1: alive but missing from the listing. f2, f3: present.
    let (dir, files) = synced_dir(&home, &mock, 4, |i| match i {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    });
    mock.set_listing(vec![
        remote_file(&files[2].id, "f2.txt"),
        remote_file(&files[3].id, "f3.txt"),
    ]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert!(
        !dir.join("f0.txt").exists(),
        "the tombstoned file must leave the sync folder"
    );
    assert_all_present(&dir, &files[1..], "only f0 had a tombstone");
    let rec = recovered(&dir);
    assert_eq!(
        rec,
        vec![("f0.txt".to_string(), files[0].bytes.clone())],
        "f0 must be recoverable, byte for byte"
    );
    assert_eq!(manifest_keys(&dir), vec!["f1.txt", "f2.txt", "f3.txt"]);
    assert!(mock.trashed().is_empty(), "a local removal never trashes remotely");
    assert!(out.status.success(), "exit 0 expected, got {:?}", out.status.code());
}

/// 205 tombstones in one pass is a mass delete: nothing is removed, exit 3, and
/// only an explicit flag lets it through (still to the recoverable folder).
#[tokio::test(flavor = "multi_thread")]
async fn two_hundred_tombstones_trip_the_breaker() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    // f0..f4 present remotely; f5..f209 tombstoned.
    let (dir, files) = synced_dir(&home, &mock, 210, |i| if i >= 5 { Some(true) } else { None });
    mock.set_listing((0..5).map(|i| remote_file(&files[i].id, &files[i].rel)).collect());

    let out = sync_once(&home, &api, &dir, &[]).await;
    assert_all_present(&dir, &files, "mass-delete breaker");
    assert_eq!(manifest_keys(&dir).len(), 210);
    assert_eq!(out.status.code(), Some(3), "a held-back mass delete must not exit 0");
    assert!(
        text(&out).contains("--allow-mass-delete"),
        "the message must name the explicit confirmation, got: {}",
        text(&out)
    );
    assert!(recovered(&dir).is_empty());

    // The explicit confirmation: removes them, but into the recoverable folder.
    let out = sync_once(&home, &api, &dir, &["--allow-mass-delete"]).await;
    assert!(
        out.status.success(),
        "confirmed mass delete should finish, got {:?}",
        out.status.code()
    );
    assert_all_present(&dir, &files[..5], "the five live files stay");
    assert_eq!(recovered(&dir).len(), 205, "all 205 removals must be recoverable");
    assert!(mock.trashed().is_empty());
}

/// The directory last synced against a different remote folder (the old one was
/// trashed and re-created, or the remote path changed). Its files are all
/// reported trashed - and still nothing may be removed on that account.
#[tokio::test(flavor = "multi_thread")]
async fn changed_remote_root_deletes_nothing() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    // The manifest says these files lived under some OTHER root folder.
    let mut state: Value = serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap();
    state["remote_folder_id"] = json!("99999999-9999-4999-8999-999999999999");
    std::fs::write(dir.join(".bb-sync.json"), state.to_string()).unwrap();
    // The (new) root has an unrelated file, so the listing is not empty.
    mock.set_listing(vec![remote_file(&uuid::Uuid::new_v4().to_string(), "other.txt")]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert_all_present(&dir, &files, "remote root changed");
    assert!(recovered(&dir).is_empty());
    assert!(
        text(&out).contains("last synced with"),
        "must say why, got: {}",
        text(&out)
    );
}

// ───────────────────────────── 1916 ─────────────────────────────

/// A download that cannot be decrypted leaves the remote copy alone, reports
/// the error (exit 3), writes no local file and no manifest entry, and is
/// attempted again on the next pass.
#[tokio::test(flavor = "multi_thread")]
async fn decrypt_failure_leaves_the_remote_untouched() {
    let mock = Mock::default();
    *mock.download_body.lock().unwrap() = vec![0x42; 64];
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let dir = home.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    mock.set_listing(vec![remote_file(&id, "remote-only.txt")]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert!(
        mock.trashed().is_empty(),
        "a failed decrypt must NEVER trash the remote copy, trashed: {:?}",
        mock.trashed()
    );
    assert_eq!(mock.downloads.lock().unwrap().len(), 1, "the download was attempted");
    assert!(!dir.join("remote-only.txt").exists(), "no partial local file");
    assert!(
        manifest_keys(&dir).is_empty(),
        "no manifest entry for a failed download"
    );
    assert_eq!(out.status.code(), Some(3), "the failure must be reported, not exit 0");
    assert!(
        text(&out).contains("remote-only.txt"),
        "the error must name the file, got: {}",
        text(&out)
    );

    // Retried later: the next pass tries again, still never trashing.
    let _ = sync_once(&home, &api, &dir, &[]).await;
    assert_eq!(
        mock.downloads.lock().unwrap().len(),
        2,
        "next pass retries the download"
    );
    assert!(mock.trashed().is_empty());
}
