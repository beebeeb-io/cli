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

/// SF-0002 review P2: a name re-sealed under the normal convention does not
/// make the row writable while its CURRENT version's content is still sealed
/// under the uppercase id. Rename is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_is_refused_when_only_the_content_is_uppercase() {
    let h = Harness::new(Fixture::upper_repair_content_only()).await;
    let out = h
        .run(&["mv", "Shared/uppercase-repair.pdf", "Shared/renamed.pdf"])
        .await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "rename of a content-repair row succeeded: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("This file was saved by an older version of the iOS app"),
        "stderr={stderr}"
    );
    assert_eq!(h.count("PATCH", &format!("/api/v1/files/{UPPER_FILE}")), 0);
    h.close().await;
}

/// SF-0002 review P2: `push --replace` of a content-repair row is refused: no
/// upload session is started and no version is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_is_refused_when_only_the_content_is_uppercase() {
    let h = Harness::new(Fixture::upper_repair_content_only()).await;
    let local = h.home.join("uppercase-repair.pdf");
    std::fs::write(&local, b"replacement bytes").unwrap();
    let out = h
        .run(&["push", local.to_str().unwrap(), "--folder", "Shared", "--replace"])
        .await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "replace of a content-repair row succeeded: stdout={} stderr={stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("This file was saved by an older version of the iOS app"),
        "stderr={stderr}"
    );
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0);
    h.close().await;
}

/// SF-0002 review P1: a shared reader's row whose NAME opens under the folder
/// key but whose CONTENT is sealed under the owner's string convention must
/// not open. The explicit (folder) key is the only key such a download may
/// try; the owner's derived and repair keys are not its to use.
fn explicit_key_row_with_owner_content() -> Fixture {
    let mut f = Fixture::transfer(false);
    let owner_key = beebeeb_core::kdf::derive_file_key(&master(), FILE.as_bytes());
    f.frames = vec![beebeeb_core::encrypt::encrypt_chunk_raw(&owner_key, &f.plaintext).unwrap()];
    f.stride = f.plaintext.len() as u64;
    f.rows[1]["chunk_count"] = serde_json::json!(1);
    f.rows[1]["size_bytes"] = serde_json::json!(f.plaintext.len());
    f
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_key_download_never_falls_back_to_the_owner_keys() {
    let h = Harness::new(explicit_key_row_with_owner_content()).await;
    let out_path = h.home.join("must-not-exist.bin");
    let out = h
        .run(&["pull", "Shared/guest.txt", "-o", out_path.to_str().unwrap()])
        .await;
    assert!(
        !out.status.success(),
        "a shared-folder key opened content sealed under the owner's key: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out_path.exists(), "the download wrote a file it must not have opened");
    h.close().await;
}

/// What `bb` says when the read-only check could not read the version
/// (`owned_keys::WRITE_CHECK_UNKNOWN`).
const COULD_NOT_CHECK: &str = "Couldn't check this file before changing it. Try again.";

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// SF-0002 round 2 (C1): `push --replace` asks the read-only check first. A
/// probe the server answers with a 5xx is not proof of anything: the replace is
/// refused with the retryable message and no upload session starts. With the
/// version readable, the same replace goes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_refuses_when_the_check_cannot_read_the_version() {
    let h = Harness::new(Fixture::owner_file()).await;
    let local = h.home.join("owned.txt");
    std::fs::write(&local, b"replacement bytes").unwrap();
    h.state.lock().unwrap().download_status = Some(503);
    let out = h
        .run(&["push", local.to_str().unwrap(), "--folder", "Shared", "--replace"])
        .await;
    assert!(!out.status.success(), "a replace went ahead without a check: {}", stderr(&out));
    assert!(stderr(&out).contains(COULD_NOT_CHECK), "stderr={}", stderr(&out));
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0, "an upload session was started");
    assert!(h.count("GET", &format!("/api/v1/files/{FILE}/download")) >= 1, "the check never asked");

    h.state.lock().unwrap().download_status = None;
    let out = h
        .run(&["push", local.to_str().unwrap(), "--folder", "Shared", "--replace"])
        .await;
    assert!(out.status.success(), "a readable ordinary row must stay replaceable: {}", stderr(&out));
    {
        let f = h.state.lock().unwrap();
        assert_eq!(f.upload_request["file_id"], FILE, "the replace must version the same id");
        assert_eq!(f.upload_request["base_version_number"], 1);
    }
    h.close().await;
}

/// SF-0002 round 2 (C1): `bb mv` asks the same check before a rename. A 5xx
/// probe refuses it and sends no PATCH; a readable version lets it through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_refuses_when_the_check_cannot_read_the_version() {
    let h = Harness::new(Fixture::owner_file()).await;
    h.state.lock().unwrap().download_status = Some(503);
    let out = h.run(&["mv", "Shared/owned.txt", "Shared/renamed.txt"]).await;
    assert!(!out.status.success(), "a rename went ahead without a check: {}", stderr(&out));
    assert!(stderr(&out).contains(COULD_NOT_CHECK), "stderr={}", stderr(&out));
    assert_eq!(h.count("PATCH", &format!("/api/v1/files/{FILE}")), 0, "a rename was sent");

    h.state.lock().unwrap().download_status = None;
    let out = h.run(&["mv", "Shared/owned.txt", "Shared/renamed.txt"]).await;
    assert!(out.status.success(), "a readable ordinary row must stay renamable: {}", stderr(&out));
    assert_eq!(h.count("PATCH", &format!("/api/v1/files/{FILE}")), 1);
    h.close().await;
}

/// Every GET of a version's content the fixture server answered.
fn downloads(h: &Harness) -> usize {
    h.state
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(m, p, _)| m == "GET" && p.ends_with("/download"))
        .count()
}

/// SF-0002 round 2 (C4): renaming a folder with `bb mv` asks nothing about
/// content (a folder has no version): no download is made and the rename goes
/// through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_rename_downloads_nothing() {
    let h = Harness::new(Fixture::basic()).await;
    let out = h.run(&["mv", "Shared", "Renamed"]).await;
    assert!(out.status.success(), "a folder rename must go through: {}", stderr(&out));
    assert_eq!(h.count("PATCH", &format!("/api/v1/files/{ROOT}")), 1);
    assert_eq!(downloads(&h), 0, "a folder rename probed content");
    h.close().await;
}

/// SF-0002 round 2 (C5): an owner wrap of the uppercase-id key is the same
/// read-only row for `bb mv` and `push --replace` (the resolver path) as it is
/// for WebDAV and the mount: refused by its NAME, before any content probe.
/// The probe is made to fail, so a verdict that waited for the content would
/// say "couldn't check" instead (WebDAV's half is
/// `webdav::sf_0002_write_check_tests::an_owner_wrap_of_the_uppercase_key_is_read_only`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_owner_wrap_of_the_uppercase_key_is_read_only() {
    let h = Harness::new(Fixture::upper_repair_owner_wrap()).await;
    h.state.lock().unwrap().download_status = Some(503);
    let out = h
        .run(&["mv", "Shared/uppercase-repair.pdf", "Shared/renamed.pdf"])
        .await;
    assert!(!out.status.success(), "a read-only owner-wrap row was renamed");
    assert!(
        stderr(&out).contains("This file was saved by an older version of the iOS app"),
        "stderr={}",
        stderr(&out)
    );
    assert_eq!(h.count("PATCH", &format!("/api/v1/files/{UPPER_FILE}")), 0);

    let local = h.home.join("uppercase-repair.pdf");
    std::fs::write(&local, b"replacement bytes").unwrap();
    let out = h
        .run(&["push", local.to_str().unwrap(), "--folder", "Shared", "--replace"])
        .await;
    assert!(!out.status.success(), "a read-only owner-wrap row was replaced");
    assert!(
        stderr(&out).contains("This file was saved by an older version of the iOS app"),
        "stderr={}",
        stderr(&out)
    );
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0);
    assert_eq!(downloads(&h), 0, "the name decides; no content probe is needed");
    h.close().await;
}
