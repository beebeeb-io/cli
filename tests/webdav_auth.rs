//! Task 1790 / finding M31: `bb webdav` serves the DECRYPTED vault on a loopback port.
//! Before this change it answered any request: any Host (DNS rebinding), any Origin
//! (a hostile web page writing with the user's session), no credential (any local
//! account), and read-write by default.
//!
//! These tests run the real `bb` binary against a scratch HOME (the real config holds a
//! live session and master key) with a fake session + random master key. The vault API is
//! an in-process mock; the mock counts the requests that reach it so a refused write is
//! proven to have done nothing, not merely to have returned 403.

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use axum::Router;
use axum::routing::{any, get, post};
use base64::Engine as _;

/// Requests that reached the mock vault API, by any route.
type Hits = Arc<AtomicUsize>;

async fn spawn_mock_api() -> (String, Hits) {
    let hits: Hits = Arc::new(AtomicUsize::new(0));
    let list_hits = hits.clone();
    let folder_hits = hits.clone();
    let other_hits = hits.clone();
    let app = Router::new()
        .route(
            "/api/v1/files",
            get(move || {
                let h = list_hits.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({ "files": [] }))
                }
            }),
        )
        .route(
            "/api/v1/files/folder",
            post(move || {
                let h = folder_hits.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({ "id": "44444444-4444-4444-8444-444444444444" }))
                }
            }),
        )
        .fallback(any(move || {
            let h = other_hits.clone();
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "mock: unhandled")
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, hits)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("webdav-auth-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Fake login in the scratch HOME. `dirs::config_dir()` is `$HOME/Library/Application
/// Support` on macOS and `$XDG_CONFIG_HOME` on Linux; write both.
fn write_fake_session(home: &Path, api_url: &str) {
    let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    let json = serde_json::json!({
        "api_url": api_url,
        "session_token": "fake-session-token",
        "email": "webdav-test@beebeeb.io",
        "master_key": key,
    })
    .to_string();
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), &json).unwrap();
    }
}

struct Gateway {
    child: Child,
    port: u16,
    /// The per-run credential `bb webdav` printed, if it printed one.
    secret: Option<String>,
    /// Everything the command wrote to stdout before it was ready.
    startup_output: String,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn strip_ansi(s: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(s, "")
        .into_owned()
}

fn spawn_bb(home: &Path, args: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", home.join("empty-path"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bb")
}

/// Start `bb webdav --port <p> <extra...>` and wait until it prints its "Ctrl+C" line
/// (the last startup line), collecting what it said.
fn start_gateway(name: &str, api_url: &str, extra: &[&str]) -> Gateway {
    let home = scratch(name);
    write_fake_session(&home, api_url);
    let port = free_port();
    let port_s = port.to_string();
    let mut args = vec!["webdav", "--port", port_s.as_str()];
    args.extend_from_slice(extra);
    let mut child = spawn_bb(&home, &args);

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut startup_output = String::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => {
                let line = strip_ansi(&line);
                startup_output.push_str(&line);
                startup_output.push('\n');
                if line.contains("Ctrl+C") {
                    break;
                }
            }
            Err(_) => panic!("bb webdav never became ready; output so far:\n{startup_output}"),
        }
    }
    // Keep draining so the child never blocks on a full pipe.
    std::thread::spawn(move || while rx.recv().is_ok() {});

    let secret = startup_output.lines().find_map(|l| {
        let t = l.trim();
        t.strip_prefix("password").map(|rest| rest.trim().to_string())
    });
    Gateway {
        child,
        port,
        secret,
        startup_output,
    }
}

impl Gateway {
    fn basic(&self) -> String {
        let secret = self.secret.clone().unwrap_or_else(|| "no-credential-printed".into());
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("bb:{secret}"))
        )
    }

    /// One request. `host`/`origin` override those headers; `auth` is the full
    /// `Authorization` header value.
    async fn request(
        &self,
        method: &str,
        path: &str,
        host: Option<&str>,
        origin: Option<&str>,
        auth: Option<&str>,
    ) -> u16 {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let mut req = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("http://127.0.0.1:{}{path}", self.port),
        );
        if let Some(h) = host {
            req = req.header("Host", h);
        }
        if let Some(o) = origin {
            req = req.header("Origin", o);
        }
        if let Some(a) = auth {
            req = req.header("Authorization", a);
        }
        if method == "MOVE" {
            req = req.header("Destination", "/moved.txt");
        }
        if method == "PUT" {
            req = req.body("hello");
        }
        req.send().await.expect("request to bb webdav").status().as_u16()
    }

    /// An authenticated request with a good Host and no Origin.
    async fn authed(&self, method: &str, path: &str) -> u16 {
        let host = format!("127.0.0.1:{}", self.port);
        let auth = self.basic();
        self.request(method, path, Some(&host), None, Some(&auth)).await
    }
}

#[tokio::test]
async fn webdav_rejects_foreign_host_header() {
    let (api, _) = spawn_mock_api().await;
    let gw = start_gateway("host", &api, &[]);
    let auth = gw.basic();
    // DNS rebinding: the page's own hostname resolves to 127.0.0.1.
    assert_eq!(
        gw.request("PROPFIND", "/", Some("evil.example"), None, Some(&auth))
            .await,
        403
    );
    assert_eq!(
        gw.request(
            "PROPFIND",
            "/",
            Some(&format!("evil.example:{}", gw.port)),
            None,
            Some(&auth)
        )
        .await,
        403
    );
    assert_eq!(
        gw.request(
            "PROPFIND",
            "/",
            Some(&format!("127.0.0.1.evil.example:{}", gw.port)),
            None,
            Some(&auth)
        )
        .await,
        403
    );
    // Right host, wrong port: a different local service's origin.
    assert_eq!(
        gw.request("PROPFIND", "/", Some("127.0.0.1:1"), None, Some(&auth))
            .await,
        403
    );
    // No port at all is not our authority either (we are not on :80).
    assert_eq!(
        gw.request("PROPFIND", "/", Some("localhost"), None, Some(&auth)).await,
        403
    );
}

#[tokio::test]
async fn webdav_rejects_any_origin_header() {
    let (api, _) = spawn_mock_api().await;
    let gw = start_gateway("origin", &api, &[]);
    let host = format!("127.0.0.1:{}", gw.port);
    let auth = gw.basic();
    for origin in ["http://evil.example", "null", &format!("http://127.0.0.1:{}", gw.port)] {
        assert_eq!(
            gw.request("PROPFIND", "/", Some(&host), Some(origin), Some(&auth))
                .await,
            403,
            "Origin: {origin} must be refused even with a valid credential"
        );
    }
}

#[tokio::test]
async fn webdav_requires_the_per_run_credential() {
    let (api, hits) = spawn_mock_api().await;
    let gw = start_gateway("cred", &api, &[]);
    assert!(
        gw.secret.is_some(),
        "startup must print a per-run credential; output was:\n{}",
        gw.startup_output
    );
    let host = format!("127.0.0.1:{}", gw.port);

    // No credential.
    assert_eq!(gw.request("PROPFIND", "/", Some(&host), None, None).await, 401);
    // Wrong credential.
    let wrong = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("bb:not-the-secret")
    );
    assert_eq!(gw.request("PROPFIND", "/", Some(&host), None, Some(&wrong)).await, 401);
    // Empty password, and a non-Basic scheme.
    let empty = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("bb:"));
    assert_eq!(gw.request("PROPFIND", "/", Some(&host), None, Some(&empty)).await, 401);
    assert_eq!(
        gw.request("GET", "/", Some(&host), None, Some("Bearer fake-session-token"))
            .await,
        401
    );
    // Every method is gated, not only PROPFIND.
    for m in ["OPTIONS", "GET", "HEAD", "PUT", "DELETE", "MKCOL"] {
        assert_eq!(
            gw.request(m, "/x", Some(&host), None, None).await,
            401,
            "{m} without credential"
        );
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "an unauthenticated request must not reach the vault API"
    );

    // Correct credential.
    assert_eq!(gw.authed("PROPFIND", "/").await, 207);
}

#[tokio::test]
async fn webdav_challenges_with_basic_realm() {
    let (api, _) = spawn_mock_api().await;
    let gw = start_gateway("challenge", &api, &[]);
    let resp = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(b"PROPFIND").unwrap(),
            format!("http://127.0.0.1:{}/", gw.port),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
    let challenge = resp
        .headers()
        .get("www-authenticate")
        .expect("WWW-Authenticate")
        .to_str()
        .unwrap();
    assert!(challenge.starts_with("Basic "), "got {challenge}");
}

#[tokio::test]
async fn webdav_credential_is_per_run_and_never_the_session() {
    let (api, _) = spawn_mock_api().await;
    let a = start_gateway("run-a", &api, &[]);
    let b = start_gateway("run-b", &api, &[]);
    let (sa, sb) = (a.secret.clone().expect("a secret"), b.secret.clone().expect("b secret"));
    assert_ne!(sa, sb, "two runs must not share a credential");
    // >= 128 bits of entropy: 32 random bytes base64url = 43 chars; demand at least 22.
    assert!(sa.len() >= 22, "credential too short: {}", sa.len());
    for out in [&a.startup_output, &b.startup_output] {
        assert!(!out.contains("fake-session-token"), "session token leaked to stdout");
    }
    // A's credential does not open B.
    let host = format!("127.0.0.1:{}", b.port);
    let a_cred = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("bb:{sa}"))
    );
    assert_eq!(b.request("PROPFIND", "/", Some(&host), None, Some(&a_cred)).await, 401);
}

#[tokio::test]
async fn webdav_default_is_read_only() {
    let (api, hits) = spawn_mock_api().await;
    let gw = start_gateway("readonly", &api, &[]);
    for m in ["PUT", "DELETE", "MKCOL", "MOVE", "COPY", "PROPPATCH"] {
        assert_eq!(gw.authed(m, "/a.txt").await, 403, "{m} must be refused by default");
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "a refused write must not reach the vault API"
    );
    // Reads still work.
    assert_eq!(gw.authed("PROPFIND", "/").await, 207);
    assert_eq!(gw.authed("OPTIONS", "/").await, 200);
}

#[tokio::test]
async fn webdav_read_only_flag_is_still_accepted() {
    let (api, hits) = spawn_mock_api().await;
    let gw = start_gateway("readonly-alias", &api, &["--read-only"]);
    assert_eq!(gw.authed("MKCOL", "/Newdir").await, 403);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn webdav_read_write_flag_enables_writes() {
    let (api, hits) = spawn_mock_api().await;
    let gw = start_gateway("readwrite", &api, &["--read-write"]);
    assert!(
        gw.startup_output.contains("read-write"),
        "banner must say read-write:\n{}",
        gw.startup_output
    );
    // MKCOL reaches the handler, lists the vault, creates the folder on the mock API.
    assert_eq!(gw.authed("MKCOL", "/Newdir").await, 201);
    assert!(
        hits.load(Ordering::SeqCst) >= 2,
        "MKCOL must have reached the vault API"
    );
    // PUT is not refused by the gateway: it gets as far as the (mock) upload API.
    let before = hits.load(Ordering::SeqCst);
    let status = gw.authed("PUT", "/a.txt").await;
    assert!(![401, 403].contains(&status), "PUT refused by the gateway: {status}");
    assert!(
        hits.load(Ordering::SeqCst) > before,
        "PUT must have reached the vault API"
    );
    // Even read-write does not lift the Host / Origin / credential checks.
    let auth = gw.basic();
    assert_eq!(
        gw.request("MKCOL", "/Evil", Some("evil.example"), None, Some(&auth))
            .await,
        403
    );
    assert_eq!(
        gw.request("MKCOL", "/Evil", Some(&format!("127.0.0.1:{}", gw.port)), None, None)
            .await,
        401
    );
}

#[tokio::test]
async fn webdav_accepts_loopback_host_spellings() {
    let (api, _) = spawn_mock_api().await;
    let gw = start_gateway("hosts", &api, &[]);
    let auth = gw.basic();
    for host in [
        format!("127.0.0.1:{}", gw.port),
        format!("localhost:{}", gw.port),
        format!("LOCALHOST:{}", gw.port),
    ] {
        assert_eq!(
            gw.request("PROPFIND", "/", Some(&host), None, Some(&auth)).await,
            207,
            "Host: {host}"
        );
    }
}

#[tokio::test]
async fn webdav_prints_a_ready_to_use_mount_url_once() {
    let (api, _) = spawn_mock_api().await;
    let gw = start_gateway("banner", &api, &[]);
    let secret = gw.secret.clone().expect("password line");
    let url_line = gw
        .startup_output
        .lines()
        .find(|l| l.contains(&format!("@localhost:{}", gw.port)))
        .unwrap_or_else(|| panic!("no mount URL with credential in:\n{}", gw.startup_output));
    assert!(url_line.contains(&secret), "mount URL must embed the credential");
    assert_eq!(
        gw.startup_output.matches(&secret).count(),
        2,
        "the secret appears exactly in the password line and the mount URL:\n{}",
        gw.startup_output
    );
    // The old hard-coded example port (7878) must not be advertised for another port.
    if gw.port != 7878 {
        assert!(
            !gw.startup_output.contains("7878"),
            "stale example port in:\n{}",
            gw.startup_output
        );
    }
}

#[tokio::test]
async fn webdav_fails_clearly_when_the_port_is_taken() {
    let (api, _) = spawn_mock_api().await;
    let first = start_gateway("taken-1", &api, &[]);
    let home = scratch("taken-2");
    write_fake_session(&home, &api);
    let port_s = first.port.to_string();
    let child = spawn_bb(&home, &["webdav", "--port", &port_s]);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "second bind must fail");
    let text = strip_ansi(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ));
    assert!(text.contains("failed to bind"), "unclear error: {text}");
}

#[tokio::test]
async fn webdav_read_only_and_read_write_conflict() {
    let (api, _) = spawn_mock_api().await;
    let home = scratch("conflict");
    write_fake_session(&home, &api);
    let child = spawn_bb(&home, &["webdav", "--read-only", "--read-write"]);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    let text = strip_ansi(&String::from_utf8_lossy(&out.stderr));
    assert!(
        text.contains("cannot be used with"),
        "expected a clap conflict error, got: {text}"
    );
}

#[test]
fn webdav_help_says_read_only_is_the_default() {
    let home = scratch("help");
    let out = spawn_bb(&home, &["webdav", "--help"]).wait_with_output().unwrap();
    let text = strip_ansi(&String::from_utf8_lossy(&out.stdout));
    assert!(
        text.contains("--read-write"),
        "help must document --read-write:\n{text}"
    );
    assert!(
        text.to_lowercase().contains("read-only by default"),
        "help must say the default:\n{text}"
    );
}
