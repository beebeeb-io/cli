#![allow(dead_code)]
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use beebeeb_core::{
    kdf::{FileKey, MasterKey},
    owned_keys,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Output,
    sync::{Arc, Mutex},
};
pub const ROOT: &str = "22222222-2222-4222-8222-222222222222";
pub const SUB: &str = "33333333-3333-4333-8333-333333333333";
pub const FILE: &str = "11111111-1111-4111-8111-111111111111";
pub const BAD: &str = "44444444-4444-4444-8444-444444444444";
pub const MASTER: [u8; 32] = [7; 32];
pub fn master() -> MasterKey {
    MasterKey::from_bytes(MASTER)
}
pub fn folder_key() -> FileKey {
    FileKey::from_bytes([128; 32])
}
pub fn item_key() -> FileKey {
    FileKey::from_bytes([160; 32])
}
pub fn name(key: &FileKey, text: &str) -> String {
    serde_json::to_string(&beebeeb_core::encrypt::encrypt_metadata(key, text).unwrap()).unwrap()
}
pub fn row(id: &str, parent: Option<&str>, text: &str, folder: bool, foreign: bool) -> Value {
    let key = if foreign {
        item_key()
    } else {
        beebeeb_core::kdf::derive_file_key(&master(), id.as_bytes())
    };
    json!({"id":id,"parent_id":parent,"name_encrypted":name(&key,text),"is_folder":folder,"size_bytes":12,"chunk_count":1,"version_number":1,"created_at":"2026-10-28T10:00:00Z","acted_via_share_id":if foreign {Some(BAD)} else {None},"uploaded_by":null,"owner_wrapped_file_key":null})
}
pub fn wrap(id: &str, folder: bool) -> String {
    B64.encode(owned_keys::wrap_child_key(&folder_key(), ROOT, id, if folder { 2 } else { 1 }, &item_key()).unwrap())
}
#[derive(Default)]
pub struct Fixture {
    pub rows: Vec<Value>,
    pub canonical: HashMap<String, Value>,
    pub inventory: Vec<Value>,
    pub requests: Vec<(String, String, Vec<u8>)>,
    pub cycle: bool,
    pub requests_doc: Value,
    pub legacy_invite: bool,
}
impl Fixture {
    pub fn add_request(&mut self) {
        let private = [31; 32];
        let public = beebeeb_core::opaque::derive_x25519_public(&private);
        let (wrapped, nonce) = beebeeb_core::file_request::wrap_request_private(&master(), b"", &private).unwrap();
        self.requests_doc = json!({"file_requests":[{"id":SUB,"wrapped_private_key":B64.encode(wrapped),"wrap_nonce":B64.encode(nonce)}]});
        let seal = beebeeb_core::file_request::seal_to_request(&public, b"", item_key().as_bytes()).unwrap();
        let mut r = row(SUB, None, "request.txt", false, true);
        r["acted_via_share_id"] = Value::Null;
        r["file_request_id"] = json!(SUB);
        r["sender_ephemeral_pubkey"] = json!(B64.encode(seal.e_pub));
        r["wrapped_content_key"] = json!(B64.encode(seal.wrapped_key));
        self.rows.push(r);
    }
    pub fn basic() -> Self {
        Self {
            rows: vec![
                row(ROOT, None, "Shared", true, false),
                row(FILE, Some(ROOT), "guest.txt", false, true),
            ],
            inventory: vec![json!({"file_id":FILE,"encrypted_file_key":wrap(FILE,false)})],
            ..Self::default()
        }
    }
}
pub struct Harness {
    pub state: Arc<Mutex<Fixture>>,
    pub api: String,
    pub home: PathBuf,
    server: Option<tokio::task::JoinHandle<()>>,
}
impl Harness {
    pub async fn new(fixture: Fixture) -> Self {
        let state = Arc::new(Mutex::new(fixture));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let home = std::env::temp_dir().join(format!("bb-2237d-{}", uuid::Uuid::new_v4()));
        let config=json!({"api_url":api,"session_token":"fixture-only","email":"fixture@localhost","master_key":B64.encode(MASTER)}).to_string();
        for dir in [
            home.join(".config/beebeeb"),
            home.join("Library/Application Support/beebeeb"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.json"), &config).unwrap();
        }
        Self {
            state,
            api,
            home,
            server: Some(server),
        }
    }
    pub async fn run(&self, args: &[&str]) -> Output {
        let home = self.home.clone();
        let api = self.api.clone();
        let args: Vec<_> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_bb"))
                .args(args)
                .args(["--api", &api, "--json"])
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("BB_REFUSE_NON_LOOPBACK_API", "1")
                .env("BB_NO_UPDATE", "1")
                .env("BB_TRANSPORT", "api")
                .env_remove("APP_URL")
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    }
    pub async fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args).await;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }
    pub fn count(&self, method: &str, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, p, _)| m == method && p.starts_with(path))
            .count()
    }
    pub async fn close(mut self) {
        if let Some(s) = self.server.take() {
            s.abort();
            let _ = s.await;
        }
        std::fs::remove_dir_all(&self.home).unwrap();
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(s) = self.server.take() {
            s.abort();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}
async fn handle(State(state): State<Arc<Mutex<Fixture>>>, req: Request) -> Response {
    let method = req.method().to_string();
    let uri = req.uri().clone();
    let path = uri.path();
    let query: HashMap<String, String> = reqwest::Url::parse(&format!("http://localhost{uri}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    let mut f = state.lock().unwrap();
    f.requests.push((method.clone(), uri.to_string(), body));
    let value = if method == "GET" && path == "/api/v1/file-requests" {
        f.requests_doc.clone()
    } else if method == "GET" && path == "/api/v1/shares/invites/sent" {
        json!({"invites":if f.legacy_invite {vec![json!({"id":BAD,"file_id":ROOT,"is_folder_share":true,"status":"approved","encrypted_owner_folder_key":B64.encode(owned_keys::wrap_key_to_owner(&master(),ROOT,&folder_key()).unwrap())})]} else {vec![]}})
    } else if method == "GET" && path == format!("/api/v1/shares/invites/{BAD}/folder-keys") {
        json!({"keys":f.inventory})
    } else if f.legacy_invite && path == format!("/api/v1/shared-folders/{ROOT}") {
        return StatusCode::NOT_FOUND.into_response();
    } else if method == "GET" && path == "/api/v1/files" {
        json!({"files":f.rows.iter().filter(|r|r["parent_id"].as_str()==query.get("parent_id").map(String::as_str)).cloned().collect::<Vec<_>>()})
    } else if method == "GET" && path == "/api/v1/files/index" {
        json!({"files":f.rows})
    } else if method == "GET" && path == format!("/api/v1/shared-folders/{ROOT}") {
        json!({"folder_id":ROOT,"role":"owner","access_level":"edit","key_version":1,"owner_wrapped_folder_key":B64.encode(owned_keys::wrap_folder_custody(&master(),ROOT,&folder_key()).unwrap())})
    } else if method == "GET" && path == format!("/api/v1/shared-folders/{ROOT}/keys") {
        let second = query.contains_key("cursor");
        let cut = f.inventory.len().min(500);
        json!({"keys": if second {&f.inventory[cut..]} else {&f.inventory[..cut]},"next_cursor":if f.cycle || (!second && cut<f.inventory.len()) {Some("opaque+cursor/=")} else {None}})
    } else if method == "GET" && path.starts_with("/api/v1/files/") {
        let id = path.trim_start_matches("/api/v1/files/");
        if let Some(row) = f.canonical.get(id).or_else(|| f.rows.iter().find(|r| r["id"] == id)) {
            row.clone()
        } else {
            return (StatusCode::NOT_FOUND, axum::Json(json!({"error":"not_found"}))).into_response();
        }
    } else {
        return (StatusCode::NOT_FOUND, Body::empty()).into_response();
    };
    axum::Json(value).into_response()
}
