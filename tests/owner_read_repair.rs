//! SF-0002: a file the old iOS File Provider sealed under the uppercase id
//! opens here read-only. Names and content resolve through the owner-key
//! repair conventions; renames, version replaces and editor writes refuse.
#[path = "support/shared_folder.rs"]
mod sf;
use serde_json::Value;
use sf::*;

fn state(dir: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(".bb-sync.json")).unwrap()).unwrap()
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
    pub async fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args).await;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn plaintext(h: &Harness) -> Vec<u8> {
    h.state.lock().unwrap().plaintext.clone()
}

/// `bb ls` shows the repaired name and `bb pull` writes the repaired bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listing_and_pull_show_the_uppercase_row() {
    let h = Harness::new(Fixture::upper_repair()).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert_eq!(v["files"][0]["name"], "uppercase-repair.pdf");

    let out_path = h.home.join("pulled.bin");
    let out = h
        .run(&["pull", "Shared/uppercase-repair.pdf", "-o", out_path.to_str().unwrap()])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&out_path).unwrap(), plaintext(&h));
    h.close().await;
}

/// A rename rewrites the name envelope, so it is refused; moves stay possible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_is_refused_with_the_plain_message() {
    let h = Harness::new(Fixture::upper_repair()).await;
    let out = h
        .run(&["mv", "Shared/uppercase-repair.pdf", "Shared/renamed.pdf"])
        .await;
    assert!(
        !out.status.success(),
        "rename of a read-only row succeeded: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let reqs: Vec<_> = h
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .map(|(m, p, _)| format!("{m} {p}"))
        .collect();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("This file was saved by an older version of the iOS app"),
        "stderr={stderr} requests={reqs:?}"
    );
    assert_eq!(h.count("PATCH", &format!("/api/v1/files/{UPPER_FILE}")), 0);
    h.close().await;
}

/// `bb sync` may not version-replace the row: it trashes it and uploads a new
/// file under a fresh id, exactly like any other edit it cannot write back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_edit_uploads_a_new_file_instead_of_a_version() {
    let h = Harness::new(Fixture::upper_repair()).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(dir.join("uppercase-repair.pdf")).unwrap(), plaintext(&h));

    let edited = b"edited locally, must become a copy";
    std::fs::write(dir.join("uppercase-repair.pdf"), edited).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let f = h.state.lock().unwrap();
    let upload = &f.upload_request;
    assert_ne!(
        upload["file_id"].as_str(),
        Some(UPPER_FILE),
        "upload reused the read-only id"
    );
    assert_eq!(upload["base_version_number"], Value::Null, "uploaded as a version");
    let trashed = f
        .requests
        .iter()
        .any(|(m, p, _)| m == "DELETE" && p.contains(UPPER_FILE));
    assert!(trashed, "the old row was neither trashed nor replaced");
}

/// Ordinary owner rows keep working: a normal transfer row still lists and
/// pulls, and the repair fallback never fires for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_rows_still_resolve_the_old_way() {
    let h = Harness::new(Fixture::transfer(true)).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert_eq!(v["files"][0]["name"], "guest.txt");
    let out_path = h.home.join("guest.bin");
    let out = h
        .run(&["pull", "Shared/guest.txt", "-o", out_path.to_str().unwrap()])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&out_path).unwrap(), plaintext(&h));
    h.close().await;
}

/// A repaired row converges: after the first sync saved the edit as a copy,
/// a second run of the same tree uploads nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repaired_row_converges_after_the_copy() {
    let h = Harness::new(Fixture::upper_repair()).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::write(dir.join("uppercase-repair.pdf"), b"edited locally, must become a copy").unwrap();
    for _ in 0..2 {
        let out = h
            .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
            .await;
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 1);
    assert_eq!(state(&dir)["account_id"], ACCOUNT);
}
