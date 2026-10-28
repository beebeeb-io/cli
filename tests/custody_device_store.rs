//! L7d: the custody memory belongs to the DEVICE. One monotonic set of folder ids per config dir, keyed by
//! nothing the server reports. No sign-out, sign-in, account switch, 401 or `/me` answer removes an entry.
#[path = "support/shared_folder.rs"]
mod sf;
use axum::extract::State;
use axum::response::IntoResponse;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sf::*;
use std::sync::Arc;

fn replay(f: &mut Fixture) {
    f.legacy_invite = true;
    f.custody_status = Some(404);
    f.inventory[0]["encrypted_file_key"] =
        json!(B64.encode(beebeeb_core::encrypt::encrypt_chunk_raw(&folder_key(), item_key().as_bytes()).unwrap()));
    f.rows[1]["owner_wrapped_file_key"] = Value::Null;
    f.requests.clear();
}
fn ok(out: std::process::Output) {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}
fn remember(h: &Harness, ids: &[&str]) {
    std::fs::create_dir_all(h.config_dir()).unwrap();
    std::fs::write(h.custody_path(), json!({ "custody_seen": ids }).to_string()).unwrap();
}
fn offline(h: &mut Harness) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    h.api = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_account_id_neither_erases_nor_bypasses_device_memory() {
    let h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    let before = std::fs::read(h.custody_path()).unwrap();
    {
        // `/me` now names another account and the server withdraws custody, replaying an old unbound wrap.
        let mut f = h.state.lock().unwrap();
        f.account_id = Some(BAD.into());
        replay(&mut f);
    }
    let output = h.home.join("other-account.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    refusal(&h, &out, &output);
    assert_eq!(h.count("GET", &format!("/api/v1/shares/invites/{BAD}/folder-keys")), 0);
    assert_eq!(
        std::fs::read(h.custody_path()).unwrap(),
        before,
        "the file must be intact"
    );
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logout_removes_the_session_and_keeps_the_device_memory() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    let before = std::fs::read(h.custody_path()).unwrap();
    assert!(h.config_dir().join("config.json").exists());
    offline(&mut h);
    ok(h.run(&["logout"]).await);
    assert!(
        !h.config_dir().join("config.json").exists(),
        "logout must remove the master key"
    );
    assert_eq!(std::fs::read(h.custody_path()).unwrap(), before);
    h.close().await;
}

/// A store that cannot be read refuses reads (below), but it must never be able to keep the master key on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_device_store_does_not_stop_logout() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    std::fs::remove_file(h.custody_path()).unwrap();
    std::fs::create_dir(h.custody_path()).unwrap();
    offline(&mut h);
    ok(h.run(&["logout"]).await);
    assert!(!h.config_dir().join("config.json").exists());
    assert!(h.custody_path().is_dir(), "logout leaves the store alone");
    h.close().await;
}

#[test]
fn sign_in_and_out_never_touch_the_device_store_and_it_has_no_delete_api() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in ["commands/login.rs", "commands/logout.rs", "config.rs"] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        assert!(
            !text.contains("custody"),
            "{file} must not touch the device custody store"
        );
    }
    let store = std::fs::read_to_string(src.join("custody.rs")).unwrap();
    let mut api: Vec<_> = store
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("pub async fn ").or_else(|| l.strip_prefix("pub fn "))
        })
        .filter_map(|l| l.split('(').next())
        .collect();
    api.sort_unstable();
    assert_eq!(
        api,
        ["open", "remember", "seen"],
        "an entry may only ever be added: a new public function in custody.rs needs a ruling"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_device_store_refuses_before_legacy_or_content() {
    let h = Harness::new(Fixture::transfer(false)).await;
    ok(h.run(&["ls", "Shared"]).await);
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    std::fs::write(h.custody_path(), b"broken-json").unwrap();
    replay(&mut h.state.lock().unwrap());
    let output = h.home.join("corrupt.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    refusal(&h, &out, &output);
    assert_eq!(h.count("GET", &format!("/api/v1/shares/invites/{BAD}/folder-keys")), 0);
    h.close().await;
}

/// Reverse proxy to the fixture that runs `after` once the custody request for ROOT has been answered: what
/// another process (or a broken disk) can do to the store between this process's check and its next step.
struct Tap {
    upstream: String,
    after: Box<dyn Fn() + Send + Sync>,
}
async fn tap(State(tap): State<Arc<Tap>>, req: axum::extract::Request) -> axum::response::Response {
    let (method, uri) = (req.method().clone(), req.uri().clone());
    let body = axum::body::to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
    let response = reqwest::Client::new()
        .request(method, format!("{}{}", tap.upstream, uri))
        .body(body)
        .send()
        .await
        .unwrap();
    if uri.path() == format!("/api/v1/shared-folders/{ROOT}") {
        (tap.after)();
    }
    let status = response.status();
    (status, response.bytes().await.unwrap()).into_response()
}
/// Point `h` at a tap in front of its fixture. The caller aborts the returned handle when done.
async fn tapped(h: &mut Harness, after: impl Fn() + Send + Sync + 'static) -> tokio::task::JoinHandle<()> {
    let tap_state = Arc::new(Tap {
        upstream: h.api.clone(),
        after: Box::new(after),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    h.api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().fallback(tap).with_state(tap_state))
            .await
            .unwrap();
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_observation_write_refuses_before_inventory_or_content() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    let record = h.custody_path();
    // The resolver already checked the empty store and is about to open L6; a directory in the record's place
    // makes the write fail.
    let server = tapped(&mut h, move || std::fs::create_dir(&record).unwrap()).await;
    let output = h.home.join("uncommitted.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    server.abort();
    let _ = server.await;
    assert_eq!(
        h.count("GET", &format!("/api/v1/shared-folders/{ROOT}")),
        1,
        "instrument must reach L6"
    );
    refusal(&h, &out, &output);
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")), 0);
    h.close().await;
}

/// Another process records custody for ROOT after this one asked the server and was told "no custody". The
/// legacy root must not be installed on that stale answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custody_recorded_after_the_first_check_still_refuses_a_legacy_root() {
    let mut f = Fixture::transfer(false);
    replay(&mut f);
    let mut h = Harness::new(f).await;
    let record = h.custody_path();
    let server = tapped(&mut h, move || {
        std::fs::write(&record, json!({ "custody_seen": [ROOT] }).to_string()).unwrap()
    })
    .await;
    let output = h.home.join("raced.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    server.abort();
    let _ = server.await;
    assert_eq!(
        h.count("GET", &format!("/api/v1/shared-folders/{ROOT}")),
        1,
        "instrument must reach the custody request"
    );
    refusal(&h, &out, &output);
    h.close().await;
}

/// ROOT has custody; SUB inside it only has an approved legacy invite; FILE sits in SUB.
fn nested() -> Fixture {
    let mut f = Fixture::transfer(false);
    f.rows[1]["parent_id"] = json!(SUB);
    f.rows[1]["owner_wrapped_file_key"] = Value::Null;
    f.rows.push(row(SUB, Some(ROOT), "sub", true, true));
    f.nested_legacy = true;
    f.sub_inventory = vec![json!({
        "file_id": FILE,
        "encrypted_file_key": B64.encode(beebeeb_core::encrypt::encrypt_chunk_raw(&folder_key(), item_key().as_bytes()).unwrap()),
    })];
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_root_is_refused_when_any_ancestor_has_seen_custody() {
    let h = Harness::new(nested()).await;
    remember(&h, &[ROOT]);
    let output = h.home.join("nested.txt");
    let out = h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await;
    refusal(&h, &out, &output);
    assert_eq!(
        h.count("GET", &format!("/api/v1/shared-folders/{SUB}")),
        1,
        "instrument must reach SUB's custody request"
    );
    h.close().await;
}

/// The control for the test above: the same tree reads through SUB's legacy invite when nothing was seen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_root_inside_an_unseen_folder_still_reads() {
    let h = Harness::new(nested()).await;
    let output = h.home.join("nested.txt");
    ok(h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await);
    assert_eq!(std::fs::read(&output).unwrap(), h.state.lock().unwrap().plaintext);
    assert_eq!(
        h.count("GET", &format!("/api/v1/shares/invites/{SUB_INVITE}/folder-keys")),
        1
    );
    h.close().await;
}

fn refusal(h: &Harness, out: &std::process::Output, output: &std::path::Path) {
    assert!(!out.status.success(), "accepted unusable security state");
    assert!(String::from_utf8_lossy(&out.stderr).contains("This folder's keys are unavailable"));
    assert!(!output.exists());
    let f = h.state.lock().unwrap();
    assert_eq!(f.requests.iter().filter(|(m, _, _)| m != "GET").count(), 0);
    assert_eq!(
        f.requests.iter().filter(|(_, p, _)| p.ends_with("/download")).count(),
        0
    );
}
impl Harness {
    async fn run(&self, args: &[&str]) -> std::process::Output {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(args)
            .args(["--api", &self.api, "--json"])
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .env("BB_NO_UPDATE", "1")
            .env("BB_TRANSPORT", "api")
            .env_remove("APP_URL")
            .output()
            .await
            .unwrap()
    }
}
