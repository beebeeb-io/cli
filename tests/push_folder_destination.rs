//! Task 2334: `bb push <folder>` puts everything where the command asked.
//!
//! Two defects, found on a real push of a movie folder (Guus, 2026-11-06):
//!
//! 1. Re-pushing a folder made a SECOND folder with the same name, at the top
//!    level and at every level under it: `push_directory` created each folder
//!    anew and never looked for an existing one.
//! 2. A file landed outside the folder it was pushed into. An earlier push of
//!    the same local file, interrupted, left a resume record keyed by the local
//!    path only. The re-run, into another folder, continued that session. The
//!    server fixes a file's parent when the upload is INITED and keeps it at
//!    completion, so the file completed in the earlier attempt's folder.
//!
//! Black box: the real `bb` binary against an in-process mock that, like the
//! server, keeps folders by parent, creates a file's row (with its parent) at
//! init, never changes that parent at completion, and lists a folder's
//! children. Scratch `HOME`, stdin not a terminal, loopback only.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use beebeeb_core::chunk_stream::{ExpectedChunkFormat, SessionResume, chunk_format_for_pin, resume_policy};
use serde_json::{Value, json};

const MOVIE_DIR: &str = "A House of Dynamite (2025)";
const MOVIE: &str = "A House of Dynamite (2025) (2160p).mkv";

/// A row of the mock's `files` table: a folder, or a file.
struct Item {
    id: String,
    parent: Option<String>,
    name_encrypted: String,
    is_folder: bool,
    /// Creation order, served as `created_at`.
    seq: u64,
    /// A file whose upload is open (the row exists from init on).
    uploading: bool,
    /// 0 until a version completed.
    version: i64,
    chunk_count: u32,
}

struct Session {
    file_id: String,
    base: Option<i64>,
    chunk_count: u32,
    frames: BTreeMap<u32, Vec<u8>>,
}

#[derive(Default)]
struct Vault {
    items: BTreeMap<String, Item>,
    sessions: BTreeMap<String, Session>,
    seq: u64,
    /// While set, every chunk PUT past index 0 hangs (the test then kills `bb`).
    stall: bool,
    inits: usize,
    abandons: usize,
    /// Chunk PUTs answered `skipped` (the session already held that index).
    skipped: usize,
}

type Shared = Arc<Mutex<Vault>>;

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "message": msg }))).into_response()
}

/// A row in the server's shape (`GET /files/{id}` and the listing).
fn row(i: &Item) -> Value {
    json!({
        "id": i.id,
        "parent_id": i.parent,
        "name_encrypted": i.name_encrypted,
        "is_folder": i.is_folder,
        "is_trashed": false,
        "size_bytes": 0,
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

fn parent_is_live_folder(v: &Vault, parent: Option<&str>) -> bool {
    parent.is_none_or(|p| v.items.get(p).is_some_and(|i| i.is_folder))
}

async fn start_mock(vault: Shared) -> String {
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
                    let v = v.lock().unwrap();
                    let parent = q.get("parent_id").cloned();
                    let files: Vec<Value> = v.items.values().filter(|i| i.parent == parent).map(row).collect();
                    Json(json!({ "files": files, "next_cursor": null }))
                },
            ),
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
                let parent = body.get("parent_id").and_then(Value::as_str).map(str::to_string);
                let files: Vec<Value> = v
                    .items
                    .values()
                    .filter(|i| !i.is_folder && i.parent == parent)
                    .map(|i| json!({ "id": i.id, "name_encrypted": i.name_encrypted }))
                    .collect();
                Json(json!({ "files": files }))
            }),
        )
        .route(
            "/api/v1/files/:id",
            get(|State(v): State<Shared>, UrlPath(id): UrlPath<String>| async move {
                let v = v.lock().unwrap();
                match v.items.get(&id) {
                    Some(i) => Json(row(i)).into_response(),
                    None => err(StatusCode::NOT_FOUND, "not_found"),
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
                let id = body.get("file_id").and_then(Value::as_str).unwrap_or("").to_string();
                let base = body.get("base_version_number").and_then(Value::as_i64);
                let parent = body.get("parent_id").and_then(Value::as_str).map(str::to_string);
                let chunk_size = body.get("chunk_size_bytes").and_then(Value::as_u64).unwrap_or(0);
                let chunk_count = body.get("chunk_count").and_then(Value::as_u64).unwrap_or(0) as u32;
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
                Json(json!({ "id": s.file_id, "version_number": version })).into_response()
            }),
        )
        .fallback(|| async { err(StatusCode::NOT_FOUND, "not_found") })
        // A 4 MiB chunk frame exceeds axum's 2 MB default body limit.
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024))
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
fn master() -> beebeeb_core::kdf::MasterKey {
    beebeeb_core::kdf::MasterKey::from_bytes([0u8; 32])
}

fn name_of(i: &Item) -> String {
    beebeeb_core::encrypt::decrypt_name(&master(), &i.id, &i.name_encrypted).expect("a name this test's bb sealed")
}

/// Ids of the folders named `name` directly in `parent` (`None` = the root).
fn folders_named(v: &Vault, parent: Option<&str>, name: &str) -> Vec<String> {
    v.items
        .values()
        .filter(|i| i.is_folder && i.parent.as_deref() == parent && name_of(i) == name)
        .map(|i| i.id.clone())
        .collect()
}

/// Ids of every folder named `name`, anywhere in the vault.
fn folders_named_anywhere(v: &Vault, name: &str) -> Vec<String> {
    v.items
        .values()
        .filter(|i| i.is_folder && name_of(i) == name)
        .map(|i| i.id.clone())
        .collect()
}

/// The one folder named `name` in `parent`, or a failure naming how many.
fn the_folder(v: &Vault, parent: Option<&str>, name: &str) -> String {
    let found = folders_named(v, parent, name);
    assert_eq!(
        found.len(),
        1,
        "exactly one folder named {name:?} in {parent:?}: {found:?}"
    );
    found[0].clone()
}

/// (id, parent, completed) of every file row named `name`.
fn files_named(v: &Vault, name: &str) -> Vec<(String, Option<String>, bool)> {
    v.items
        .values()
        .filter(|i| !i.is_folder && name_of(i) == name)
        .map(|i| (i.id.clone(), i.parent.clone(), i.version > 0 && !i.uploading))
        .collect()
}

/// The names of the files directly in `parent`.
fn file_names_in(v: &Vault, parent: &str) -> Vec<String> {
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

fn scratch_home(api: &str) -> PathBuf {
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

fn bb_command(home: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.env("BB_REFUSE_NON_LOOPBACK_API", "1");
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    cmd
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

async fn bb(home: &Path, args: &[&str]) -> Output {
    let mut cmd = bb_command(home, &owned(args));
    let shown = args.join(" ");
    let out = tokio::task::spawn_blocking(move || {
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run bb")
    })
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run `bb <args>` with chunk PUTs stalled until the server holds chunk 0 of a
/// session that needs more, then SIGKILL it: an interrupted push.
async fn push_and_kill(vault: &Shared, home: &Path, args: &[&str]) {
    vault.lock().unwrap().stall = true;
    let mut child = bb_command(home, &owned(args))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn bb");
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
fn two_chunks(k: u8) -> Vec<u8> {
    (0..4 * 1024 * 1024 + 1000usize)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(k).wrapping_add((i >> 12) as u8))
        .collect()
}

/// `<home>/<rel>` with `bytes`, its folders made first.
fn write_file(home: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let path = home.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

// ── Defect 1: a re-push makes a second same-name folder ─────────────────────

/// Pushing the same folder into the same place twice leaves ONE folder of that
/// name there, holding what both pushes uploaded. Before the fix,
/// `push_directory` created the pushed folder anew on every run.
#[tokio::test(flavor = "multi_thread")]
async fn repushing_a_folder_reuses_the_same_name_folder() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "first push: {}", stderr(&out));
    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "second push: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let pushed = folders_named(&v, Some(&movies), MOVIE_DIR);
    assert_eq!(
        pushed.len(),
        1,
        "a re-push must reuse the existing {MOVIE_DIR:?} in Movies, not make another: {pushed:?}"
    );
    assert_eq!(
        file_names_in(&v, &pushed[0]),
        vec![
            "A House of Dynamite (2025) (2160p) (1).mkv".to_string(),
            MOVIE.to_string()
        ],
        "both pushes' files are in that one folder (the second kept both, per --keep-both)"
    );
}

/// The same at every level under the pushed folder: a re-push that adds a new
/// episode puts it next to the first, in the ONE existing season folder.
#[tokio::test(flavor = "multi_thread")]
async fn repushing_a_folder_reuses_its_same_name_subfolders() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "Show/Season 1/e01.mkv", b"episode one");

    let out = bb(&home, &["--api", &api, "push", "--folder", "Shows", "Show"]).await;
    assert_eq!(out.status.code(), Some(0), "first push: {}", stderr(&out));
    write_file(&home, "Show/Season 1/e02.mkv", b"episode two");
    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Shows", "--keep-both", "Show"],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "second push: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let seasons = folders_named_anywhere(&v, "Season 1");
    assert_eq!(
        seasons.len(),
        1,
        "a re-push must reuse the existing nested \"Season 1\", not make another: {seasons:?}"
    );
    let names = file_names_in(&v, &seasons[0]);
    assert!(
        names.contains(&"e01.mkv".to_string()) && names.contains(&"e02.mkv".to_string()),
        "both episodes are in the one season folder: {names:?}"
    );
}

// ── Defect 2: a resumed upload lands in the earlier attempt's folder ────────

/// Guus's sequence: a push of the folder (no `--folder`) is interrupted, then
/// the same folder is pushed with `--folder Movies`. The file must complete in
/// Movies/<folder>, the folder THIS command asked for. Before the fix the
/// re-run continued the first attempt's session, whose row the server had
/// created under the first attempt's folder at the root, and the file landed
/// there.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_upload_lands_in_the_folder_this_push_asked_for() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), &two_chunks(1));

    push_and_kill(&vault, &home, &["--api", &api, "push", MOVIE_DIR]).await;
    let first_folder = {
        let v = vault.lock().unwrap();
        let first_folder = the_folder(&v, None, MOVIE_DIR);
        let files = files_named(&v, MOVIE);
        assert_eq!(files.len(), 1, "the interrupted upload's row: {files:?}");
        assert_eq!(files[0].1.as_deref(), Some(first_folder.as_str()));
        assert!(!files[0].2, "nothing completed before the kill");
        first_folder
    };

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "the push into Movies: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let wanted = the_folder(&v, Some(&movies), MOVIE_DIR);
    let completed: Vec<_> = files_named(&v, MOVIE).into_iter().filter(|f| f.2).collect();
    assert_eq!(completed.len(), 1, "one completed copy of the movie: {completed:?}");
    assert_eq!(
        completed[0].1.as_deref(),
        Some(wanted.as_str()),
        "the movie must complete in Movies/{MOVIE_DIR} ({wanted}), the folder this push asked for, \
         not in the interrupted attempt's {MOVIE_DIR} at the root ({first_folder})"
    );
    assert!(
        file_names_in(&v, &first_folder).is_empty(),
        "nothing is left in the first attempt's folder, not even an unfinished upload"
    );
}

/// The guard for the fix above: a re-run of an interrupted folder push into the
/// SAME place still resumes the upload (core's `resume_policy`), in the one
/// folder of that name. A fix that restarted every resumed upload, or that
/// made a new folder for the re-run, fails here.
#[tokio::test(flavor = "multi_thread")]
async fn rerunning_an_interrupted_folder_push_resumes_into_the_same_folder() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), &two_chunks(2));

    push_and_kill(&vault, &home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    let interrupted_id = {
        let v = vault.lock().unwrap();
        let files = files_named(&v, MOVIE);
        assert_eq!(files.len(), 1, "the interrupted upload's row: {files:?}");
        files[0].0.clone()
    };

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "the re-run: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let folder = the_folder(&v, Some(&movies), MOVIE_DIR);
    let files = files_named(&v, MOVIE);
    assert_eq!(files.len(), 1, "one movie, no copy: {files:?}");
    assert_eq!(files[0].0, interrupted_id, "the interrupted upload itself completed");
    assert_eq!(files[0].1.as_deref(), Some(folder.as_str()), "in the one {MOVIE_DIR}");
    assert!(files[0].2, "completed");
    let writing = chunk_format_for_pin(ExpectedChunkFormat::Unrecorded);
    match resume_policy(Some(writing), writing) {
        SessionResume::ContinueSession => {
            assert_eq!(v.inits, 1, "the stored session is continued, not re-inited");
            assert_eq!(v.abandons, 0);
            assert!(v.skipped >= 1, "chunk 0 the server already held is skipped");
        }
        SessionResume::RestartWithFreshSession => {
            assert_eq!(v.abandons, 1, "the old session is abandoned");
            assert_eq!(v.inits, 2, "and the same file is inited again from chunk 0");
        }
    }
}
