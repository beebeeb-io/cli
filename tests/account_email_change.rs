//! Task 1907: `bb account update --email` must never put the account password
//! in a request body for an OPAQUE account.
//!
//! Before the fix, `update_email` ran the (OPAQUE) step-up and then ALWAYS
//! called `PUT /auth/account/email` with the plaintext password; only if the
//! server answered "OPAQUE accounts must re-register" did it fall through to
//! the "use the web app" stub. So an OPAQUE user got the error AND had already
//! sent their password. The account type is now decided from the step-up
//! itself (`confirm-opaque-start` succeeded = OPAQUE; `409 opaque_unavailable`
//! = legacy), before anything carrying the password is sent.
//!
//! Runs the real `bb` binary under a pty (the password is read from
//! `/dev/tty`) against an in-process mock API whose catch-all handler records
//! EVERY request, so "the password is in no request" is asserted over the
//! whole run. Scratch `HOME` per run (never the developer's real config),
//! `BB_NO_UPDATE=1`. Unix-only (pty).
#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use beebeeb_core::opaque_protocol as op;
use serde_json::{Value, json};

const EMAIL: &str = "emailchange1907@beebeeb.io";
const NEW_EMAIL: &str = "moved1907@beebeeb.io";
/// Distinctive on purpose: the assertions grep every captured request for it.
const PASSWORD: &str = "tr0ub4dor-1907-email-change-secret";

static SEQ: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// A real OPAQUE account. The email route answers like the live server
    /// does for an OPAQUE account hit with the legacy password body.
    Opaque,
    /// `confirm-opaque-start` answers 409 `opaque_unavailable` (legacy account).
    Legacy,
}

struct Captured {
    method: String,
    uri: String,
    body: String,
}

struct Mock {
    mode: Mode,
    setup: Vec<u8>,
    file: Vec<u8>,
    log: Mutex<Vec<Captured>>,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn registered() -> &'static (Vec<u8>, Vec<u8>) {
    static R: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
    R.get_or_init(|| {
        let setup = op::create_server_setup();
        let c = op::client_registration_start(PASSWORD.as_bytes()).unwrap();
        let resp = op::server_registration_start(&setup, &c.message, EMAIL.as_bytes()).unwrap();
        let upload = op::client_registration_finish(&c.state, PASSWORD.as_bytes(), &resp).unwrap();
        let file = op::server_registration_finish(&upload).unwrap();
        (setup, file)
    })
}

async fn handle(State(m): State<Arc<Mock>>, req: Request<Body>) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().to_string();
    let path = req.uri().path().to_string();
    let bytes = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
    let body = String::from_utf8_lossy(&bytes).into_owned();
    m.log.lock().unwrap().push(Captured {
        method: method.clone(),
        uri: uri.clone(),
        body: body.clone(),
    });

    let ok = |v: Value| axum::Json(v).into_response();
    match (method.as_str(), path.as_str()) {
        ("POST", "/api/v1/auth/confirm-opaque-start") => match m.mode {
            Mode::Legacy => (
                StatusCode::CONFLICT,
                axum::Json(json!({ "opaque_unavailable": true, "message": "account has no OPAQUE credential; use /auth/confirm" })),
            )
                .into_response(),
            Mode::Opaque => {
                let req: Value = serde_json::from_str(&body).unwrap();
                let msg = b64().decode(req["client_message"].as_str().unwrap()).unwrap();
                let r = op::server_login_start(&m.setup, &m.file, &msg, EMAIL.as_bytes()).unwrap();
                ok(json!({
                    "server_message": b64().encode(&r.message),
                    "server_state": b64().encode(&r.state),
                    "ksf_version": 1,
                }))
            }
        },
        ("POST", "/api/v1/auth/confirm-opaque-finish") => {
            let req: Value = serde_json::from_str(&body).unwrap();
            let msg = b64().decode(req["client_message"].as_str().unwrap()).unwrap();
            let state = b64().decode(req["server_state"].as_str().unwrap()).unwrap();
            match op::server_login_finish(&state, &msg) {
                Ok(_) => ok(json!({ "confirmation_token": "opaque-token", "expires_at": "2099-01-01T00:00:00Z" })),
                Err(_) => (StatusCode::UNAUTHORIZED, axum::Json(json!({ "error": "unauthorized" }))).into_response(),
            }
        }
        ("POST", "/api/v1/auth/confirm") => {
            ok(json!({ "confirmation_token": "legacy-token", "expires_at": "2099-01-01T00:00:00Z" }))
        }
        ("PUT", "/api/v1/auth/account/email") => match m.mode {
            Mode::Opaque => (
                StatusCode::CONFLICT,
                axum::Json(json!({
                    "error": "conflict",
                    "message": "OPAQUE accounts must re-register to change email",
                })),
            )
                .into_response(),
            Mode::Legacy => ok(json!({ "message": "verification email sent" })),
        },
        _ => (StatusCode::NOT_FOUND, axum::Json(json!({ "error": "not_found" }))).into_response(),
    }
}

fn spawn_mock(mode: Mode) -> (String, Arc<Mock>) {
    let (setup, file) = registered().clone();
    let mock = Arc::new(Mock {
        mode,
        setup,
        file,
        log: Mutex::new(Vec::new()),
    });
    let served = mock.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = axum::Router::new().fallback(handle).with_state(served);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    (format!("http://{}", rx.recv().unwrap()), mock)
}

fn scratch_home() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("account-email-1907-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_logged_in_config(home: &Path, api_url: &str) {
    let config = json!({
        "api_url": api_url,
        "session_token": "live-session-token",
        "email": EMAIL,
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

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `bb <args>` with a controlling pty and type `typed` + Enter into it.
fn bb_with_typed_password(home: &Path, args: &[&str], typed: &str) -> Run {
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
    cmd.args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env_remove("INVOCATION_ID")
        .env_remove("XPC_SERVICE_NAME")
        .env_remove("BEEBEEB_SYNC_SERVICE")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // stdin is already dup2'd onto fd 0 when this runs: become a session leader
    // and adopt the pty as the controlling terminal, so `/dev/tty` resolves.
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
    // Dropping `cmd` closes the parent's copy of the slave end.
    drop(cmd);

    // Drain what the child writes to the terminal (the prompt, echo). Nobody
    // reading the master leaves the slave's output queue undrained, and a
    // session leader's exit then blocks on closing the tty.
    let mut tty_out = master.try_clone().unwrap();
    let drain = std::thread::spawn(move || {
        let mut sink = [0u8; 1024];
        while matches!(tty_out.read(&mut sink), Ok(n) if n > 0) {}
    });

    // Typed input waits in the line discipline until the child reads it.
    master.write_all(format!("{typed}\n").as_bytes()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bb did not exit within 120s (stuck on the password prompt?)");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    drop(master);
    let _ = drain.join();
    Run {
        code: status.code(),
        stdout,
        stderr,
    }
}

fn sequence(m: &Mock) -> Vec<String> {
    m.log
        .lock()
        .unwrap()
        .iter()
        .map(|c| format!("{} {}", c.method, c.uri.split('?').next().unwrap()))
        .collect()
}

#[test]
fn opaque_account_email_change_never_sends_the_password_and_points_to_the_web_app() {
    let (api, mock) = spawn_mock(Mode::Opaque);
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let run = bb_with_typed_password(&home, &["account", "update", "--email", NEW_EMAIL], PASSWORD);

    assert_ne!(
        run.code,
        Some(0),
        "an unsupported change must exit non-zero\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
    assert_eq!(run.code, Some(1), "stdout: {}\nstderr: {}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("Use the web app"),
        "must say to use the web app, got stderr: {}",
        run.stderr
    );
    assert_eq!(
        sequence(&mock),
        [
            "POST /api/v1/auth/confirm-opaque-start",
            "POST /api/v1/auth/confirm-opaque-finish"
        ],
        "an OPAQUE account must stop after the OPAQUE step-up: no PUT /auth/account/email, no legacy confirm"
    );
    let log = mock.log.lock().unwrap();
    assert!(!log.is_empty(), "nothing captured, the assertion would be vacuous");
    for c in log.iter() {
        assert!(!c.uri.contains(PASSWORD), "{} {} carries the password", c.method, c.uri);
        assert!(
            !c.body.contains(PASSWORD),
            "{} {} body carries the plaintext password: {}",
            c.method,
            c.uri,
            c.body
        );
    }
}

#[test]
fn legacy_account_email_change_still_uses_the_legacy_endpoint() {
    let (api, mock) = spawn_mock(Mode::Legacy);
    let home = scratch_home();
    write_logged_in_config(&home, &api);

    let run = bb_with_typed_password(&home, &["account", "update", "--email", NEW_EMAIL], PASSWORD);

    assert_eq!(run.code, Some(0), "stdout: {}\nstderr: {}", run.stdout, run.stderr);
    assert_eq!(
        sequence(&mock),
        [
            "POST /api/v1/auth/confirm-opaque-start",
            "POST /api/v1/auth/confirm",
            "PUT /api/v1/auth/account/email"
        ]
    );
    let log = mock.log.lock().unwrap();
    let put: Value = serde_json::from_str(&log[2].body).unwrap();
    assert_eq!(put["new_email"], NEW_EMAIL);
    assert_eq!(put["password"], PASSWORD, "the documented legacy exception");
    assert!(run.stdout.contains("verification email sent"), "stdout: {}", run.stdout);
}
