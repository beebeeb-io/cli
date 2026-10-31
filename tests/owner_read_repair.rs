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

/// `bb sync` may not version-replace the row, and it may not trash it either:
/// the read-only message promises the original stays ("save a copy"). The
/// edit is not uploaded, the pass reports it (exit 3) and the local edit is
/// left exactly as the user wrote it. (Round 1 trashed the row and uploaded
/// the edit under a fresh id; SF-0002 round 2, C2, keeps the original.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_keeps_a_read_only_row_and_the_local_edit() {
    let h = Harness::new(Fixture::upper_repair()).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(dir.join("uppercase-repair.pdf")).unwrap(), plaintext(&h));

    let edited = b"edited locally, the original must stay";
    std::fs::write(dir.join("uppercase-repair.pdf"), edited).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert_eq!(
        out.status.code(),
        Some(3),
        "a pass that left the edit unsynced must say so: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("This file was saved by an older version of the iOS app"),
        "stderr={}",
        stderr(&out)
    );
    // Round 3 (K6): a notice with guidance, not a failed upload.
    assert!(
        stderr(&out).contains(READ_ONLY_GUIDANCE),
        "the notice must say what to do: {}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("upload failed"),
        "a kept read-only edit is not a failure: {}",
        stderr(&out)
    );
    let summary: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(summary["read_only_kept"], 1);
    assert_eq!(summary["failed"], 0);
    assert_eq!(
        h.count("DELETE", &format!("/api/v1/files/{UPPER_FILE}")),
        0,
        "the original was trashed"
    );
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0, "the edit was uploaded");
    assert_eq!(std::fs::read(dir.join("uppercase-repair.pdf")).unwrap(), edited);
    h.close().await;
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

/// Later passes stay the same: the read-only row is never trashed and the
/// edit is never uploaded, however often sync runs, and the local edit
/// survives every pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_only_row_is_left_alone_on_every_pass() {
    let h = Harness::new(Fixture::upper_repair()).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let edited = b"edited locally, the original must stay";
    std::fs::write(dir.join("uppercase-repair.pdf"), edited).unwrap();
    for _ in 0..2 {
        let out = h
            .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
            .await;
        assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    }
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0);
    assert_eq!(h.count("DELETE", &format!("/api/v1/files/{UPPER_FILE}")), 0);
    assert_eq!(std::fs::read(dir.join("uppercase-repair.pdf")).unwrap(), edited);
    assert_eq!(state(&dir)["account_id"], ACCOUNT);
    h.close().await;
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

/// The guidance sync adds to a kept read-only edit (round 3, K6).
const READ_ONLY_GUIDANCE: &str =
    "Your edit stays on this computer: save it under a new name and sync uploads that as a new file.";

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
    assert!(
        !out.status.success(),
        "a replace went ahead without a check: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains(COULD_NOT_CHECK), "stderr={}", stderr(&out));
    assert_eq!(
        h.count("POST", "/api/v1/uploads/init"),
        0,
        "an upload session was started"
    );
    assert!(
        h.count("GET", &format!("/api/v1/files/{FILE}/download")) >= 1,
        "the check never asked"
    );

    h.state.lock().unwrap().download_status = None;
    let out = h
        .run(&["push", local.to_str().unwrap(), "--folder", "Shared", "--replace"])
        .await;
    assert!(
        out.status.success(),
        "a readable ordinary row must stay replaceable: {}",
        stderr(&out)
    );
    {
        let f = h.state.lock().unwrap();
        assert_eq!(
            f.upload_request["file_id"], FILE,
            "the replace must version the same id"
        );
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
    assert!(
        !out.status.success(),
        "a rename went ahead without a check: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains(COULD_NOT_CHECK), "stderr={}", stderr(&out));
    assert_eq!(
        h.count("PATCH", &format!("/api/v1/files/{FILE}")),
        0,
        "a rename was sent"
    );

    h.state.lock().unwrap().download_status = None;
    let out = h.run(&["mv", "Shared/owned.txt", "Shared/renamed.txt"]).await;
    assert!(
        out.status.success(),
        "a readable ordinary row must stay renamable: {}",
        stderr(&out)
    );
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
    assert!(
        out.status.success(),
        "a folder rename must go through: {}",
        stderr(&out)
    );
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

/// SF-0002 round 2 (C2, data safety): a sync whose write check cannot read
/// the version (a 5xx probe) leaves the remote row untouched (no trash) and
/// uploads nothing this pass; it is not marked read-only either. The next pass
/// with the version readable replaces it IN PLACE (same id, next version).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_skips_a_row_it_cannot_check_and_replaces_it_in_place_next_pass() {
    let h = Harness::new(Fixture::transfer(true)).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", stderr(&out));
    let edited = b"a shared-folder edit that must replace in place";
    std::fs::write(dir.join("guest.txt"), edited).unwrap();

    h.state.lock().unwrap().download_status = Some(503);
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert_eq!(
        out.status.code(),
        Some(3),
        "a skipped upload must not report synced: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains(COULD_NOT_CHECK), "stderr={}", stderr(&out));
    assert_eq!(
        h.count("DELETE", &format!("/api/v1/files/{FILE}")),
        0,
        "an unchecked row was trashed"
    );
    assert_eq!(
        h.count("POST", "/api/v1/uploads/init"),
        0,
        "an unchecked row was re-uploaded"
    );
    assert_eq!(std::fs::read(dir.join("guest.txt")).unwrap(), edited);

    h.state.lock().unwrap().download_status = None;
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", stderr(&out));
    {
        let f = h.state.lock().unwrap();
        assert_eq!(f.upload_request["file_id"], FILE, "the edit must replace the same row");
        assert_eq!(f.upload_request["base_version_number"], 1, "as its next version");
    }
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 1);
    assert_eq!(h.count("DELETE", &format!("/api/v1/files/{FILE}")), 0);
    h.close().await;
}

/// SF-0002 round 2 (C2): an ordinary owner row (name lowercase) whose CURRENT
/// version is still uppercase-sealed is read-only for sync too: it is not
/// trashed and replaced by a fresh id, as an ordinary owner edit would be.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_keeps_a_row_whose_content_is_uppercase() {
    let h = Harness::new(Fixture::upper_repair_content_only()).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", stderr(&out));
    std::fs::write(dir.join("uppercase-repair.pdf"), b"edited locally").unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("This file was saved by an older version of the iOS app"),
        "stderr={}",
        stderr(&out)
    );
    assert_eq!(
        h.count("DELETE", &format!("/api/v1/files/{UPPER_FILE}")),
        0,
        "the original was trashed"
    );
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0);
    h.close().await;
}

/// SF-0002 round 3 (K1): an old-CLI row whose chunk is a JSON blob was
/// permanently Unknown (only raw frames were tried). It is writable again:
/// `push --replace` versions it in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_json_blob_row_is_replaceable() {
    let h = Harness::new(Fixture::owner_file_json_blob()).await;
    let local = h.home.join("legacy.txt");
    std::fs::write(&local, b"replacement bytes").unwrap();
    let out = h
        .run(&["push", local.to_str().unwrap(), "--folder", "Shared", "--replace"])
        .await;
    assert!(
        out.status.success(),
        "a JSON-blob row must be replaceable: {}",
        stderr(&out)
    );
    {
        let f = h.state.lock().unwrap();
        assert_eq!(f.upload_request["file_id"], FILE);
        assert_eq!(f.upload_request["base_version_number"], 1);
    }
    h.close().await;
}

/// SF-0002 round 3 (K1): the same row syncs. The edit is uploaded as a new file
/// under a fresh id and the old row is trashed only after that (L5).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_json_blob_row_syncs_an_edit() {
    let mut fixture = Fixture::owner_file_json_blob();
    fixture.review_complete_requested_id = true; // an honest server echoes the id it was sent
    let h = Harness::new(fixture).await;
    let dir = h.home.join("sync");
    std::fs::create_dir(&dir).unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(std::fs::read(dir.join("legacy.txt")).unwrap(), plaintext(&h));
    std::fs::write(dir.join("legacy.txt"), b"edited locally").unwrap();
    let out = h
        .run(&["sync", dir.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await;
    assert!(
        out.status.success(),
        "a JSON-blob row must sync an edit: {}",
        stderr(&out)
    );
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 1);
    let f = h.state.lock().unwrap();
    assert_ne!(
        f.upload_request["file_id"].as_str(),
        Some(FILE),
        "a new file, as sync does for owner rows"
    );
    let order: Vec<_> = f
        .requests
        .iter()
        .filter_map(|(m, p, _)| {
            if m == "DELETE" && p.contains(FILE) {
                Some("trash")
            } else if m == "POST" && p.ends_with("/complete") {
                Some("complete")
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        order,
        ["complete", "trash"],
        "the old row is trashed after the new one completed"
    );
}
