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
    sync::{Arc, Mutex},
};
pub const ROOT: &str = "22222222-2222-4222-8222-222222222222";
pub const SUB: &str = "33333333-3333-4333-8333-333333333333";
pub const FILE: &str = "11111111-1111-4111-8111-111111111111";
pub const BAD: &str = "44444444-4444-4444-8444-444444444444";
pub const ACCOUNT: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
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
    json!({"id":id,"parent_id":parent,"name_encrypted":name(&key,text),"is_folder":folder,"size_bytes":12,"chunk_count":1,"version_number":1,"created_at":"2026-10-28T10:00:00Z","updated_at":"2026-10-28T10:00:00Z","acted_via_share_id":if foreign {Some(BAD)} else {None},"uploaded_by":null,"owner_wrapped_file_key":null})
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
    pub frames: Vec<Vec<u8>>,
    pub stride: u64,
    pub plaintext: Vec<u8>,
    pub direct: bool,
    pub changed: bool,
    pub custody_revoked: bool,
    pub adopt_failures: usize,
    pub adopt_lost: bool,
    pub index_conflicts: usize,
    pub concurrent_shared: HashMap<(u32, u32), (u64, Vec<u8>)>,
    pub index_partial: bool,
    pub review_partial_repair_only: bool,
    pub review_complete_requested_id: bool,
    pub shared_index: HashMap<(u32, u32), (u64, Vec<u8>)>,
    pub owner_index: HashMap<(u32, u32), (u64, Vec<u8>)>,
    pub upload_request: Value,
    pub uploaded_chunks: Vec<Vec<u8>>,
}
impl Fixture {
    pub fn transfer(v2: bool) -> Self {
        use beebeeb_core::chunk_stream::{ChunkEncryptor, ExpectedChunkFormat};
        let mut f = Self::basic();
        f.plaintext = b"guest-content: lots of bytes across frames".to_vec();
        f.stride = 8;
        let pin = if v2 {
            ExpectedChunkFormat::V2
        } else {
            ExpectedChunkFormat::Unrecorded
        };
        let mut enc = ChunkEncryptor::from_reader_with_file_key(
            item_key(),
            FILE,
            f.plaintext.len() as u64,
            f.stride,
            std::io::Cursor::new(f.plaintext.clone()),
        )
        .unwrap()
        .with_recorded_pin(pin)
        .unwrap();
        while let Some(c) = enc.next_chunk().unwrap() {
            f.frames.push(c.data);
        }
        enc.finish().unwrap();
        if !v2 {
            f.frames = f
                .plaintext
                .chunks(f.stride as usize)
                .map(|c| beebeeb_core::encrypt::encrypt_chunk_raw(&item_key(), c).unwrap())
                .collect();
        }
        f.rows[1]["name_encrypted"] =
            json!(beebeeb_core::encrypt::encrypt_name_for(&item_key(), "guest.txt", None, pin).unwrap());
        f.rows[1]["size_bytes"] = json!(f.plaintext.len());
        f.rows[1]["chunk_count"] = json!(f.frames.len());
        f
    }
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
        r["size_bytes"] = json!(self.plaintext.len());
        r["chunk_count"] = json!(1);
        if !self.plaintext.is_empty() {
            self.frames = vec![beebeeb_core::encrypt::encrypt_chunk_raw(&item_key(), &self.plaintext).unwrap()];
            self.stride = self.plaintext.len() as u64;
        }
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
    let host = req.headers().get("host").unwrap().to_str().unwrap().to_string();
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
    // Shared CAS and ordinary LWW stores intentionally have different contracts.
    if path.contains("/search-index/shards") {
        let shared = path.starts_with("/api/v1/shared-folders/");
        let suffix = path.split("/search-index/shards").nth(1).unwrap();
        if suffix.is_empty() {
            let index = if shared { &f.shared_index } else { &f.owner_index };
            return axum::Json(json!({"shards":index.iter().map(|(&(bucket,page),(version,_))| json!({"bucket":bucket,"page":page,"version":version})).collect::<Vec<_>>()})).into_response();
        }
        let parts: Vec<_> = suffix.trim_matches('/').split('/').collect();
        let key = (parts[0].parse::<u32>().unwrap(), parts[1].parse::<u32>().unwrap());
        if method == "GET" {
            return match (if shared { &f.shared_index } else { &f.owner_index }).get(&key) {
                Some((_, b)) => b.clone().into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            };
        }
        if shared && f.index_conflicts > 0 {
            f.index_conflicts -= 1;
            if !f.concurrent_shared.is_empty() {
                f.shared_index = std::mem::take(&mut f.concurrent_shared);
            }
            return (
                StatusCode::CONFLICT,
                axum::Json(json!({"error":"index_version_conflict"})),
            )
                .into_response();
        }
        let bytes = f.requests.last().unwrap().2.clone();
        let index = if shared {
            &mut f.shared_index
        } else {
            &mut f.owner_index
        };
        let version = index.get(&key).map(|v| v.0).unwrap_or(0);
        if shared {
            assert_eq!(
                query.get("if_version").and_then(|s| s.parse::<u64>().ok()),
                Some(version)
            );
        } else {
            assert!(!query.contains_key("if_version"));
        }
        if method == "PUT" {
            index.insert(key, (version + 1, bytes));
        } else if method == "DELETE" {
            index.remove(&key);
        }
        return axum::Json(json!({"version":version+1})).into_response();
    }
    if method == "POST" && path.ends_with("/owner-key") {
        if f.adopt_failures > 0 {
            f.adopt_failures -= 1;
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let id = path.trim_start_matches("/api/v1/files/").trim_end_matches("/owner-key");
        let wrap =
            serde_json::from_slice::<Value>(&f.requests.last().unwrap().2).unwrap()["owner_wrapped_file_key"].clone();
        if let Some(r) = f.rows.iter_mut().find(|r| r["id"] == id) {
            if r["owner_wrapped_file_key"].is_null() {
                r["owner_wrapped_file_key"] = wrap;
            }
        }
        if let Some(r) = f.rows.iter().find(|r| r["id"] == id).cloned() {
            if let Some(canonical) = f.canonical.get_mut(id) {
                canonical["owner_wrapped_file_key"] = r["owner_wrapped_file_key"].clone();
            }
        }
        if f.adopt_lost {
            f.adopt_lost = false;
            return Response::new(Body::from_stream(futures_util::stream::once(async {
                Err::<bytes::Bytes, _>(std::io::Error::other("fixture response lost after commit"))
            })));
        }
        return axum::Json(json!({"adopted":true})).into_response();
    }
    let value = if method == "GET" && path == "/api/v1/auth/me" {
        json!({"id":ACCOUNT})
    } else if method == "GET" && path == "/api/v1/transfer/capabilities" {
        json!({"download":if f.direct {vec!["object-download-v1"]} else {vec![]},"upload":[],"storage_hosts":[host],"storage_buckets":[{"host":host,"bucket":"fixture"}],"max_window":32,"url_ttl_seconds":300})
    } else if method == "POST" && path.ends_with("/download-grants") {
        if f.changed {
            return (StatusCode::CONFLICT, axum::Json(json!({"error":"version_unavailable"}))).into_response();
        }
        let request: Value = serde_json::from_slice(&f.requests.last().unwrap().2).unwrap();
        let start = request["start_index"].as_u64().unwrap_or(0) as usize;
        let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        json!({"transport":"object-download-v1","file_id":FILE,"object_version_id":SUB,"chunk_count":f.frames.len(),"chunk_size":f.stride,"encrypted_size":f.frames.iter().map(Vec::len).sum::<usize>(),"original_size":f.plaintext.len(),"grants":(start..f.frames.len()).map(|i|json!({"index":i,"method":"GET","url":format!("http://{host}/fixture/{FILE}/{i}?X-Amz-Signature=fixture&X-Amz-Expires=300"),"length":f.frames[i].len(),"expires_at":expires})).collect::<Vec<_>>(),"next_index":null})
    } else if method == "GET" && path.starts_with("/fixture/") {
        let i = path.rsplit('/').next().unwrap().parse::<usize>().unwrap();
        return f.frames[i].clone().into_response();
    } else if method == "GET" && path.ends_with("/download") {
        return (
            [
                ("X-Chunk-Size", f.stride.to_string()),
                ("X-Chunk-Count", f.frames.len().to_string()),
                ("X-Original-Size", f.plaintext.len().to_string()),
            ],
            f.frames.concat(),
        )
            .into_response();
    } else if method == "POST" && path == "/api/v1/files/check-conflict" {
        json!({"files":f.rows.iter().filter(|r|r["parent_id"]==serde_json::from_slice::<Value>(&f.requests.last().unwrap().2).unwrap()["parent_id"]).cloned().collect::<Vec<_>>()})
    } else if method == "GET" && path == "/api/v1/billing/subscription" {
        json!({"plan":"personal","quota_bytes":1000000000,"used_bytes":0})
    } else if method == "GET" && path == "/api/v1/files/usage" {
        json!({"used_bytes":0})
    } else if method == "POST" && path == "/api/v1/uploads/init" {
        let request: Value = serde_json::from_slice(&f.requests.last().unwrap().2).unwrap();
        f.upload_request = request.clone();
        f.uploaded_chunks.clear();
        json!({"upload_session_id":BAD,"file_id":request["file_id"].as_str().unwrap_or(FILE),"chunk_size_bytes":request["chunk_size_bytes"],"chunk_count":request["chunk_count"]})
    } else if method == "PUT" && path.contains("/chunks/") {
        let b = f.requests.last().unwrap().2.clone();
        f.uploaded_chunks.push(b);
        json!({"stored":true})
    } else if method == "POST" && path.ends_with("/complete") {
        let req = f.upload_request.clone();
        if req["file_id"] == FILE || f.review_complete_requested_id {
            let chunks = f.uploaded_chunks.len();
            let target = if f.review_complete_requested_id {
                req["file_id"].as_str().unwrap_or(FILE)
            } else {
                FILE
            };
            if let Some(r) = f.rows.iter_mut().find(|r| r["id"] == target) {
                r["name_encrypted"] = req["file_name"].clone();
                r["version_number"] = json!(r["version_number"].as_u64().unwrap() + 1);
                r["size_bytes"] = req["size_bytes"].clone();
                r["chunk_count"] = json!(chunks);
            }
        }
        let completed_id = if f.review_complete_requested_id {
            req["file_id"].as_str().unwrap_or(FILE)
        } else {
            FILE
        };
        json!({"file_id":completed_id,"id":completed_id})
    } else if method == "GET" && path == "/api/v1/file-requests" {
        f.requests_doc.clone()
    } else if method == "GET" && path == "/api/v1/shares/invites/sent" {
        json!({"invites":if f.legacy_invite {vec![json!({"id":BAD,"file_id":ROOT,"is_folder_share":true,"status":"approved","encrypted_owner_folder_key":B64.encode(owned_keys::wrap_key_to_owner(&master(),ROOT,&folder_key()).unwrap())})]} else {vec![]}})
    } else if method == "GET" && path == format!("/api/v1/shares/invites/{BAD}/folder-keys") {
        json!({"keys":f.inventory})
    } else if (f.legacy_invite || f.custody_revoked) && path == format!("/api/v1/shared-folders/{ROOT}") {
        return StatusCode::NOT_FOUND.into_response();
    } else if method == "GET" && path == "/api/v1/files" {
        let mut rows = f
            .rows
            .iter()
            .filter(|r| {
                r["parent_id"].as_str() == query.get("parent_id").map(String::as_str)
                    && r["is_trashed"].as_bool().unwrap_or(false)
                        == (query.get("trashed").map(String::as_str) == Some("true"))
            })
            .cloned()
            .collect::<Vec<_>>();
        let root_listings = f
            .requests
            .iter()
            .filter(|(m, p, _)| m == "GET" && p.starts_with(&format!("/api/v1/files?parent_id={ROOT}")))
            .count();
        if (f.index_partial || f.review_partial_repair_only && root_listings >= 2)
            && query.get("parent_id").map(String::as_str) == Some(ROOT)
        {
            let mut bad = row(BAD, Some(ROOT), "unreadable", false, true);
            bad["name_encrypted"] = json!("corrupt");
            rows.push(bad);
        }
        json!({"files":rows})
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
