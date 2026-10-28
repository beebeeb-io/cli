#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sf::*;
use std::path::{Path, PathBuf};

fn state(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap()
}
fn dir(h: &Harness) -> PathBuf {
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    dir
}
async fn sync(h: &Harness, dir: &Path) -> std::process::Output {
    h.run(&["sync", dir.to_str().unwrap(), "/Shared", "--once"]).await
}
fn ok(out: std::process::Output) {
    assert!(
        out.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn legacy(f: &mut Fixture, status: u16) {
    f.legacy_invite = true;
    f.custody_status = Some(status);
    f.inventory[0]["encrypted_file_key"] =
        json!(B64.encode(beebeeb_core::encrypt::encrypt_chunk_raw(&folder_key(), item_key().as_bytes()).unwrap()));
    f.rows[1]["owner_wrapped_file_key"] = Value::Null;
}
async fn downgrade(status: u16) {
    let h = Harness::new(Fixture::transfer(false)).await;
    let dir = dir(&h);
    ok(sync(&h, &dir).await);
    assert_eq!(
        std::fs::read(dir.join("guest.txt")).unwrap(),
        h.state.lock().unwrap().plaintext
    );
    std::fs::remove_file(dir.join("guest.txt")).unwrap();
    {
        let mut f = h.state.lock().unwrap();
        legacy(&mut f, status);
        // A new child has no cached adoption envelope. Replaying an unbound
        // child must not let a second process open it under a custody root.
        f.rows[1]["id"] = json!(SUB);
        f.inventory[0]["file_id"] = json!(SUB);
        f.requests.clear();
    }
    let before = std::fs::read(dir.join(".bb-sync.json")).unwrap();
    let out = sync(&h, &dir).await;
    assert!(!out.status.success(), "downgrade {status} succeeded");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("This folder's keys are unavailable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.join("guest.txt").exists());
    assert_eq!(std::fs::read(dir.join(".bb-sync.json")).unwrap(), before);
    {
        let f = h.state.lock().unwrap();
        assert_eq!(f.requests.iter().filter(|(m, _, _)| m != "GET").count(), 0);
        assert_eq!(
            f.requests.iter().filter(|(_, p, _)| p.ends_with("/download")).count(),
            0
        );
        assert_eq!(
            f.requests
                .iter()
                .filter(|(_, p, _)| p.contains("/shares/invites/"))
                .count(),
            0
        );
    }
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seen_then_404_refuses_with_zero_mutations_after_restart() {
    downgrade(404).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seen_then_405_refuses_with_zero_mutations_after_restart() {
    downgrade(405).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seen_then_501_refuses_with_zero_mutations_after_restart() {
    downgrade(501).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_seen_501_reads_legacy() {
    let mut f = Fixture::transfer(false);
    legacy(&mut f, 501);
    let h = Harness::new(f).await;
    let dir = dir(&h);
    ok(sync(&h, &dir).await);
    assert_eq!(
        std::fs::read(dir.join("guest.txt")).unwrap(),
        h.state.lock().unwrap().plaintext
    );
    assert_eq!(h.count("GET", &format!("/api/v1/shares/invites/{BAD}/folder-keys")), 1);
    assert_eq!(h.custody_state()["custody_seen"], json!([]));
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custody_memory_is_bound_and_survives_second_sync() {
    let h = Harness::new(Fixture::transfer(false)).await;
    let dir = dir(&h);
    for _ in 0..2 {
        ok(sync(&h, &dir).await);
        assert_eq!(state(&dir)["account_id"], ACCOUNT);
        assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    }
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_failed_sync_retains_custody_without_binding_opaque_keys() {
    let mut f = Fixture::transfer(false);
    f.frames[0][0] ^= 1;
    let h = Harness::new(f).await;
    let dir = dir(&h);
    assert_eq!(sync(&h, &dir).await.status.code(), Some(3));
    let saved = state(&dir);
    assert!(saved["account_id"].is_null());
    assert_eq!(saved["owned_keys"], json!({}));
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    {
        let mut f = h.state.lock().unwrap();
        f.frames[0][0] ^= 1;
        legacy(&mut f, 404);
        f.requests.clear();
    }
    let out = sync(&h, &dir).await;
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("This folder's keys are unavailable"));
    assert!(!dir.join("guest.txt").exists());
    assert_eq!(
        h.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, _, _)| m != "GET")
            .count(),
        0
    );
    assert_eq!(h.custody_state()["custody_seen"], json!([ROOT]));
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_custody_fields_are_inert() {
    let mut f = Fixture::transfer(false);
    legacy(&mut f, 501);
    let h = Harness::new(f).await;
    let dir = dir(&h);
    std::fs::write(
        dir.join(".bb-sync.json"),
        json!({"remote_path":"/Shared", "remote_folder_id":ROOT, "custody_account_id":BAD, "custody_seen":[ROOT]})
            .to_string(),
    )
    .unwrap();
    ok(sync(&h, &dir).await);
    assert_eq!(state(&dir)["account_id"], ACCOUNT);
    assert_eq!(h.custody_state()["custody_seen"], json!([]));
    assert!(dir.join("guest.txt").exists());
    h.close().await;
}
impl Harness {
    pub async fn run(&self, args: &[&str]) -> std::process::Output {
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
}
