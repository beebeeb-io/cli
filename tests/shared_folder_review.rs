#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde_json::{Value, json};
use sf::*;
use std::path::Path;

fn manifest(d: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(d.join(".bb-sync.json")).unwrap()).unwrap()
}
async fn sync(h: &Harness, d: &Path) -> std::process::Output {
    h.run(&["sync", d.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await
}
fn ok(r: &std::process::Output) {
    assert!(
        r.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
}
async fn initial(h: &Harness) -> std::path::PathBuf {
    let d = h.home.join("sync");
    std::fs::create_dir(&d).unwrap();
    ok(&sync(h, &d).await);
    d
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_identity_conflicts_before_comparing_versions() {
    for version in [1, 2] {
        let mut f = Fixture::transfer(true);
        f.review_complete_requested_id = true;
        f.rows[1]["version_number"] = json!(2);
        let h = Harness::new(f).await;
        let d = initial(&h).await;
        let edited = b"offline edits of the ORIGINAL file";
        std::fs::write(d.join("guest.txt"), edited).unwrap();
        {
            let mut f = h.state.lock().unwrap();
            let mut next = f.rows[1].clone();
            next["id"] = json!(SUB);
            next["version_number"] = json!(version);
            next["updated_at"] = json!("2030-01-01T00:00:00Z");
            next["owner_wrapped_file_key"] =
                json!(B64.encode(beebeeb_core::owned_keys::wrap_key_to_owner(&master(), SUB, &item_key()).unwrap()));
            f.rows[1]["is_trashed"] = json!(true);
            f.rows.push(next);
        }
        let result = sync(&h, &d).await;
        assert_eq!(
            h.count("POST", "/api/v1/uploads/init"),
            0,
            "different identities must conflict, even at version {version}"
        );
        assert_eq!(result.status.code(), Some(3));
        let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(summary["conflicts"], 1);
        assert_eq!(summary["uploaded"], 0);
        assert_eq!(std::fs::read(d.join("guest.txt")).unwrap(), edited);
        assert_eq!(manifest(&d)["files"]["guest.txt"]["remote_id"], FILE);
        assert_eq!(
            h.state.lock().unwrap().rows.iter().find(|r| r["id"] == SUB).unwrap()["version_number"],
            version
        );
        h.close().await;
    }
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
