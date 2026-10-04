//! Task 1733 [P0]: `bb pull` must never write outside the directory the user
//! chose, whatever file or folder names the server hands back.
//!
//! The attack: an anonymous uploader with a victim's file-request link seals
//! the upload under a content key *they* chose, so the file name the victim's
//! CLI decrypts is entirely attacker-controlled (`../../.ssh/authorized_keys`,
//! an absolute path, ...). Before the fix `pull_folder_inner` did
//! `out_dir.join(&decrypted_name)` + `create_dir_all` + `fs::write` with no
//! validation and no containment check.
//!
//! These tests drive the REAL `bb` binary against an in-process mock API that
//! serves a folder tree whose names are malicious. Every run gets a scratch
//! `HOME` (the developer's real config holds a live session) and
//! `BB_NO_UPDATE=1`, so nothing leaves the loopback. They assert on the file
//! system: a snapshot of everything in the scratch tree *outside* the output
//! directory must be unchanged after the run.

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

struct Served {
    rows: HashMap<String, Value>,
    children: HashMap<String, Vec<String>>,
    /// id -> (download body, plaintext size for `X-Original-Size`)
    bodies: HashMap<String, (Vec<u8>, usize)>,
    requests: Value,
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

    /// A file or folder uploaded through the file request: the name is sealed
    /// under a per-item content key C that the uploader chose.
    fn add_request_item(&mut self, parent: &str, name: &str, is_folder: bool, body: &[u8]) -> String {
        use rand::RngCore;
        let id = self.fresh_id();
        // `bb`'s chunk-format sniffing treats a body whose first byte is `{` as
        // a legacy JSON blob; the random nonce hits that 1 time in 256 (a
        // separate, pre-existing flake, see task 1733 Notes). Re-roll the
        // content key until the fixture body is unambiguous, so these tests
        // are deterministic.
        let (c, fk, ct) = loop {
            let mut c = [0u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut c);
            let fk = FileKey::from_bytes(c);
            let ct = beebeeb_core::encrypt::encrypt_chunk_raw(&fk, body).unwrap();
            if ct.first() != Some(&b'{') {
                break (c, fk, ct);
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
            "chunk_count": if is_folder { 0 } else { 1 },
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
            self.served.bodies.insert(id.clone(), (ct, body.len()));
        }
        id
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

    /// An ordinary (non-request) file whose `name_encrypted` is a plaintext
    /// string — the server controls it completely (the plaintext fast path).
    fn add_plain_file(&mut self, parent: &str, name: &str, body: &[u8]) -> String {
        let id = self.fresh_id();
        let row = json!({
            "id": id,
            "name_encrypted": name,
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
        self.served.bodies.insert(id.clone(), (data, body.len()));
        id
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
                        Some((body, size)) => (
                            [
                                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                                (header::HeaderName::from_static("x-chunk-count"), "1".to_string()),
                                (header::HeaderName::from_static("x-original-size"), size.to_string()),
                            ],
                            body.clone(),
                        )
                            .into_response(),
                        None => (StatusCode::NOT_FOUND, "not found").into_response(),
                    }
                }
                async fn requests(State(s): State<Arc<Served>>) -> impl IntoResponse {
                    Json(s.requests.clone())
                }
                let app = Router::new()
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

/// Control: a folder pull of ordinary request uploads still works (including a
/// nested real subfolder and a non-ASCII name), so the refusals below are not
/// a broken fixture.
#[test]
fn folder_pull_happy_path_still_works() {
    let mut fx = Fixture::new();
    fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
    fx.add_request_file(ROOT_ID, "rapport Q3 \u{00e9}\u{00e8}.pdf", b"unicode name\n");
    let sub = fx.add_request_folder(ROOT_ID, "sub");
    fx.add_request_file(&sub, "inner.txt", b"nested\n");
    fx.add_plain_file(ROOT_ID, "plain.txt", b"plain body\n");
    let url = fx.serve();
    let s = Scratch::new("happy", &url);

    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(std::fs::read(s.out_dir().join("ok.txt")).unwrap(), BENIGN);
    assert_eq!(
        std::fs::read(s.out_dir().join("rapport Q3 \u{00e9}\u{00e8}.pdf")).unwrap(),
        b"unicode name\n"
    );
    assert_eq!(std::fs::read(s.out_dir().join("sub/inner.txt")).unwrap(), b"nested\n");
    assert_eq!(std::fs::read(s.out_dir().join("plain.txt")).unwrap(), b"plain body\n");
}

/// Malicious FILE names, from a request upload (attacker-chosen content key)
/// and from a plaintext-name row. Each case: the hostile entry sits first, a
/// benign sibling second. Required: nothing appears outside `out/`, nothing
/// hostile lands inside `out/` either, the benign sibling is still pulled, and
/// the run exits 3 (finished with skipped items) naming the unsafe name.
#[test]
fn folder_pull_rejects_hostile_file_names() {
    // Scratch layout: <root>/cwd/out is the output dir, so `../../x` lands in
    // <root>, `a/../../x` lands in <root>/cwd, and an absolute path is built
    // per-case from the scratch root.
    type NameFn = Box<dyn Fn(&Path) -> String>;
    let cases: Vec<(&str, NameFn)> = vec![
        ("dotdot_dotdot", Box::new(|_| "../../escape.txt".to_string())),
        ("dotdot_one", Box::new(|_| "../escape.txt".to_string())),
        ("nested_dotdot", Box::new(|_| "a/../../x".to_string())),
        (
            "absolute",
            Box::new(|root| root.join("abs-escape.txt").display().to_string()),
        ),
        ("subdir_separator", Box::new(|_| "dir/file.txt".to_string())),
        ("backslash_dotdot", Box::new(|_| "..\\x".to_string())),
        ("windows_drive", Box::new(|_| "C:\\x".to_string())),
        ("windows_drive_relative", Box::new(|_| "C:x".to_string())),
        ("single_dot", Box::new(|_| ".".to_string())),
        ("double_dot", Box::new(|_| "..".to_string())),
        ("empty", Box::new(|_| String::new())),
        ("nul_byte", Box::new(|_| "a\0b".to_string())),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (kind, mk_name) in &cases {
        for source in ["request", "plain"] {
            let label = format!("{kind}/{source}");
            // The absolute-path case needs the scratch root, which needs the
            // scratch dir, which needs the server URL: create the scratch first
            // against a placeholder URL, then repoint its config at the mock.
            let placeholder = Scratch::new(&format!("{kind}-{source}"), "http://127.0.0.1:1");
            let name = mk_name(&placeholder.root);

            let mut fx = Fixture::new();
            if source == "request" {
                fx.add_request_file(ROOT_ID, &name, PAYLOAD);
            } else {
                fx.add_plain_file(ROOT_ID, &name, PAYLOAD);
            }
            fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
            placeholder.point_at(&fx.serve());

            let before = placeholder.snapshot_outside_out();
            let out = placeholder.bb(&["pull", ROOT_ID, "-o", "out"]);
            let after = placeholder.snapshot_outside_out();

            let escaped: Vec<_> = after.difference(&before).cloned().collect();
            if !escaped.is_empty() {
                failures.push(format!("{label}: wrote OUTSIDE out/: {escaped:?}\n{}", describe(&out)));
                continue;
            }
            let entries = placeholder.out_entries();
            let stray: Vec<_> = entries.iter().filter(|e| e.as_str() != "ok.txt").cloned().collect();
            if !stray.is_empty() {
                failures.push(format!(
                    "{label}: hostile entry created inside out/: {stray:?}\n{}",
                    describe(&out)
                ));
                continue;
            }
            match std::fs::read(placeholder.out_dir().join("ok.txt")) {
                Ok(b) if b == BENIGN => {}
                other => {
                    failures.push(format!(
                        "{label}: benign sibling not pulled: {other:?}\n{}",
                        describe(&out)
                    ));
                    continue;
                }
            }
            if out.status.code() != Some(3) {
                failures.push(format!(
                    "{label}: expected exit 3, got {:?}\n{}",
                    out.status.code(),
                    describe(&out)
                ));
                continue;
            }
            if !String::from_utf8_lossy(&out.stderr).contains("unsafe") {
                failures.push(format!(
                    "{label}: stderr does not name the unsafe name\n{}",
                    describe(&out)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n\n{}",
        failures.len(),
        cases.len() * 2,
        failures.join("\n\n")
    );
}

/// A hostile FOLDER name (`..`, `../..`, absolute) must not be descended into:
/// its children would otherwise be written into the parent directory.
#[test]
fn folder_pull_rejects_hostile_folder_names() {
    let mut failures: Vec<String> = Vec::new();
    for (kind, name) in [("dotdot", ".."), ("dotdot_two", "../.."), ("slash", "x/y")] {
        let mut fx = Fixture::new();
        let evil = fx.add_request_folder(ROOT_ID, name);
        fx.add_request_file(&evil, "inner.txt", PAYLOAD);
        fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
        let url = fx.serve();
        let s = Scratch::new(&format!("folder-{kind}"), &url);

        let before = s.snapshot_outside_out();
        let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);
        let after = s.snapshot_outside_out();

        let escaped: Vec<_> = after.difference(&before).cloned().collect();
        if !escaped.is_empty() {
            failures.push(format!("{kind}: wrote OUTSIDE out/: {escaped:?}\n{}", describe(&out)));
            continue;
        }
        let stray: Vec<_> = s.out_entries().into_iter().filter(|e| e != "ok.txt").collect();
        if !stray.is_empty() {
            failures.push(format!(
                "{kind}: hostile entry inside out/: {stray:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        if out.status.code() != Some(3) {
            failures.push(format!(
                "{kind}: expected exit 3, got {:?}\n{}",
                out.status.code(),
                describe(&out)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A pre-existing symlink inside the output dir must not be followed out of it,
/// even though the folder name itself (`sub`) is perfectly innocent.
#[cfg(unix)]
#[test]
fn folder_pull_does_not_follow_preexisting_symlink_out_of_target() {
    let mut fx = Fixture::new();
    let sub = fx.add_request_folder(ROOT_ID, "sub");
    fx.add_request_file(&sub, "inner.txt", PAYLOAD);
    fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("symlink-dir", &url);

    let outside = s.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(s.out_dir()).unwrap();
    std::os::unix::fs::symlink(&outside, s.out_dir().join("sub")).unwrap();

    let before = s.snapshot_outside_out();
    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);
    let after = s.snapshot_outside_out();

    let escaped: Vec<_> = after.difference(&before).cloned().collect();
    assert!(
        escaped.is_empty(),
        "followed the symlink out of out/: {escaped:?}\n{}",
        describe(&out)
    );
    assert!(!outside.join("inner.txt").exists(), "{}", describe(&out));
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert_eq!(std::fs::read(s.out_dir().join("ok.txt")).unwrap(), BENIGN);
}

/// Same, for a symlink sitting where a FILE is about to be written: the write
/// must not go through it to its target.
#[cfg(unix)]
#[test]
fn folder_pull_does_not_write_through_preexisting_file_symlink() {
    let mut fx = Fixture::new();
    fx.add_request_file(ROOT_ID, "link.txt", PAYLOAD);
    fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("symlink-file", &url);

    let victim = s.root.join("victim.txt");
    std::fs::write(&victim, LOCAL).unwrap();
    std::fs::create_dir_all(s.out_dir()).unwrap();
    std::os::unix::fs::symlink(&victim, s.out_dir().join("link.txt")).unwrap();

    // --force must not turn a symlink into a write-through either.
    let out = s.bb(&["pull", ROOT_ID, "-o", "out", "--force"]);

    assert_eq!(
        std::fs::read(&victim).unwrap(),
        LOCAL,
        "the symlink target was overwritten\n{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
}

/// `guard_existing_output` must also fire on the request-key branch of a folder
/// pull: an attacker-named file must not silently replace a local file of the
/// same name (without --force); with --force it is the user's explicit choice.
#[test]
fn folder_pull_request_branch_refuses_to_overwrite_without_force() {
    let mut fx = Fixture::new();
    fx.add_request_file(ROOT_ID, "note.txt", PAYLOAD);
    let url = fx.serve();
    let s = Scratch::new("guard", &url);
    std::fs::create_dir_all(s.out_dir()).unwrap();
    std::fs::write(s.out_dir().join("note.txt"), LOCAL).unwrap();

    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);
    assert_eq!(
        std::fs::read(s.out_dir().join("note.txt")).unwrap(),
        LOCAL,
        "existing local file was overwritten without --force\n{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--force"),
        "{}",
        describe(&out)
    );

    let forced = s.bb(&["pull", ROOT_ID, "-o", "out", "--force"]);
    assert!(forced.status.success(), "{}", describe(&forced));
    assert_eq!(std::fs::read(s.out_dir().join("note.txt")).unwrap(), PAYLOAD);
}

/// Single-file `bb pull <id>` with no `-o`: the default output path is built
/// from the server-provided name and must be refused when unsafe.
#[test]
fn single_file_default_output_rejects_hostile_names() {
    let mut failures: Vec<String> = Vec::new();
    for (kind, source) in [
        ("../escape.txt", "request"),
        ("../escape.txt", "plain"),
        ("a/../../x", "request"),
        ("abs", "request"),
        ("abs", "plain"),
    ] {
        let placeholder = Scratch::new("single", "http://127.0.0.1:1");
        let name = if kind == "abs" {
            placeholder.root.join("abs-single.txt").display().to_string()
        } else {
            kind.to_string()
        };
        let mut fx = Fixture::new();
        let id = if source == "request" {
            fx.add_request_file(ROOT_ID, &name, PAYLOAD)
        } else {
            fx.add_plain_file(ROOT_ID, &name, PAYLOAD)
        };
        placeholder.point_at(&fx.serve());

        let before = placeholder.snapshot_outside_out();
        let out = placeholder.bb(&["pull", &id]);
        let after = placeholder.snapshot_outside_out();

        let label = format!("{kind}/{source}");
        let escaped: Vec<_> = after.difference(&before).cloned().collect();
        if !escaped.is_empty() {
            failures.push(format!(
                "{label}: wrote outside cwd or created a file: {escaped:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        if out.status.success() {
            failures.push(format!("{label}: expected a refusal, got success\n{}", describe(&out)));
            continue;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !(stderr.contains("unsafe") && stderr.contains("-o")) {
            failures.push(format!(
                "{label}: refusal must say 'unsafe' and point at -o\n{}",
                describe(&out)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// An explicit `-o <path>` is the user's own choice and is never second-guessed
/// (it may legitimately be absolute or contain `..`).
#[test]
fn explicit_output_path_is_honoured() {
    let mut fx = Fixture::new();
    let id = fx.add_request_file(ROOT_ID, "../would-be-unsafe.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("explicit-o", &url);
    let target = s.root.join("chosen.txt");

    let out = s.bb(&["pull", &id, "-o", target.to_str().unwrap()]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(std::fs::read(&target).unwrap(), BENIGN);
}

/// `bb sync` mirrors a remote folder into a local directory. A hostile remote
/// name (`../../escape.txt`, absolute, separators) must be skipped, never
/// joined onto the sync root; legitimate files in the same folder still sync.
/// (The sync variant is weaker than pull: names only reach it from a hostile
/// server, since sync decrypts with the master key alone.)
#[test]
fn sync_download_skips_hostile_remote_names() {
    let mut failures: Vec<String> = Vec::new();
    for kind in ["dotdot", "absolute", "separator"] {
        let placeholder = Scratch::new(&format!("sync-{kind}"), "http://127.0.0.1:1");
        // Sync root is <root>/cwd/local, so `../../escape.txt` lands in <root>.
        let local = placeholder.cwd.join("local");
        std::fs::create_dir_all(&local).unwrap();
        let name = match kind {
            "dotdot" => "../../escape.txt".to_string(),
            "absolute" => placeholder.root.join("abs-sync.txt").display().to_string(),
            _ => "dir/inner.txt".to_string(),
        };

        let mut fx = Fixture::new();
        let vault = fx.add_plain_folder("", "vault");
        fx.add_plain_file(&vault, &name, PAYLOAD);
        fx.add_plain_file(&vault, "ok.txt", BENIGN);
        placeholder.point_at(&fx.serve());

        let before = placeholder.snapshot_outside_out();
        let out = placeholder.bb(&["sync", local.to_str().unwrap(), "/vault", "--once"]);
        let after = placeholder.snapshot_outside_out();

        let new: Vec<_> = after
            .difference(&before)
            // `home/` is bb's own config dir (device.json); `cwd/local/` is the sync root.
            .filter(|p| !p.starts_with("cwd/local/") && !p.starts_with("home/"))
            .cloned()
            .collect();
        if !new.is_empty() {
            failures.push(format!(
                "{kind}: wrote outside the sync root: {new:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        let stray: Vec<String> = std::fs::read_dir(&local)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "ok.txt" && !n.starts_with(".bb"))
            .collect();
        if !stray.is_empty() {
            failures.push(format!(
                "{kind}: hostile entry inside the sync root: {stray:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        match std::fs::read(local.join("ok.txt")) {
            Ok(b) if b == BENIGN => {}
            other => failures.push(format!("{kind}: benign file not synced: {other:?}\n{}", describe(&out))),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// `bb pull --zip` builds archive entry paths from the same server-provided
/// names. A `../..` or absolute entry is a zip-slip for whoever extracts the
/// archive later, so unsafe names must be left out (and the run flagged).
#[test]
fn zip_pull_leaves_hostile_names_out_of_the_archive() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_plain_file(&vault, "../../zipslip-marker.txt", PAYLOAD);
    fx.add_plain_file(&vault, "/abs/zipslip-abs-marker.txt", PAYLOAD);
    fx.add_plain_file(&vault, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    let zip_bytes = std::fs::read(s.cwd.join("out.zip")).unwrap_or_default();
    let has = |needle: &str| zip_bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(
        has("vault/ok.txt"),
        "benign entry missing from the archive\n{}",
        describe(&out)
    );
    assert!(
        !has("zipslip-marker") && !has("zipslip-abs-marker"),
        "hostile entry names ended up in the archive\n{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
}
