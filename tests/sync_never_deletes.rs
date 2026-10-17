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
/// A root folder the manifest may claim, that is NOT the one `/SyncUp` resolves to.
const OLD_ROOT_ID: &str = "99999999-9999-4999-8999-999999999999";
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

/// A remote file row. Its name is sealed under the scratch account's master
/// key (all zero bytes): a FILE's `name_encrypted` must be an encrypted envelope,
/// because the 1908 read path takes the chunk-format pin from it (task 1943).
fn remote_file(id: &str, name: &str) -> Value {
    let mk = beebeeb_core::kdf::MasterKey::from_bytes([0u8; 32]);
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(&mk, id, name, None).unwrap();
    json!({
        "id": id,
        "name_encrypted": name_encrypted,
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

/// Make the manifest claim it last synced against `OLD_ROOT_ID` (the old remote
/// folder was trashed and re-created, or the remote path changed).
fn point_manifest_at_old_root(dir: &Path) {
    let mut state: Value = serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap();
    state["remote_folder_id"] = json!(OLD_ROOT_ID);
    std::fs::write(dir.join(".bb-sync.json"), state.to_string()).unwrap();
}

/// The new root holds one unrelated file that also exists locally. It was never
/// synced, so the pass reports it as a both-sides conflict instead of
/// downloading it: the listing is not empty, and the pass finishes cleanly.
fn new_root_with_unrelated_file(dir: &Path, mock: &Mock) {
    std::fs::write(dir.join("other.txt"), b"unrelated local file").unwrap();
    mock.set_listing(vec![remote_file(&uuid::Uuid::new_v4().to_string(), "other.txt")]);
}

fn state_root(dir: &Path) -> Value {
    let v: Value = serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap();
    v["remote_folder_id"].clone()
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
    point_manifest_at_old_root(&dir);
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

/// The first pass after a root change must not quietly adopt the new folder as
/// trusted: if it did, the NEXT pass would treat the old tree's tombstones as
/// deletions in a folder those files never belonged to (#83 review, P1).
#[tokio::test(flavor = "multi_thread")]
async fn changed_remote_root_stays_blocked_on_later_passes() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    point_manifest_at_old_root(&dir);
    new_root_with_unrelated_file(&dir, &mock);

    let mut last = None;
    for pass in 1..=2 {
        let out = sync_once(&home, &api, &dir, &[]).await;
        assert_all_present(&dir, &files, &format!("pass {pass} after a root change"));
        assert!(recovered(&dir).is_empty(), "pass {pass} moved files to .bb-trash");
        last = Some(out);
    }
    assert_eq!(
        state_root(&dir),
        json!(OLD_ROOT_ID),
        "the old binding is kept until --rebind"
    );
    assert_eq!(manifest_keys(&dir), vec!["f0.txt", "f1.txt", "f2.txt"]);
    assert!(mock.trashed().is_empty());
    let last = last.unwrap();
    assert!(
        text(&last).contains("--rebind"),
        "the notice must name the way out, got: {}",
        text(&last)
    );
}

/// `--rebind` is the explicit way out of a root change: it adopts the new
/// folder and forgets the old tree's manifest, so nothing that belonged there
/// can be removed on a later pass either.
#[tokio::test(flavor = "multi_thread")]
async fn rebind_adopts_the_new_root_and_removes_nothing() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    point_manifest_at_old_root(&dir);
    new_root_with_unrelated_file(&dir, &mock);

    // The mock has no upload routes, so re-uploading f0..f2 into the new folder
    // fails and the exit code says nothing here; the state on disk does.
    let _ = sync_once(&home, &api, &dir, &["--rebind"]).await;
    assert_all_present(&dir, &files, "--rebind");
    assert!(recovered(&dir).is_empty());
    assert_eq!(
        state_root(&dir),
        json!(SYNC_FOLDER_ID),
        "--rebind adopts the new folder"
    );
    let old_ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    let state: Value = serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap();
    for (rel, entry) in state["files"].as_object().unwrap() {
        assert!(
            !old_ids.contains(&entry["remote_id"].as_str().unwrap()),
            "{rel} still points at the old tree after --rebind"
        );
    }

    // A plain pass afterwards: no root complaint, and still nothing removed.
    let out = sync_once(&home, &api, &dir, &[]).await;
    assert_all_present(&dir, &files, "the pass after --rebind");
    assert!(recovered(&dir).is_empty());
    assert!(
        !text(&out).contains("last synced with"),
        "the root is reconciled, got: {}",
        text(&out)
    );
}

/// The continuous watch loop resyncs in the SAME process right after the first
/// pass, so it is the first place a freshly adopted root would be trusted. Its
/// resync after a root change must remove nothing either (#83 review, P1).
#[tokio::test(flavor = "multi_thread")]
async fn watch_resync_after_root_change_removes_nothing() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    point_manifest_at_old_root(&dir);
    new_root_with_unrelated_file(&dir, &mock);

    let log_path = home.join("watch.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(["--api", &api, "sync", dir.to_str().unwrap(), "/SyncUp"])
        .current_dir(&home)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn bb sync");
    let read_log = || std::fs::read_to_string(&log_path).unwrap_or_default();
    let any_gone = || files.iter().any(|f| !dir.join(&f.rel).exists());
    let wait = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    };

    let watching = wait(&|| read_log().contains("watching for changes"));
    // A new local file wakes the watcher; its resync re-plans every absence.
    std::fs::write(dir.join("trigger.txt"), b"wake the watcher").unwrap();
    // Decided: the resync printed its own "kept ... missing" verdict (the first
    // came from the initial pass), or it removed something.
    let decided =
        watching && wait(&|| read_log().matches("missing from the remote listing").count() >= 2 || any_gone());
    let _ = child.kill();
    let _ = child.wait();

    assert!(watching, "bb sync never reached watch mode:\n{}", read_log());
    assert!(decided, "the watch resync never ran:\n{}", read_log());
    assert_all_present(&dir, &files, "watch-loop resync after a root change");
    assert!(recovered(&dir).is_empty(), "the watch loop moved files to .bb-trash");
}

/// An explicit `--delete` after a root change trashes nothing: the manifest's
/// "synced before, now missing locally" entries describe the OLD tree, so a
/// same-named file in the new folder is not the file the user deleted. The pass
/// exits 3 naming `--rebind`; after `--rebind`, `--delete` works as before.
#[tokio::test(flavor = "multi_thread")]
async fn delete_after_root_change_trashes_nothing_until_rebind() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    point_manifest_at_old_root(&dir);
    // The new folder has a DIFFERENT file named f1.txt, and f1.txt is missing
    // locally: exactly what `--delete` would trash on a trusted root.
    let new_f1 = uuid::Uuid::new_v4().to_string();
    mock.set_listing(vec![remote_file(&new_f1, "f1.txt")]);
    std::fs::remove_file(dir.join("f1.txt")).unwrap();
    // The --rebind pass downloads the new f1.txt: serve undecryptable bytes so
    // that is a reported failure (the pass still saves its state), not the
    // hard "empty body" error that aborts before the rebind is written.
    *mock.download_body.lock().unwrap() = vec![0x42; 64];

    let out = sync_once(&home, &api, &dir, &["--delete"]).await;
    assert!(
        mock.trashed().is_empty(),
        "--delete on a changed root trashed {:?}",
        mock.trashed()
    );
    for f in [&files[0], &files[2]] {
        assert_eq!(
            std::fs::read(dir.join(&f.rel)).ok(),
            Some(f.bytes.clone()),
            "{} was removed or changed by --delete after a root change",
            f.rel
        );
    }
    assert!(recovered(&dir).is_empty());
    assert_eq!(out.status.code(), Some(3), "a refused --delete must not exit 0");
    assert!(
        text(&out).contains("--delete held back") && text(&out).contains("bb sync --rebind"),
        "must say --delete was refused and name the way out, got: {}",
        text(&out)
    );

    // Reconcile, then let the new folder's f1.txt be a file this directory synced.
    let _ = sync_once(&home, &api, &dir, &["--rebind"]).await;
    // Keep the last pass about f1 alone (the mock has no upload routes for f0/f2).
    for rel in ["f0.txt", "f1.txt", "f2.txt"] {
        let _ = std::fs::remove_file(dir.join(rel));
    }
    let mut state: Value = serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap();
    state["files"]["f1.txt"] = json!({
        "remote_id": new_f1,
        "last_mtime": 0,
        "last_size": 5,
        "last_sync": "2026-10-01T00:00:00Z",
    });
    std::fs::write(dir.join(".bb-sync.json"), state.to_string()).unwrap();

    let _ = sync_once(&home, &api, &dir, &["--delete"]).await;
    assert_eq!(
        mock.trashed(),
        vec![new_f1],
        "after --rebind, --delete trashes the file deleted locally, as before"
    );
}

/// After a root change, a locally EDITED file whose name also exists in the new
/// folder is a both-sides conflict: the old manifest entry describes a file of
/// the old tree, so it must not drive a trash-then-upload replace of the new
/// folder's unrelated file (#83 review).
#[tokio::test(flavor = "multi_thread")]
async fn local_edit_after_root_change_is_a_conflict_not_a_replace() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, _files) = synced_dir(&home, &mock, 3, |_| Some(true));
    point_manifest_at_old_root(&dir);
    let new_f1 = uuid::Uuid::new_v4().to_string();
    mock.set_listing(vec![remote_file(&new_f1, "f1.txt")]);
    let edit = b"edited locally after the last sync".to_vec();
    std::fs::write(dir.join("f1.txt"), &edit).unwrap();

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert!(
        mock.trashed().is_empty(),
        "the new folder's f1.txt was trashed for an old-tree edit: {:?}",
        mock.trashed()
    );
    assert_eq!(
        std::fs::read(dir.join("f1.txt")).unwrap(),
        edit,
        "the local edit stays local"
    );
    assert!(
        text(&out).contains("f1.txt") && text(&out).contains("exists both sides"),
        "f1.txt must be reported as a conflict, got: {}",
        text(&out)
    );
    assert_eq!(out.status.code(), Some(3), "an unresolved conflict is not a clean pass");
}

/// After a root change, a NEWER same-named file in the new folder is not
/// downloaded over the local file the old manifest vouches for (#83 review).
#[tokio::test(flavor = "multi_thread")]
async fn newer_remote_after_root_change_is_not_downloaded_over_local() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let (dir, files) = synced_dir(&home, &mock, 3, |_| Some(true));
    point_manifest_at_old_root(&dir);
    let new_f1 = uuid::Uuid::new_v4().to_string();
    let mut newer = remote_file(&new_f1, "f1.txt");
    newer["updated_at"] = json!("2026-10-10T00:00:00+00:00"); // after the manifest's last_sync
    mock.set_listing(vec![newer]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert!(
        mock.downloads.lock().unwrap().is_empty(),
        "the new folder's f1.txt was downloaded over the local copy: {:?}",
        mock.downloads.lock().unwrap()
    );
    assert_all_present(&dir, &files, "a newer same-named remote file after a root change");
    assert!(
        text(&out).contains("exists both sides"),
        "f1.txt must be reported as a conflict, got: {}",
        text(&out)
    );
}

// ───────────────────────────── 1943 ─────────────────────────────

/// A remote file whose `name_encrypted` is a bare plaintext string has no
/// authenticated metadata, so no chunk-format pin: its content is never read
/// (nothing downloaded, nothing written), it is reported, the remote copy is
/// left alone, and the pass exits 3 like any unreadable download.
#[tokio::test(flavor = "multi_thread")]
async fn a_remote_file_without_authenticated_metadata_is_not_downloaded() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let dir = home.join("syncdir");
    std::fs::create_dir_all(&dir).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let mut row = remote_file(&id, "no-pin.txt");
    row["name_encrypted"] = json!("no-pin.txt");
    mock.set_listing(vec![row]);

    let out = sync_once(&home, &api, &dir, &[]).await;

    assert!(mock.downloads.lock().unwrap().is_empty(), "content read without a pin");
    assert!(!dir.join("no-pin.txt").exists());
    assert!(mock.trashed().is_empty(), "the remote copy must be left alone");
    assert_eq!(out.status.code(), Some(3), "the unreadable file must be reported");
    assert!(
        text(&out).contains("no-pin.txt") && text(&out).contains("refusing to guess"),
        "got: {}",
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
