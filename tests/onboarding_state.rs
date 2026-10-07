//! Task 1750 (epic 1725, spec T13): `bb` reads `signup.mode` and
//! `account.state` off `GET /api/v1/onboarding`.
//!
//! Drives the real `bb` binary against an in-process mock API that serves the
//! VENDORED golden fixtures (`contracts/onboarding/fixtures`, byte-identical to
//! the server's, guarded by `scripts/check-onboarding-contract.sh`). Every run
//! has its own scratch `HOME` (the developer's real config holds a live
//! session) and `BB_NO_UPDATE=1`.
//!
//! What this pins:
//! - `bb signup` asks anonymously (no `Authorization`), declares itself `cli`,
//!   schema 1, `direct`, and ALWAYS sends the user to the web signup, even when
//!   the document says `signup.mode = native` (the CLI is never lifted);
//! - `--json` carries the server's `signup` block verbatim, `null` on a server
//!   without the endpoint, and the pre-1750 keys are unchanged;
//! - a 404 / 500 / garbage / schema-2 answer leaves `bb signup` and
//!   `bb whoami` exactly as they were (legacy fallback);
//! - `bb whoami` shows the document's account state and denied capabilities.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use serde_json::{Value, json};

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("contracts/onboarding/fixtures")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn scratch_home() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("onboarding-state-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_config(home: &Path, api_url: &str, token: Option<&str>) {
    let mut config = json!({ "api_url": api_url });
    if let Some(t) = token {
        config["session_token"] = json!(t);
        config["email"] = json!("onboarding@beebeeb.io");
        config["master_key"] = json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    }
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }
}

fn bb(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        // A desktop session must not make `bb signup` open a real browser.
        .env("BB_LOGIN_HEADLESS", "1")
        .env_remove("APP_URL")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run bb")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[derive(Clone)]
enum Onboarding {
    Doc(Value),
    Status(u16, &'static str),
}

#[derive(Clone)]
struct Mock {
    onboarding: Onboarding,
    subscription: Value,
    seen: Arc<Mutex<Vec<HeaderMap>>>,
}

/// Mock API: the onboarding route plus the routes `bb whoami` needs.
fn spawn_mock(onboarding: Onboarding) -> (String, Arc<Mutex<Vec<HeaderMap>>>) {
    spawn_mock_with_subscription(
        onboarding,
        json!({ "plan": "pro", "quota_bytes": 1_000_000_000_000i64, "account_state": "ok" }),
    )
}

/// As [`spawn_mock`], with the legacy `GET /billing/subscription` body chosen
/// by the test (the legacy and document views of an account can disagree).
fn spawn_mock_with_subscription(onboarding: Onboarding, subscription: Value) -> (String, Arc<Mutex<Vec<HeaderMap>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = Mock {
        onboarding,
        subscription,
        seen: seen.clone(),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new()
                .route(
                    "/api/v1/onboarding",
                    get(|State(m): State<Mock>, headers: HeaderMap| async move {
                        m.seen.lock().unwrap().push(headers);
                        match m.onboarding {
                            Onboarding::Doc(v) => (StatusCode::OK, Json(v)).into_response_compat(),
                            Onboarding::Status(code, body) => {
                                (StatusCode::from_u16(code).unwrap(), body.to_string()).into_response_compat()
                            }
                        }
                    }),
                )
                .route(
                    "/api/v1/auth/me",
                    get(|| async { Json(json!({ "email": "onboarding@beebeeb.io" })) }),
                )
                .route(
                    "/api/v1/billing/subscription",
                    get(|State(m): State<Mock>| async move { Json(m.subscription) }),
                )
                .route(
                    "/api/v1/me/region",
                    get(|| async { Json(json!({ "preferred_region": "europe" })) }),
                )
                .route(
                    "/api/v1/auth/sessions",
                    get(|| async { Json(json!({ "sessions": [] })) }),
                )
                .route(
                    "/api/v1/files/usage",
                    get(|| async { Json(json!({ "used_bytes": 1000 })) }),
                )
                .route(
                    "/api/v1/files/count",
                    get(|| async { Json(json!({ "total_files": 3 })) }),
                )
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    (format!("http://{}", rx.recv().unwrap()), seen)
}

trait IntoResponseCompat {
    fn into_response_compat(self) -> axum::response::Response;
}
impl<T: axum::response::IntoResponse> IntoResponseCompat for T {
    fn into_response_compat(self) -> axum::response::Response {
        self.into_response()
    }
}

fn json_of(out: &Output) -> Value {
    serde_json::from_str(&text(&out.stdout)).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(&out.stdout)))
}

// ── bb signup ───────────────────────────────────────────────────────────────

#[test]
fn signup_json_carries_the_server_signup_block_and_the_old_keys() {
    // pre_account.desktop says web_only / signup_web_only (task 1836); the block is
    // echoed verbatim whatever it holds.
    let doc = fixture("pre_account.desktop");
    let (api, seen) = spawn_mock(Onboarding::Doc(doc.clone()));
    let home = scratch_home();
    write_config(&home, &api, None);

    let out = bb(&home, &["signup", "--json"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    let v = json_of(&out);
    assert_eq!(v["signup"], doc["signup"], "the server's block, verbatim");
    // Pre-1750 keys, unchanged. The mock API is localhost, so the web app is
    // the local dev web app, never production.
    assert_eq!(v["url"], "http://localhost:5173/signup");
    assert_eq!(v["next"], "bb login");
    assert!(v["note"].as_str().unwrap().contains("CLI only signs in"));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one onboarding request");
    let h = &seen[0];
    assert!(h.get("authorization").is_none(), "signup is anonymous: {h:?}");
    assert_eq!(h["x-beebeeb-client"], "cli");
    assert_eq!(h["x-beebeeb-onboarding-schema"], "1");
    assert_eq!(h["x-beebeeb-store-channel"], "direct");
    assert!(["macos", "linux", "windows", "unknown"].contains(&h["x-beebeeb-client-os"].to_str().unwrap()));
}

#[test]
fn signup_is_never_lifted_even_when_the_document_says_native() {
    // The fixture says web_only since task 1836; the strongest case for "the CLI is
    // never lifted" is a document that offers a native signup, so build one.
    let mut doc = fixture("pre_account.desktop");
    doc["signup"] =
        json!({"allowed": true, "mode": "native", "reason": null, "web_url": doc["signup"]["web_url"].clone()});
    let (api, _) = spawn_mock(Onboarding::Doc(doc));
    let home = scratch_home();
    write_config(&home, &api, None);

    let out = bb(&home, &["signup"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert!(
        stdout.contains("Create your account at http://localhost:5173/signup"),
        "{stdout}"
    );
    assert!(
        stdout.contains("the terminal does not"),
        "must explain, not obey: {stdout}"
    );
    assert!(!stdout.to_lowercase().contains("password"), "no native flow: {stdout}");
}

#[test]
fn signup_explains_web_only_and_a_closed_door() {
    let mut doc = fixture("pre_account.web");
    doc["signup"] =
        json!({"allowed": true, "mode": "web_only", "reason": null, "web_url": "https://app.beebeeb.io/signup"});
    let (api, _) = spawn_mock(Onboarding::Doc(doc));
    let home = scratch_home();
    write_config(&home, &api, None);
    let stdout = text(&bb(&home, &["signup"]).stdout);
    assert!(stdout.contains("the terminal only signs in"), "{stdout}");

    let mut doc = fixture("pre_account.web");
    doc["signup"] = json!({"allowed": false, "mode": "web_only", "reason": "signups_paused"});
    let (api, _) = spawn_mock(Onboarding::Doc(doc));
    let home = scratch_home();
    write_config(&home, &api, None);
    let stdout = text(&bb(&home, &["signup"]).stdout);
    assert!(
        stdout.contains("not accepting new accounts from this client"),
        "{stdout}"
    );
    assert!(stdout.contains("(signups_paused)"), "{stdout}");
    assert!(
        stdout.contains("Create your account at"),
        "still points at the web: {stdout}"
    );
}

#[test]
fn signup_falls_back_to_the_old_behaviour_when_the_document_is_unusable() {
    let cases: Vec<(&str, Onboarding)> = vec![
        ("404 (server predates the endpoint)", Onboarding::Status(404, "")),
        ("500", Onboarding::Status(500, "boom")),
        ("200 garbage", Onboarding::Status(200, "<html>not json</html>")),
        (
            "schema 2",
            Onboarding::Doc(
                json!({"schema": 2, "stage": "pre_account", "signup": {"mode": "native", "allowed": true}}),
            ),
        ),
    ];
    for (label, mock) in cases {
        let (api, _) = spawn_mock(mock);
        let home = scratch_home();
        write_config(&home, &api, None);

        let out = bb(&home, &["signup", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{label}: stderr: {}", text(&out.stderr));
        let v = json_of(&out);
        assert_eq!(v["signup"], Value::Null, "{label}");
        assert_eq!(v["url"], "http://localhost:5173/signup", "{label}");

        let out = bb(&home, &["signup"]);
        let stdout = text(&out.stdout);
        assert_eq!(out.status.code(), Some(0), "{label}");
        assert_eq!(
            stdout.trim(),
            "Create your account at http://localhost:5173/signup \u{2014} then run `bb login`.",
            "{label}: exactly the pre-1750 output"
        );
    }
}

#[test]
fn signup_says_so_when_the_cli_is_too_old() {
    let mut doc = fixture("pre_account.web");
    doc["client"] = json!({"status": "update_required", "min_version": "9.0.0"});
    let (api, _) = spawn_mock(Onboarding::Doc(doc));
    let home = scratch_home();
    write_config(&home, &api, None);
    let stdout = text(&bb(&home, &["signup"]).stdout);
    assert!(stdout.contains("too old for the server"), "{stdout}");
}

// ── bb whoami ───────────────────────────────────────────────────────────────

#[test]
fn whoami_shows_the_document_state_and_what_it_denies() {
    let (api, seen) = spawn_mock(Onboarding::Doc(fixture("account.trial_ended.ios")));
    let home = scratch_home();
    write_config(&home, &api, Some("live-ish-token"));

    let out = bb(&home, &["whoami"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert!(stdout.contains("read-only (trial ended)"), "{stdout}");
    assert!(stdout.contains("will be deleted on November 1, 2026"), "{stdout}");
    assert!(stdout.contains("blocked"), "upload row: {stdout}");

    // (Scoped: holding the lock would block the mock on the next run.)
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["authorization"], "Bearer live-ish-token");
    }

    let out = bb(&home, &["whoami", "--json"]);
    let v = json_of(&out);
    assert_eq!(v["account"]["state"], "trial_ended");
    assert_eq!(
        v["account"]["denied"],
        json!([
            {"capability": "share", "reason": "trial_ended"},
            {"capability": "upload", "reason": "trial_ended"},
        ])
    );
    // Legacy keys untouched.
    assert_eq!(v["account_state"], "ok");
    assert_eq!(v["plan"], "pro");
}

#[test]
fn whoami_is_unchanged_on_a_server_without_the_endpoint() {
    let (api, _) = spawn_mock(Onboarding::Status(404, ""));
    let home = scratch_home();
    write_config(&home, &api, Some("tok"));

    let out = bb(&home, &["whoami", "--json"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    let v = json_of(&out);
    assert_eq!(v["account"], Value::Null);
    assert_eq!(v["account_state"], "ok");

    let stdout = text(&bb(&home, &["whoami"]).stdout);
    assert!(stdout.contains("state"), "{stdout}");
    assert!(!stdout.contains("blocked"), "{stdout}");
}

#[test]
fn whoami_survives_a_broken_onboarding_answer() {
    let (api, _) = spawn_mock(Onboarding::Status(500, "boom"));
    let home = scratch_home();
    write_config(&home, &api, Some("tok"));
    let out = bb(&home, &["whoami", "--json"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(json_of(&out)["account"], Value::Null);
}

/// Codex P2 on PR #61: for an `allowance` account the legacy subscription says
/// `needs_plan` while the document says `upload.allowed = true`. The document
/// is authoritative whenever it is available, so `whoami` must not show the
/// upload row as blocked, nor the state in the error colour.
#[test]
fn whoami_document_allowing_upload_overrides_the_legacy_needs_plan_notice() {
    let legacy = json!({
        "plan": "free", "effective_plan": "none", "quota_bytes": 0,
        "account_state": "needs_plan", "data_deletion_at": null,
    });
    let (api, _) = spawn_mock_with_subscription(Onboarding::Doc(fixture("account.allowance.web")), legacy.clone());
    let home = scratch_home();
    write_config(&home, &api, Some("tok"));
    let out = bb(&home, &["whoami"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert!(stdout.contains("free allowance"), "{stdout}");
    assert!(!stdout.contains("blocked"), "upload must not be blocked: {stdout}");
    assert!(stdout.contains("up to"), "normal upload row expected: {stdout}");

    // With the document unavailable the legacy notice is all there is, and it
    // still blocks (the fallback is unchanged).
    let (api, _) = spawn_mock_with_subscription(Onboarding::Status(404, ""), legacy);
    let home = scratch_home();
    write_config(&home, &api, Some("tok"));
    let stdout = text(&bb(&home, &["whoami"]).stdout);
    assert!(stdout.contains("blocked"), "legacy fallback still blocks: {stdout}");
}
