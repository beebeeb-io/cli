//! Task 1922, rollout step 3 (cli): the CLI sends and reads sync-client
//! metadata only as `bbcm1.` ciphertext.
//!
//! Black box: the real `bb` binary against an in-process mock of
//! `/api/v1/clients/*` (plus the little of the file API a pass needs). The mock
//! captures every request body, so a test can grep them for a sentinel instead
//! of trusting a DTO. `HOME` and `XDG_CONFIG_HOME` are scratch directories: the
//! real config holds a live prod session and is never read.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use beebeeb_core::client_meta::{ClientMetaField, seal_client_meta};
use beebeeb_core::kdf::MasterKey;
use serde_json::{Value, json};

const SENTINEL: &str = "SENT1922";
const USER_ID: &str = "11111111-1111-4111-8111-111111111111";
const DEVICE_ID: &str = "22222222-2222-4222-8222-222222222222";
const NEW_SESSION_ID: &str = "33333333-3333-4333-8333-333333333333";

/// The test master key: 32 zero bytes, standard-base64 as the config stores it.
fn test_master_key_b64() -> String {
    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string()
}

fn test_master_key() -> MasterKey {
    MasterKey::from_bytes([0u8; 32])
}

#[derive(Clone, Debug)]
struct Captured {
    method: String,
    path: String,
    body: String,
}

#[derive(Default)]
struct World {
    captured: Vec<Captured>,
    /// What `GET /api/v1/clients/sessions` serves.
    sessions: Vec<Value>,
    /// Every body posted to `POST /api/v1/clients/sessions`.
    created_sessions: Vec<Value>,
}

type Shared = Arc<Mutex<World>>;

async fn capture(State(w): State<Shared>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap_or_default();
    w.lock().unwrap().captured.push(Captured {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        body: String::from_utf8_lossy(&bytes).to_string(),
    });
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

async fn start_mock(world: Shared) -> String {
    let w = world.clone();
    let app = Router::new()
        .route("/api/v1/auth/me", get(|| async { Json(json!({ "id": USER_ID })) }))
        .route(
            "/api/v1/files",
            get(|| async { Json(json!({ "files": [], "truncated": false })) }),
        )
        .route(
            "/api/v1/files/folder",
            post(|body: Bytes| async move {
                let v: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
                let id = v
                    .get("folder_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("44444444-4444-4444-8444-444444444444");
                Json(json!({ "id": id, "is_folder": true }))
            }),
        )
        .route(
            "/api/v1/clients/devices",
            post(|body: Bytes| async move {
                let v: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
                // Echo the record key back the way the server does, so a test
                // can prove the client sent and persisted its own key.
                Json(json!({
                    "id": DEVICE_ID,
                    "client_key": v.get("client_key").cloned().unwrap_or(Value::Null),
                    "hostname_encrypted": v.get("hostname_encrypted").cloned().unwrap_or(Value::Null),
                    "hostname": "",
                    "platform": v.get("platform").cloned().unwrap_or(Value::Null),
                    "bb_version": v.get("bb_version").cloned().unwrap_or(Value::Null),
                    "last_seen": "2026-10-30T00:00:00Z",
                    "created_at": "2026-10-30T00:00:00Z",
                }))
            }),
        )
        .route(
            "/api/v1/clients/sessions",
            get(|State(w): State<Shared>| async move {
                let w = w.lock().unwrap();
                Json(json!({ "sessions": w.sessions }))
            })
            .post(|State(w): State<Shared>, body: Bytes| async move {
                let v: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
                w.lock().unwrap().created_sessions.push(v);
                (
                    StatusCode::CREATED,
                    Json(json!({
                        "id": NEW_SESSION_ID,
                        "client_key": null,
                        "name_encrypted": null,
                        "local_path_encrypted": null,
                        "remote_path_encrypted": null,
                        "name": "",
                        "remote_path": "",
                        "session_type": "sync",
                        "status": "active",
                    })),
                )
            }),
        )
        .route(
            "/api/v1/clients/sessions/:id",
            patch(|UrlPath(id): UrlPath<String>| async move {
                Json(json!({ "id": id, "status": "stopped", "name": "", "remote_path": "" }))
            }),
        )
        .route(
            "/api/v1/clients/sessions/:id/heartbeat",
            post(|| async { Json(json!({ "ok": true })) }),
        )
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({ "error": "not_found" }))) })
        .layer(middleware::from_fn_with_state(w, capture))
        .with_state(world);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A scratch HOME with the config the binary needs, and a device identity whose
/// hostname carries the sentinel.
fn scratch_home(api: &str, hostname: &str, device_id: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("bb-clientmeta-{}", uuid::Uuid::new_v4()));
    let config = json!({
        "api_url": api,
        "session_token": "test-session-token",
        "email": "client-meta@beebeeb.io",
        "master_key": test_master_key_b64(),
    });
    let device = json!({
        "device_id": device_id,
        "hostname": hostname,
        "platform": "linux",
        "registered_at": "2026-10-30T00:00:00Z",
    });
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
        std::fs::write(dir.join("device.json"), device.to_string()).unwrap();
    }
    home
}

async fn bb(home: &Path, args: &[&str]) -> Output {
    let home = home.to_path_buf();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let shown = args.join(" ");
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
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
        "[bb {} -> {:?}]\n--- stdout\n{}--- stderr\n{}",
        shown,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn captured(world: &Shared) -> Vec<Captured> {
    world.lock().unwrap().captured.clone()
}

fn bodies(world: &Shared) -> Vec<String> {
    captured(world).into_iter().map(|c| c.body).collect()
}

fn request_body(world: &Shared, method: &str, path: &str) -> Value {
    let all = captured(world);
    let found = all
        .iter()
        .find(|c| c.method == method && c.path == path)
        .unwrap_or_else(|| panic!("no {method} {path}; captured: {all:?}"));
    serde_json::from_str(&found.body).unwrap_or(Value::Null)
}

fn has_request(world: &Shared, method: &str, path: &str) -> bool {
    captured(world).iter().any(|c| c.method == method && c.path == path)
}

/// The whole point of the task: a `bb sync` run whose local path, device
/// hostname and remote path all carry the sentinel sends it nowhere, and every
/// client-chosen string arrives as a `bbcm1.` envelope.
#[tokio::test(flavor = "multi_thread")]
async fn sync_bodies_carry_no_plaintext_and_only_envelopes() {
    let world: Shared = Arc::default();
    let api = start_mock(world.clone()).await;
    let home = scratch_home(&api, &format!("{SENTINEL}-host"), DEVICE_ID);
    let local = home.join(format!("{SENTINEL}-local"));
    std::fs::create_dir_all(&local).unwrap();

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "sync",
            local.to_str().unwrap(),
            &format!("{SENTINEL}-remote"),
            "--once",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "sync failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let all_bodies = bodies(&world);
    for body in &all_bodies {
        assert!(
            !body.contains(SENTINEL),
            "a request body carried the plaintext sentinel: {body}"
        );
    }

    // The device registration carries its record key and the field-1 envelope,
    // and no plaintext `hostname` at all.
    let device = request_body(&world, "POST", "/api/v1/clients/devices");
    assert_eq!(device["client_key"], json!(DEVICE_ID), "device body: {device}");
    let hostname_env = device["hostname_encrypted"]
        .as_str()
        .unwrap_or_else(|| panic!("no hostname_encrypted in {device}"));
    assert!(
        hostname_env.starts_with("bbcm1."),
        "hostname envelope is not shaped: {hostname_env}"
    );
    assert!(device.get("hostname").is_none(), "plaintext hostname sent: {device}");

    // The session registration carries one record key and three envelopes.
    let session = request_body(&world, "POST", "/api/v1/clients/sessions");
    assert!(
        session["client_key"]
            .as_str()
            .is_some_and(|k| uuid::Uuid::parse_str(k).is_ok()),
        "session body has no record key: {session}"
    );
    for key in ["name_encrypted", "local_path_encrypted", "remote_path_encrypted"] {
        let env = session[key].as_str().unwrap_or_else(|| panic!("no {key} in {session}"));
        assert!(env.starts_with("bbcm1."), "{key} is not shaped: {env}");
    }
    for key in ["name", "local_path", "remote_path"] {
        assert!(session.get(key).is_none(), "plaintext {key} sent: {session}");
    }

    // Heartbeats carry no free text either.
    let heartbeats: Vec<&String> = all_bodies
        .iter()
        .filter(|b| b.contains("\"status\"") && b.contains("files_synced"))
        .collect();
    for body in heartbeats {
        assert!(!body.contains("current_file"), "heartbeat carried current_file: {body}");
        assert!(!body.contains("\"detail\""), "heartbeat carried detail: {body}");
    }
}

/// Reuse matches on the client-generated record key, never on the plaintext
/// remote path (which the server no longer returns at all).
#[tokio::test(flavor = "multi_thread")]
async fn session_reuse_matches_on_client_key() {
    let session_key = "55555555-5555-4555-8555-555555555555";
    let world: Shared = Arc::new(Mutex::new(World {
        sessions: vec![json!({
            "id": "66666666-6666-4666-8666-666666666666",
            "client_key": session_key,
            "device_id": DEVICE_ID,
            "device_client_key": DEVICE_ID,
            "name_encrypted": null,
            "remote_path_encrypted": null,
            "local_path_encrypted": null,
            "device_hostname_encrypted": null,
            "name": "",
            "remote_path": "",
            "session_type": "sync",
            "status": "active",
            "last_heartbeat": null,
        })],
        ..Default::default()
    }));
    let api = start_mock(world.clone()).await;
    let home = scratch_home(&api, "host", DEVICE_ID);
    let local = home.join("local");
    std::fs::create_dir_all(&local).unwrap();

    // A manifest from a previous run: the record key is persisted next to
    // session_id, and the server row's plaintext remote_path is "" (as every
    // 1922 server returns it).
    let state = json!({
        "remote_path": format!("{SENTINEL}-remote"),
        "session_client_key": session_key,
    });
    std::fs::write(local.join(".bb-sync.json"), state.to_string()).unwrap();

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "sync",
            local.to_str().unwrap(),
            &format!("{SENTINEL}-remote"),
            "--once",
        ],
    )
    .await;
    assert!(
        out.status.success(),
        "sync failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !has_request(&world, "POST", "/api/v1/clients/sessions"),
        "reuse missed the existing row and registered a new session: {:?}",
        captured(&world)
    );
    assert!(
        has_request(
            &world,
            "PATCH",
            "/api/v1/clients/sessions/66666666-6666-4666-8666-666666666666"
        ),
        "the reused session was never stopped: {:?}",
        captured(&world)
    );
}

/// `--status` decrypts with the master key. Rows that cannot be opened show
/// "(name hidden)" and never fail the command.
#[tokio::test(flavor = "multi_thread")]
async fn status_decrypts_names_paths_and_device_hostname() {
    let master = test_master_key();
    let session_key = uuid::Uuid::parse_str("77777777-7777-4777-8777-777777777777").unwrap();
    let device_key = uuid::Uuid::parse_str(DEVICE_ID).unwrap();
    let name = format!("{SENTINEL}-host/{SENTINEL}-remote");
    let local = format!("/home/{SENTINEL}-local");
    let remote = format!("/{SENTINEL}-remote");
    let seal = |record: &uuid::Uuid, device: Option<&uuid::Uuid>, field: ClientMetaField, text: &str| {
        seal_client_meta(&master, record, device, field, text).unwrap()
    };

    let world: Shared = Arc::new(Mutex::new(World {
        sessions: vec![
            json!({
                "id": "88888888-8888-4888-8888-888888888888",
                "client_key": session_key.to_string(),
                "device_client_key": device_key.to_string(),
                "device_id": DEVICE_ID,
                "name_encrypted": seal(&session_key, Some(&device_key), ClientMetaField::SessionName, &name),
                "local_path_encrypted": seal(&session_key, Some(&device_key), ClientMetaField::SessionLocalPath, &local),
                "remote_path_encrypted": seal(&session_key, Some(&device_key), ClientMetaField::SessionRemotePath, &remote),
                "device_hostname_encrypted": seal(&device_key, None, ClientMetaField::DeviceHostname, &format!("{SENTINEL}-host")),
                "name": "",
                "remote_path": "",
                "session_type": "sync",
                "status": "watching",
                "last_heartbeat": null,
            }),
            // A legacy row the server nulled: nothing opens, nothing errors.
            json!({
                "id": "99999999-9999-4999-8999-999999999999",
                "client_key": null,
                "device_client_key": null,
                "device_id": null,
                "name_encrypted": null,
                "local_path_encrypted": null,
                "remote_path_encrypted": null,
                "device_hostname_encrypted": null,
                "name": "",
                "remote_path": "",
                "session_type": "sync",
                "status": "watching",
                "last_heartbeat": null,
            }),
        ],
        ..Default::default()
    }));
    let api = start_mock(world.clone()).await;
    let home = scratch_home(&api, "host", DEVICE_ID);

    let out = bb(&home, &["--api", &api, "sync", "--status"]).await;
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "status failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(&name),
        "decrypted name missing from status output:\n{stdout}"
    );
    assert!(stdout.contains(&remote), "decrypted remote path missing:\n{stdout}");
    assert!(stdout.contains(&local), "decrypted local path missing:\n{stdout}");
    assert!(
        stdout.contains(&format!("{SENTINEL}-host")),
        "decrypted device hostname missing:\n{stdout}"
    );
    assert!(
        stdout.contains("(name hidden)"),
        "nulled row is not marked hidden:\n{stdout}"
    );
    assert!(!stdout.contains("bbcm1."), "status printed an envelope:\n{stdout}");
}
