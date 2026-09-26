//! `bb pull` must not silently clobber an existing local file (flow "CLI end
//! to end", issue 5).
//!
//! Before the fix, `echo 'MY LOCAL EDITS' > note.txt; bb pull note.txt`
//! printed `✓ note.txt 42 B`, exited 0, and replaced the local edits with the
//! remote content — no prompt, no flag, no backup.
//!
//! Drives the real `bb` binary against an in-process mock API that serves one
//! file (`note.txt`, encrypted with the real core `ChunkEncryptor` under the
//! all-zero test master key). Every run gets its own scratch `HOME` (never the
//! developer's real config, which may hold a live session) and
//! `BB_NO_UPDATE=1`, so nothing leaves the loopback. stdin is `/dev/null`, so
//! the non-interactive path is what is exercised.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::Path as AxPath;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

const FILE_ID: &str = "7b0c1a52-3f4e-4d2a-9c61-2f8e5a1d0b37";
const REMOTE: &[u8] = b"remote content from the vault\n";
const LOCAL: &[u8] = b"MY LOCAL EDITS - precious\n";

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// Fresh scratch dir under Cargo's per-target tmp dir; returns (home, cwd).
fn scratch() -> (PathBuf, PathBuf) {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pull-overwrite-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    (home, cwd)
}

fn write_logged_in_config(home: &Path, api_url: &str) {
    let config = json!({
        "api_url": api_url,
        "session_token": "test-session-token",
        "email": "flow6@beebeeb.io",
        // 32 zero bytes, base64 — the same key the mock encrypts under.
        "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }
}

fn bb(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("run bb")
}

/// Ciphertext of `REMOTE` exactly as the server stores/returns it: one frame,
/// `nonce || ct || tag`, keyed by the file id under the zero master key.
fn remote_ciphertext() -> Vec<u8> {
    let mk = beebeeb_core::kdf::MasterKey::from_bytes([0u8; 32]);
    let mut enc = beebeeb_core::chunk_stream::ChunkEncryptor::for_push_with_chunk_size(
        &mk,
        FILE_ID,
        REMOTE.len() as u64,
        1024 * 1024,
    )
    .unwrap();
    let chunk = enc.push_chunk(REMOTE).unwrap();
    enc.finish().unwrap();
    chunk.data
}

/// Mock API serving a single file `note.txt` (plaintext-name fast path).
fn spawn_mock() -> String {
    let ciphertext = remote_ciphertext();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let meta = |AxPath(id): AxPath<String>| async move {
                if id != FILE_ID {
                    return (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response();
                }
                Json(json!({
                    "id": FILE_ID,
                    "name_encrypted": "note.txt",
                    "is_folder": false,
                    "chunk_count": 1,
                    "size_bytes": REMOTE.len(),
                }))
                .into_response()
            };
            let download = move |AxPath(id): AxPath<String>| {
                let body = ciphertext.clone();
                async move {
                    if id != FILE_ID {
                        return (StatusCode::NOT_FOUND, "not found").into_response();
                    }
                    (
                        [
                            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                            (header::HeaderName::from_static("x-chunk-count"), "1".to_string()),
                            (
                                header::HeaderName::from_static("x-original-size"),
                                REMOTE.len().to_string(),
                            ),
                        ],
                        body,
                    )
                        .into_response()
                }
            };
            let app = Router::new()
                .route("/api/v1/files/:id", get(meta))
                .route("/api/v1/files/:id/download", get(download))
                .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))) });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn setup() -> (PathBuf, PathBuf) {
    let url = spawn_mock();
    let (home, cwd) = scratch();
    write_logged_in_config(&home, &url);
    (home, cwd)
}

fn describe(out: &Output) -> String {
    format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Control: with no local file the mock round-trips, so the refusals below
/// are about the existing file — not a broken fixture.
#[test]
fn pull_into_empty_dir_writes_remote_content() {
    let (home, cwd) = setup();
    let out = bb(&home, &cwd, &["pull", FILE_ID]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(cwd.join("note.txt")).unwrap(),
        REMOTE,
        "{}",
        describe(&out)
    );
}

#[test]
fn pull_refuses_to_overwrite_existing_file_without_force() {
    let (home, cwd) = setup();
    std::fs::write(cwd.join("note.txt"), LOCAL).unwrap();

    let out = bb(&home, &cwd, &["pull", FILE_ID]);

    assert!(!out.status.success(), "expected non-zero exit\n{}", describe(&out));
    assert_eq!(
        std::fs::read(cwd.join("note.txt")).unwrap(),
        LOCAL,
        "local edits were overwritten\n{}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"), "{}", describe(&out));
    assert!(stderr.contains("--force"), "{}", describe(&out));
}

#[test]
fn pull_refuses_to_overwrite_existing_output_flag_target() {
    let (home, cwd) = setup();
    std::fs::write(cwd.join("keep.txt"), LOCAL).unwrap();

    let out = bb(&home, &cwd, &["pull", FILE_ID, "-o", "keep.txt"]);

    assert!(!out.status.success(), "expected non-zero exit\n{}", describe(&out));
    assert_eq!(
        std::fs::read(cwd.join("keep.txt")).unwrap(),
        LOCAL,
        "{}",
        describe(&out)
    );
}

#[test]
fn pull_with_force_overwrites_existing_file() {
    let (home, cwd) = setup();
    std::fs::write(cwd.join("note.txt"), LOCAL).unwrap();

    let out = bb(&home, &cwd, &["pull", FILE_ID, "--force"]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(cwd.join("note.txt")).unwrap(),
        REMOTE,
        "{}",
        describe(&out)
    );
}

#[test]
fn pull_help_documents_force() {
    let (home, cwd) = scratch();
    let out = bb(&home, &cwd, &["pull", "--help"]);
    assert!(out.status.success(), "{}", describe(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--force"), "{}", describe(&out));
}
