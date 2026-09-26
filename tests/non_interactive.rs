//! Non-interactive (stdin is not a terminal) behaviour of the commands that
//! used to prompt — and, with no one to answer, silently did nothing while
//! exiting 0.
//!
//! Every test runs the real `bb` binary with `stdin = Stdio::null()` against a
//! throwaway in-process mock API, with `HOME` / `XDG_CONFIG_HOME` pointed at a
//! scratch dir (the developer's real config may hold a live session) and
//! `BB_NO_UPDATE=1` (no self-update call to GitHub).
//!
//! The contract under test (flow "CLI end to end", flow-6):
//! - `bb push <name>` where `<name>` already exists → exit 2 + a hint naming
//!   `--replace` / `--keep-both` (was: "skip", exit 0).
//! - `bb rm <target>` without `-f` → exit 2, nothing trashed (was: "cancelled",
//!   exit 0). `bb rm -f` still trashes.
//! - `bb unshare` with no id → exit 2 + a message pointing at `bb shares` (was:
//!   "failed to enable raw mode: Device not configured").
//! - `bb sync --once` with a failed upload or an unresolved conflict → exit 3 +
//!   a count (was: "✓ synced", exit 0).

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde_json::{Value, json};

const EXISTING_FILE_ID: &str = "11111111-1111-4111-8111-111111111111";
const SYNC_FOLDER_ID: &str = "22222222-2222-4222-8222-222222222222";
const REMOTE_CLASH_ID: &str = "33333333-3333-4333-8333-333333333333";
const SHARE_ID: &str = "44444444-4444-4444-8444-444444444444";
const EMPTY_FOLDER_ID: &str = "55555555-5555-4555-8555-555555555555";

#[derive(Clone, Default)]
struct Calls {
    trash: Arc<AtomicUsize>,
    upload_init: Arc<AtomicUsize>,
}

/// Starts the mock API on an ephemeral port; returns its base URL.
async fn start_mock(calls: Calls) -> String {
    let app = Router::new()
        .route(
            "/api/v1/files/usage",
            get(|| async { Json(json!({ "used_bytes": 0 })) }),
        )
        .route(
            "/api/v1/files/check-conflict",
            // A file named `note.txt` already exists in the target folder.
            post(|| async { Json(json!({ "files": [{ "id": EXISTING_FILE_ID, "name_encrypted": "note.txt" }] })) }),
        )
        .route(
            "/api/v1/files/trash",
            post(|State(c): State<Calls>, Json(body): Json<Value>| async move {
                c.trash.fetch_add(1, Ordering::SeqCst);
                let ids = body.get("ids").cloned().unwrap_or_else(|| json!([]));
                Json(json!({ "trashed": ids, "already_trashed": [], "missing": [] }))
            }),
        )
        .route(
            "/api/v1/files/:id",
            get(|| async { Json(json!({ "id": EXISTING_FILE_ID, "is_folder": false })) }),
        )
        .route(
            "/api/v1/files",
            // Root holds `SyncTest` (containing `clash.txt`) and the empty `SyncUp`.
            get(
                |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                    match q.get("parent_id").map(String::as_str) {
                        None => Json(json!({ "files": [
                        { "id": SYNC_FOLDER_ID, "is_folder": true, "name_encrypted": "SyncTest" },
                        { "id": EMPTY_FOLDER_ID, "is_folder": true, "name_encrypted": "SyncUp" }
                    ] })),
                        Some(SYNC_FOLDER_ID) => Json(json!({ "files": [
                        { "id": REMOTE_CLASH_ID, "is_folder": false, "name_encrypted": "clash.txt",
                          "updated_at": "2026-01-01T00:00:00Z", "chunk_count": 1 }
                    ] })),
                        Some(_) => Json(json!({ "files": [] })),
                    }
                },
            ),
        )
        .route(
            "/api/v1/uploads/init",
            // Induced upload failure.
            post(|State(c): State<Calls>| async move {
                c.upload_init.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal", "message": "induced upload failure" })),
                )
            }),
        )
        .route(
            "/api/v1/shares/mine",
            get(|| async {
                Json(json!([{ "id": SHARE_ID, "file_id": EXISTING_FILE_ID, "expires_at": null, "open_count": 0 }]))
            }),
        )
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "not_found", "message": "not found" })),
            )
        })
        .with_state(calls);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A scratch HOME holding a logged-in config that points at `api`.
fn scratch_home(api: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-nonint-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "noninteractive@beebeeb.io",
        // 32 zero bytes — a throwaway key; the mock never checks crypto.
        "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });
    // macOS (`dirs::config_dir` = $HOME/Library/Application Support) and
    // Linux ($XDG_CONFIG_HOME) — write both so the test is portable.
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    }
    home
}

/// Runs `bb <args>` with stdin = /dev/null, off the async runtime.
async fn bb(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let home = home.to_path_buf();
    let cwd = cwd.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(&args)
            .current_dir(&cwd)
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
    // Captured by the test harness; shown with `-- --nocapture` or on failure.
    let (stdout, stderr) = text(&out);
    eprintln!(
        "[exit {:?}]\n--- stdout\n{stdout}--- stderr\n{stderr}",
        out.status.code()
    );
    out
}

fn text(o: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_push_without_a_strategy_fails_with_a_hint() {
    let calls = Calls::default();
    let api = start_mock(calls.clone()).await;
    let home = scratch_home(&api);
    std::fs::write(home.join("note.txt"), b"hello non-interactive").unwrap();

    let out = bb(&home, &home, &["--api", &api, "push", "note.txt"]).await;
    let (stdout, stderr) = text(&out);

    assert_eq!(
        out.status.code(),
        Some(2),
        "duplicate push with no TTY must exit 2, not silently skip\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("already exists") && stderr.contains("--replace") && stderr.contains("--keep-both"),
        "stderr must say how to resolve it non-interactively, got: {stderr}"
    );
    assert_eq!(calls.upload_init.load(Ordering::SeqCst), 0, "nothing must be uploaded");
}

#[tokio::test(flavor = "multi_thread")]
async fn rm_without_force_refuses_and_trashes_nothing() {
    let calls = Calls::default();
    let api = start_mock(calls.clone()).await;
    let home = scratch_home(&api);

    let out = bb(&home, &home, &["--api", &api, "rm", EXISTING_FILE_ID]).await;
    let (stdout, stderr) = text(&out);

    assert_eq!(
        out.status.code(),
        Some(2),
        "rm with no TTY and no -f must exit 2, not print 'cancelled' and exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("-f"),
        "stderr must name the -f escape hatch, got: {stderr}"
    );
    assert_eq!(calls.trash.load(Ordering::SeqCst), 0, "nothing may be trashed");
}

#[tokio::test(flavor = "multi_thread")]
async fn rm_with_force_still_trashes_non_interactively() {
    let calls = Calls::default();
    let api = start_mock(calls.clone()).await;
    let home = scratch_home(&api);

    let out = bb(&home, &home, &["--api", &api, "rm", "-f", EXISTING_FILE_ID]).await;
    let (stdout, stderr) = text(&out);

    assert!(
        out.status.success(),
        "rm -f must work without a TTY\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(calls.trash.load(Ordering::SeqCst), 1, "exactly one trash batch");
}

#[tokio::test(flavor = "multi_thread")]
async fn unshare_without_an_id_points_at_bb_shares() {
    let api = start_mock(Calls::default()).await;
    let home = scratch_home(&api);

    let out = bb(&home, &home, &["--api", &api, "unshare"]).await;
    let (stdout, stderr) = text(&out);

    assert_eq!(
        out.status.code(),
        Some(2),
        "unshare with no id and no TTY must exit 2\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("bb shares"),
        "stderr must tell the user where to find a share id, got: {stderr}"
    );
    assert!(
        !stderr.contains("raw mode"),
        "must not leak the terminal raw-mode error, got: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_once_with_a_failed_upload_exits_non_zero() {
    let calls = Calls::default();
    let api = start_mock(calls.clone()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("fresh.txt"), b"a new local file").unwrap();

    let out = bb(
        &home,
        &home,
        &["--api", &api, "sync", "--once", local.to_str().unwrap(), "/SyncUp"],
    )
    .await;
    let (stdout, stderr) = text(&out);

    assert!(
        calls.upload_init.load(Ordering::SeqCst) >= 1,
        "the upload must have been attempted"
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "a sync pass with a failed upload must exit 3, not report '✓ synced'\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("1 failed"),
        "stderr must carry the failure count, got: {stderr}"
    );
    assert!(
        !stdout.contains("\u{2713} synced"),
        "must not claim a clean sync, got: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_once_with_an_unresolved_conflict_exits_non_zero() {
    let calls = Calls::default();
    let api = start_mock(calls.clone()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    // Exists on both sides with no prior sync → conflict, skipped.
    std::fs::write(local.join("clash.txt"), b"local version").unwrap();

    let out = bb(
        &home,
        &home,
        &["--api", &api, "sync", "--once", local.to_str().unwrap(), "/SyncTest"],
    )
    .await;
    let (stdout, stderr) = text(&out);

    assert_eq!(
        out.status.code(),
        Some(3),
        "a sync pass with an unresolved conflict must exit 3\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("1 conflict"),
        "stderr must carry the conflict count, got: {stderr}"
    );
}
