//! Independent review probes adopted from rv-cli110.
//! Only storage checkpoints change: security state now belongs to the account.
#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sf::*;
use std::path::Path;

fn replay(f: &mut Fixture, status: u16) {
    f.legacy_invite = true;
    f.custody_status = Some(status);
    f.inventory[0]["encrypted_file_key"] =
        json!(B64.encode(beebeeb_core::encrypt::encrypt_chunk_raw(&folder_key(), item_key().as_bytes()).unwrap()));
    f.rows[1]["owner_wrapped_file_key"] = Value::Null;
    f.rows[1]["id"] = json!(SUB);
    f.inventory[0]["file_id"] = json!(SUB);
    f.requests.clear();
}
fn ok(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
async fn sync(h: &Harness, dir: &Path, dry: bool) -> std::process::Output {
    let mut args = vec!["sync", dir.to_str().unwrap(), "/Shared", "--once"];
    if dry {
        args.push("--dry-run");
    }
    h.run(&args).await
}
fn require_refusal(out: &std::process::Output, h: &Harness, output: &Path) {
    let f = h.state.lock().unwrap();
    let downloads = f.requests.iter().filter(|(_, p, _)| p.ends_with("/download")).count();
    let legacy_reads = f
        .requests
        .iter()
        .filter(|(_, p, _)| p.contains("/shares/invites/"))
        .count();
    let writes = f.requests.iter().filter(|(m, _, _)| m != "GET").count();
    let published = output.exists();
    let replayed = published && std::fs::read(output).unwrap() == f.plaintext;
    assert!(
        !out.status.success(),
        "L7c downgrade accepted: status={:?}, downloads={downloads}, legacy_reads={legacy_reads}, non_GET={writes}, published={published}, replayed_A_under_B={replayed}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!published);
    assert_eq!(downloads, 0);
    assert_eq!(legacy_reads, 0);
    assert_eq!(writes, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_ls_observation_must_protect_later_pull() {
    let h = Harness::new(Fixture::transfer(false)).await;
    let out = h.run(&["ls", "Shared"]).await;
    ok(&out);
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed["files"][0]["name"], "guest.txt");
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")), 1);
    replay(&mut h.state.lock().unwrap(), 404);
    let output = h.home.join("replayed.txt");
    let out = h.run(&["pull", SUB, "-o", output.to_str().unwrap()]).await;
    require_refusal(&out, &h, &output);
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_sync_memory_must_protect_a_second_local_sync_root() {
    let h = Harness::new(Fixture::transfer(false)).await;
    let a = h.home.join("a");
    let b = h.home.join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    ok(&sync(&h, &a, false).await);
    let saved = h.custody_state();
    assert_eq!(saved["custody_seen"], json!([ROOT]));
    replay(&mut h.state.lock().unwrap(), 405);
    let out = sync(&h, &b, false).await;
    require_refusal(&out, &h, &b.join("guest.txt"));
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_dry_run_observation_must_survive_restart() {
    let h = Harness::new(Fixture::transfer(false)).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    ok(&sync(&h, &dir, true).await);
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}/keys")), 1);
    assert!(!dir.join("guest.txt").exists());
    assert!(!dir.join(".bb-sync.json").exists(), "dry-run must not bind sync state");
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    replay(&mut h.state.lock().unwrap(), 501);
    let out = sync(&h, &dir, false).await;
    require_refusal(&out, &h, &dir.join("guest.txt"));
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_never_seen_404_and_405_keep_legacy_readable() {
    for status in [404, 405] {
        let mut f = Fixture::transfer(false);
        replay(&mut f, status);
        let h = Harness::new(f).await;
        let dir = h.home.join("sync");
        std::fs::create_dir(&dir).unwrap();
        ok(&sync(&h, &dir, false).await);
        assert_eq!(
            std::fs::read(dir.join("guest.txt")).unwrap(),
            h.state.lock().unwrap().plaintext
        );
        assert_eq!(h.count("GET", &format!("/api/v1/shares/invites/{BAD}/folder-keys")), 1);
        h.close().await;
    }
}

struct PausedInventory {
    upstream: String,
    reached: tokio::sync::Notify,
}
async fn pause_inventory(
    axum::extract::State(pause): axum::extract::State<std::sync::Arc<PausedInventory>>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if req.uri().path() == format!("/api/v1/shared-folders/{ROOT}/keys") {
        // load_root issues this GET only AFTER authenticating L6 custody.
        pause.reached.notify_one();
        std::future::pending::<()>().await;
    }
    let response = reqwest::Client::new()
        .request(req.method().clone(), format!("{}{}", pause.upstream, req.uri()))
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.bytes().await.unwrap()).into_response()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_interrupted_scan_must_retain_opened_custody() {
    let mut h = Harness::new(Fixture::transfer(false)).await;
    let upstream = h.api.clone();
    let pause = std::sync::Arc::new(PausedInventory {
        upstream: upstream.clone(),
        reached: tokio::sync::Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    h.api = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new().fallback(pause_inventory).with_state(pause.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_bb"))
        .args([
            "sync",
            dir.to_str().unwrap(),
            "/Shared",
            "--once",
            "--api",
            &h.api,
            "--json",
        ])
        .env("HOME", &h.home)
        .env("XDG_CONFIG_HOME", h.home.join(".config"))
        .env("BB_REFUSE_NON_LOOPBACK_API", "1")
        .env("BB_NO_UPDATE", "1")
        .env("BB_TRANSPORT", "api")
        .env_remove("APP_URL")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let reached = tokio::time::timeout(std::time::Duration::from_secs(10), pause.reached.notified())
        .await
        .is_ok();
    child.start_kill().unwrap();
    let stopped = child.wait().await.unwrap();
    server.abort();
    let _ = server.await;
    assert!(
        reached,
        "custody was not opened; instrument did not reach its checkpoint"
    );
    assert!(!stopped.success());
    assert_eq!(h.count("GET", &format!("/api/v1/shared-folders/{ROOT}")), 1);
    assert_eq!(
        h.custody_state()["custody_seen"],
        json!([ROOT]),
        "L6 observation must be durable before inventory I/O"
    );
    h.api = upstream;
    replay(&mut h.state.lock().unwrap(), 404);
    let out = sync(&h, &dir, false).await;
    require_refusal(&out, &h, &dir.join("guest.txt"));
    h.close().await;
}

impl Harness {
    async fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_bb"));
        cmd.args(args)
            .args(["--api", &self.api, "--json"])
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .env("BB_NO_UPDATE", "1")
            .env("BB_TRANSPORT", "api")
            .env_remove("APP_URL");
        cmd.output().await.unwrap()
    }
}
