mod archive;
mod folder;
mod single_file;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use beebeeb_core::kdf::{FileKey, MasterKey};
use serde_json::{Value, json};

const ROOT_ID: &str = "00000000-0000-4000-8000-000000000001";
const REQUEST_ID: &str = "11111111-2222-4333-8444-555555555555";
const BENIGN: &[u8] = b"benign body\n";
const PAYLOAD: &[u8] = b"ATTACKER PAYLOAD\n";
const LOCAL: &[u8] = b"MY LOCAL EDITS - precious\n";

static SEQ: AtomicUsize = AtomicUsize::new(0);

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

// ---------------------------------------------------------------------------
// Fixture: a vault tree served by the mock API
// ---------------------------------------------------------------------------

/// What `GET /files/:id/download` serves for one file.
struct Body {
    data: Vec<u8>,
    /// Plaintext size for `X-Original-Size`.
    size: usize,
    /// `X-Chunk-Count`.
    count: usize,
    /// `X-Chunk-Size` (the uniform plaintext chunk size), when the server sends one.
    chunk_size: Option<u64>,
}

struct Served {
    rows: HashMap<String, Value>,
    children: HashMap<String, Vec<String>>,
    bodies: HashMap<String, Body>,
    requests: Value,
    /// `GET /file-requests` answers 500 (the request keys cannot be loaded).
    requests_fail: bool,
}

struct Fixture {
    mk: MasterKey,
    r_pub: [u8; 32],
    next: u32,
    served: Served,
}

impl Fixture {
    fn new() -> Self {
        use rand::RngCore;
        let mk = MasterKey::from_bytes([0u8; 32]);
        let mut r_priv = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut r_priv);
        let r_pub = beebeeb_core::opaque::derive_x25519_public(&r_priv);
        let (wrapped, nonce) = beebeeb_core::file_request::wrap_request_private(&mk, b"", &r_priv).unwrap();
        let requests = json!({
            "file_requests": [{
                "id": REQUEST_ID,
                "wrapped_private_key": b64().encode(&wrapped),
                "wrap_nonce": b64().encode(&nonce),
            }]
        });
        let mut served = Served {
            rows: HashMap::new(),
            children: HashMap::new(),
            bodies: HashMap::new(),
            requests,
            requests_fail: false,
        };
        served.rows.insert(
            ROOT_ID.to_string(),
            json!({ "id": ROOT_ID, "name_encrypted": "evil-root", "is_folder": true, "chunk_count": 0, "size_bytes": 0 }),
        );
        Self {
            mk,
            r_pub,
            next: 100,
            served,
        }
    }

    fn fresh_id(&mut self) -> String {
        self.next += 1;
        format!("00000000-0000-4000-8000-{:012}", self.next)
    }

    /// The id the next [`Self::fresh_id`] call returns.
    fn next_id(&self) -> String {
        format!("00000000-0000-4000-8000-{:012}", self.next + 1)
    }

    /// A file or folder uploaded through the file request: the name is sealed
    /// under a per-item content key C that the uploader chose.
    fn add_request_item(&mut self, parent: &str, name: &str, is_folder: bool, body: &[u8]) -> String {
        self.add_request_item_chunked(parent, name, is_folder, body, None, None)
    }

    /// As [`Self::add_request_item`]; `chunk` splits the body into raw chunks of
    /// that many plaintext bytes (the last one shorter, like a real upload), and
    /// `header` is the `X-Chunk-Size` the server sends for it.
    fn add_request_item_chunked(
        &mut self,
        parent: &str,
        name: &str,
        is_folder: bool,
        body: &[u8],
        chunk: Option<usize>,
        header: Option<u64>,
    ) -> String {
        use rand::RngCore;
        let id = self.fresh_id();
        // A raw body whose random nonce starts with `{` is now handled by the
        // decrypt path (task 1759); the single-chunk fixtures still re-roll so
        // the older tests stay deterministic.
        let (c, fk, ct, count) = loop {
            let mut c = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut c);
            let fk = FileKey::from_bytes(c);
            let (ct, count) = match chunk {
                Some(cs) => {
                    let mut ct = Vec::new();
                    let mut n = 0;
                    for piece in body.chunks(cs) {
                        ct.extend(beebeeb_core::encrypt::encrypt_chunk_raw(&fk, piece).unwrap());
                        n += 1;
                    }
                    (ct, n)
                }
                None => (beebeeb_core::encrypt::encrypt_chunk_raw(&fk, body).unwrap(), 1),
            };
            if ct.first() != Some(&b'{') {
                break (c, fk, ct, count);
            }
        };
        let meta = json!({ "name": name, "mime_type": null }).to_string();
        let blob = beebeeb_core::encrypt::encrypt_metadata(&fk, &meta).unwrap();
        let name_encrypted = serde_json::to_string(&blob).unwrap();
        let sealed = beebeeb_core::file_request::seal_to_request(&self.r_pub, b"", &c).unwrap();
        let row = json!({
            "id": id,
            "name_encrypted": name_encrypted,
            "is_folder": is_folder,
            "chunk_count": if is_folder { 0 } else { count },
            "size_bytes": body.len(),
            "file_request_id": REQUEST_ID,
            "sender_ephemeral_pubkey": b64().encode(sealed.e_pub),
            "wrapped_content_key": b64().encode(&sealed.wrapped_key),
        });
        self.served.rows.insert(id.clone(), row);
        self.served
            .children
            .entry(parent.to_string())
            .or_default()
            .push(id.clone());
        if !is_folder {
            self.served.bodies.insert(
                id.clone(),
                Body {
                    data: ct,
                    size: body.len(),
                    count,
                    chunk_size: header,
                },
            );
        }
        id
    }

    /// A multi-chunk request upload (see [`Self::add_request_item_chunked`]).
    fn add_request_file_chunked(
        &mut self,
        parent: &str,
        name: &str,
        body: &[u8],
        chunk: usize,
        send_header: bool,
    ) -> String {
        let header = send_header.then_some(chunk as u64);
        self.add_request_item_chunked(parent, name, false, body, Some(chunk), header)
    }

    /// An ordinary file whose body is not valid ciphertext under any key.
    fn add_corrupt_file(&mut self, parent: &str, name: &str) -> String {
        let id = self.add_plain_row(parent, name);
        self.served.bodies.insert(
            id.clone(),
            Body {
                data: vec![0xAB; 96],
                size: 68,
                count: 1,
                chunk_size: None,
            },
        );
        id
    }

    /// A file row the server lists but cannot serve (download answers 404).
    fn add_plain_row(&mut self, parent: &str, name: &str) -> String {
        let id = self.fresh_id();
        let name_key = beebeeb_core::kdf::derive_file_key(&self.mk, id.as_bytes());
        let name_encrypted = beebeeb_core::encrypt::encrypt_name_for(
            &name_key,
            name,
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        let row = json!({
            "id": id, "name_encrypted": name_encrypted, "is_folder": false, "chunk_count": 1, "size_bytes": 68,
        });
        self.served.rows.insert(id.clone(), row);
        self.served
            .children
            .entry(parent.to_string())
            .or_default()
            .push(id.clone());
        id
    }

    /// List the existing item `child` under `parent` as well (a hostile server
    /// can hand back a folder cycle).
    fn link_child(&mut self, parent: &str, child: &str) {
        self.served
            .children
            .entry(parent.to_string())
            .or_default()
            .push(child.to_string());
    }

    fn add_request_file(&mut self, parent: &str, name: &str, body: &[u8]) -> String {
        self.add_request_item(parent, name, false, body)
    }

    fn add_request_folder(&mut self, parent: &str, name: &str) -> String {
        self.add_request_item(parent, name, true, b"")
    }

    /// An ordinary folder with a plaintext name. `parent` "" is the vault root.
    fn add_plain_folder(&mut self, parent: &str, name: &str) -> String {
        let id = self.fresh_id();
        let row = json!({
            "id": id, "name_encrypted": name, "is_folder": true, "chunk_count": 0, "size_bytes": 0,
        });
        self.served.rows.insert(id.clone(), row);
        self.served
            .children
            .entry(parent.to_string())
            .or_default()
            .push(id.clone());
        id
    }

    /// An ordinary (non-request) file, its name sealed under the account's
    /// master key. A FILE's `name_encrypted` must be an encrypted envelope (the
    /// 1908 read path takes the chunk-format pin from it, task 1943); the tests
    /// still choose any name they like to exercise the client's path guards.
    fn add_plain_file(&mut self, parent: &str, name: &str, body: &[u8]) -> String {
        let id = self.next_id();
        let name_key = beebeeb_core::kdf::derive_file_key(&self.mk, id.as_bytes());
        let name_encrypted = beebeeb_core::encrypt::encrypt_name_for(
            &name_key,
            name,
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        self.add_file_row(parent, name_encrypted, body)
    }

    /// A file whose `name_encrypted` is a bare PLAINTEXT string: there is no
    /// authenticated metadata, so no read pin, and its content is never read
    /// (task 1943, core round 3 P1-B).
    fn add_plaintext_named_file(&mut self, parent: &str, name: &str, body: &[u8]) -> String {
        self.add_file_row(parent, name.to_string(), body)
    }

    #[allow(deprecated)] // fixture: a fixed id and chunk size, which the NewItemId/WriteTarget writers cannot express
    fn add_file_row(&mut self, parent: &str, name_encrypted: String, body: &[u8]) -> String {
        let id = self.fresh_id();
        let row = json!({
            "id": id,
            "name_encrypted": name_encrypted,
            "is_folder": false,
            "chunk_count": 1,
            "size_bytes": body.len(),
        });
        self.served.rows.insert(id.clone(), row);
        self.served
            .children
            .entry(parent.to_string())
            .or_default()
            .push(id.clone());
        // Same `{`-first-byte re-roll as for request uploads (random nonce).
        let data = loop {
            let mut enc = beebeeb_core::chunk_stream::ChunkEncryptor::for_push_with_chunk_size(
                &self.mk,
                &id,
                body.len() as u64,
                1024 * 1024,
            )
            .unwrap();
            let chunk = enc.push_chunk(body).unwrap();
            enc.finish().unwrap();
            if chunk.data.first() != Some(&b'{') {
                break chunk.data;
            }
        };
        self.served.bodies.insert(
            id.clone(),
            Body {
                data,
                size: body.len(),
                count: 1,
                chunk_size: None,
            },
        );
        id
    }

    /// Make `GET /file-requests` fail, so the request keys cannot be loaded.
    fn fail_requests_endpoint(&mut self) {
        self.served.requests_fail = true;
    }

    fn serve(self) -> String {
        let state = Arc::new(self.served);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                async fn list(
                    State(s): State<Arc<Served>>,
                    Query(q): Query<HashMap<String, String>>,
                ) -> impl IntoResponse {
                    let parent = q.get("parent_id").cloned().unwrap_or_default();
                    let files: Vec<Value> = s
                        .children
                        .get(&parent)
                        .map(|ids| ids.iter().filter_map(|i| s.rows.get(i).cloned()).collect())
                        .unwrap_or_default();
                    Json(json!({ "files": files }))
                }
                async fn meta(State(s): State<Arc<Served>>, AxPath(id): AxPath<String>) -> impl IntoResponse {
                    match s.rows.get(&id) {
                        Some(r) => Json(r.clone()).into_response(),
                        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
                    }
                }
                async fn download(State(s): State<Arc<Served>>, AxPath(id): AxPath<String>) -> impl IntoResponse {
                    match s.bodies.get(&id) {
                        Some(b) => {
                            let mut h = header::HeaderMap::new();
                            h.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
                            h.insert("x-chunk-count", b.count.to_string().parse().unwrap());
                            h.insert("x-original-size", b.size.to_string().parse().unwrap());
                            if let Some(cs) = b.chunk_size {
                                h.insert("x-chunk-size", cs.to_string().parse().unwrap());
                            }
                            (h, b.data.clone()).into_response()
                        }
                        None => (StatusCode::NOT_FOUND, "not found").into_response(),
                    }
                }
                async fn requests(State(s): State<Arc<Served>>) -> impl IntoResponse {
                    if s.requests_fail {
                        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))).into_response();
                    }
                    Json(s.requests.clone()).into_response()
                }
                let app = Router::new()
                    .route(
                        "/api/v1/auth/me",
                        get(|| async { Json(json!({"id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"})) }),
                    )
                    .route("/api/v1/files", get(list))
                    .route("/api/v1/files/:id", get(meta))
                    .route("/api/v1/files/:id/download", get(download))
                    .route("/api/v1/file-requests", get(requests))
                    .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))) })
                    .with_state(state);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                let _ = axum::serve(listener, app).await;
            });
        });
        format!("http://{}", rx.recv().unwrap())
    }
}

// ---------------------------------------------------------------------------
// Scratch tree + running the binary
// ---------------------------------------------------------------------------

struct Scratch {
    root: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl Scratch {
    fn new(tag: &str, api_url: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pull-trav-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        // Canonicalise so snapshot paths and symlink targets agree.
        let root = root.canonicalize().unwrap();
        let s = Self {
            home: root.join("home"),
            cwd: root.join("cwd"),
            root,
        };
        s.point_at(api_url);
        s
    }

    /// (Re)write the scratch `bb` config so it talks to `api_url` with the
    /// all-zero test master key.
    fn point_at(&self, api_url: &str) {
        let config = json!({
            "api_url": api_url,
            "session_token": "test-session-token",
            "email": "sec1733@beebeeb.io",
            "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        });
        for dir in [
            self.home.join("Library/Application Support/beebeeb"),
            self.home.join(".config/beebeeb"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
        }
    }

    fn out_dir(&self) -> PathBuf {
        self.cwd.join("out")
    }

    fn bb(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .args(args)
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .expect("run bb")
    }

    /// As [`Self::bb`], but killed (own child PID) after `secs` seconds.
    /// `None` = it did not finish in time.
    fn bb_timeout(&self, args: &[&str], secs: u64) -> Option<Output> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bb"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .args(args)
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn bb");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if child.try_wait().expect("try_wait").is_some() {
                return Some(child.wait_with_output().expect("collect output"));
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Every path in the scratch tree EXCEPT the contents of `out/` (symlinks
    /// are listed, never followed). If this set changes during a pull, the
    /// pull wrote outside the directory the user chose.
    fn snapshot_outside_out(&self) -> BTreeSet<String> {
        fn walk(dir: &Path, root: &Path, out_dir: &Path, acc: &mut BTreeSet<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if p == out_dir {
                    continue; // contents checked separately; existence may change
                }
                acc.insert(rel);
                if is_dir {
                    walk(&p, root, out_dir, acc);
                }
            }
        }
        let mut acc = BTreeSet::new();
        walk(&self.root, &self.root, &self.out_dir(), &mut acc);
        acc
    }

    /// Entry names directly inside `out/`.
    fn out_entries(&self) -> BTreeSet<String> {
        match std::fs::read_dir(self.out_dir()) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => BTreeSet::new(),
        }
    }
}

fn describe(out: &Output) -> String {
    format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Read every entry of a zip archive into `name -> bytes`.
fn read_zip(path: &Path) -> HashMap<String, Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let mut z = zip::ZipArchive::new(f).expect("valid zip archive");
    (0..z.len())
        .map(|i| {
            let mut e = z.by_index(i).unwrap();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            (e.name().to_string(), buf)
        })
        .collect()
}

/// Archive entry names, sorted (unlike [`read_zip`], duplicates stay visible).
fn zip_names(path: &Path) -> Vec<String> {
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let mut z = zip::ZipArchive::new(f).expect("valid zip archive");
    let mut names: Vec<String> = (0..z.len())
        .map(|i| z.by_index(i).unwrap().name().to_string())
        .collect();
    names.sort();
    names
}

/// A deterministic body that is not a multiple of any chunk size used below.
fn uneven_body(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}
