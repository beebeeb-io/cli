//! End-to-end coverage for human auth/session/connection errors (flow "CLI
//! end to end", issue 6).
//!
//! Drives the real `bb` binary against an in-process mock API. Every run gets
//! its own scratch `HOME` (never the developer's real config, which may hold a
//! live session) and `BB_NO_UPDATE=1`, so nothing leaves the loopback.
//!
//! Before the fix:
//! - a revoked token made `bb ls` / `bb quota` print `error: unauthorized`
//!   with no hint how to recover;
//! - `bb whoami` on a revoked token printed placeholder data ("user unknown,
//!   plan Free — 5 GB") and exited 0 — a paying user would be told they are on
//!   Free;
//! - logged-out `bb whoami` exited 0;
//! - an unreachable server printed reqwest's raw error chain.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use axum::{Json, Router};
use serde_json::json;

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// Fresh scratch HOME under Cargo's per-target tmp dir.
fn scratch_home() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("auth-errors-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a config holding a (server-side revoked) session token into every
/// place `dirs::config_dir()` may resolve to under `home`.
fn write_logged_in_config(home: &Path, api_url: &str) {
    let config = json!({
        "api_url": api_url,
        "session_token": "revoked-session-token",
        "email": "flow6@beebeeb.io",
        // 32 zero bytes, base64 — a syntactically valid master key.
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

fn bb(home: &Path, args: &[&str]) -> Output {
    bb_env(home, args, &[])
}

/// Like [`bb`], with service-detection env vars cleared then `extra` applied,
/// so results do not depend on the host (systemd/GNOME set INVOCATION_ID).
fn bb_env(home: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env_remove("INVOCATION_ID")
        .env_remove("XPC_SERVICE_NAME")
        .env_remove("BEEBEEB_SYNC_SERVICE")
        .envs(extra.iter().copied())
        .output()
        .expect("run bb")
}

/// Mock API that answers every route with the server's generic 401 body.
fn spawn_unauthorized_mock() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new()
                .fallback(|| async { (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))) });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn revoked_session_ls_tells_the_user_to_run_bb_login() {
    let api = spawn_unauthorized_mock();
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let out = bb(&home, &["ls"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("bb login"),
        "ls on a revoked session must point at `bb login`: {stderr}"
    );
    assert!(
        stderr.contains("session expired or was revoked"),
        "ls on a revoked session must say what happened: {stderr}"
    );
}

#[test]
fn revoked_session_quota_tells_the_user_to_run_bb_login() {
    let api = spawn_unauthorized_mock();
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let out = bb(&home, &["quota"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("bb login"),
        "quota on a revoked session must point at `bb login`: {stderr}"
    );
}

#[test]
fn revoked_session_whoami_fails_and_never_shows_placeholder_plan_data() {
    let api = spawn_unauthorized_mock();
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let out = bb(&home, &["whoami"]);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.contains("bb login"),
        "whoami on a revoked session must point at `bb login`: {stderr}"
    );
    assert!(!stdout.contains("Free"), "whoami must not invent a Free plan: {stdout}");
    assert!(
        !stdout.contains("unknown"),
        "whoami must not print placeholder identity: {stdout}"
    );
}

#[test]
fn revoked_session_status_fails_too() {
    let api = spawn_unauthorized_mock();
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let out = bb(&home, &["status"]);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("bb login"), "{stderr}");
    assert!(!stdout.contains("Free"), "{stdout}");
}

#[test]
fn logged_out_whoami_exits_nonzero_with_a_login_hint() {
    let home = scratch_home();

    let out = bb(&home, &["whoami"]);
    let stderr = text(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "logged-out whoami must exit 1; stderr: {stderr}"
    );
    assert!(stderr.contains("Not logged in"), "{stderr}");
    assert!(stderr.contains("bb login"), "{stderr}");
}

#[test]
fn unreachable_server_gets_a_friendly_message_not_a_reqwest_chain() {
    // Grab a free port, then close it so the connect is refused.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let api = format!("http://127.0.0.1:{port}");
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let out = bb(&home, &["ls"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(&format!("Can't reach {api}")),
        "connection refused must name the server it could not reach: {stderr}"
    );
    assert!(stderr.contains("--api"), "must hint at --api: {stderr}");
    assert!(
        !stderr.contains("tcp connect error") && !stderr.contains("os error"),
        "must not dump the raw reqwest error chain: {stderr}"
    );
}

/// Mock that counts requests to /clients/devices and answers 401 to all.
#[test]
fn sync_on_dead_session_exits_77_once_without_retry() {
    use std::sync::Arc;
    let hits = Arc::new(AtomicUsize::new(0));
    let h2 = hits.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new().fallback(move || {
                let h = h2.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })))
                }
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    let api = format!("http://{}", rx.recv().unwrap());
    let home = scratch_home();
    write_logged_in_config(&home, &api);
    let dir = home.join("vault");
    std::fs::create_dir_all(&dir).unwrap();

    let out = bb(&home, &["sync", dir.to_str().unwrap(), "/vault", "--once"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(77), "stderr: {stderr}");
    assert!(
        stderr.contains("Your session has ended. Run `bb login` to sign in again."),
        "{stderr}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "401 must not be retried");
    let _ = std::fs::remove_dir_all(&home);
}

/// systemd sets INVOCATION_ID for every unit and children inherit it (GitHub
/// runners, GNOME terminals): it must not select the "under a service" message.
#[test]
fn invocation_id_alone_does_not_select_the_service_message() {
    let api = spawn_unauthorized_mock();
    let home = scratch_home();
    write_logged_in_config(&home, &api);
    let dir = home.join("vault");
    std::fs::create_dir_all(&dir).unwrap();
    let args = ["sync", dir.to_str().unwrap(), "/vault", "--once"];

    let out = bb_env(&home, &args, &[("INVOCATION_ID", "abc123")]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(77), "stderr: {stderr}");
    assert!(stderr.contains("Run `bb login` to sign in again."), "{stderr}");
    assert!(!stderr.contains("--daemon"), "{stderr}");

    let out = bb_env(&home, &args, &[("BEEBEEB_SYNC_SERVICE", "1")]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(77), "stderr: {stderr}");
    assert!(stderr.contains("`bb sync --daemon`"), "{stderr}");
    let _ = std::fs::remove_dir_all(&home);
}
