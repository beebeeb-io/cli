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
                json!(B64.encode(beebeeb_core::owned_keys::wrap_owner_file_key(&master(), SUB, &item_key()).unwrap()));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_replacement_discards_resume_for_a_different_identity() {
    for v2 in [true, false] {
        let mut f = Fixture::transfer(v2);
        f.review_complete_requested_id = true;
        f.rows.push(row(SUB, None, "unrelated-existing.txt", false, false));
        let h = Harness::new(f).await;
        let d = initial(&h).await;
        let path = d.join("guest.txt");
        let edited = b"new local replacement content";
        std::fs::write(&path, edited).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let nanos = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // The independent review reproduction: a previous push of this same
        // unchanged local path targeted the unrelated existing SUB file.
        // Keep its session distinct from the fresh init returned by the mock.
        let stale_session = "55555555-5555-4555-8555-555555555555";
        let db = json!({"uploads":{path.canonicalize().unwrap().to_str().unwrap():{"file_id":SUB,"upload_session_id":stale_session,"chunk_format":if v2 {2} else {1},"direct":false,"size":meta.len(),"mtime_ns":nanos}}});
        std::fs::write(h.home.join(".config/beebeeb/pending-uploads.json"), db.to_string()).unwrap();
        let result = sync(&h, &d).await;
        ok(&result);
        {
            let f = h.state.lock().unwrap();
            assert_eq!(
                f.upload_request["file_id"], FILE,
                "resume must never redirect an explicit replacement"
            );
            assert_eq!(f.upload_request["base_version_number"], 1);
            assert_eq!(manifest(&d)["files"]["guest.txt"]["remote_id"], FILE);
            let other_key = beebeeb_core::kdf::derive_file_key(&master(), SUB.as_bytes());
            let pin = if v2 {
                beebeeb_core::chunk_stream::ExpectedChunkFormat::V2
            } else {
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded
            };
            assert!(opened_with_pin(&other_key, FILE, &f.uploaded_chunks, pin).is_err());
            assert_eq!(
                opened_with_pin(&item_key(), FILE, &f.uploaded_chunks, pin).unwrap(),
                edited
            );
            assert_eq!(f.rows[1]["version_number"], 2, "intended FILE was replaced");
            assert_eq!(f.rows[2]["version_number"], 1, "unrelated SUB is unchanged");
            assert!(
                !f.requests
                    .iter()
                    .any(|(_, path, _)| path.starts_with(&format!("/api/v1/uploads/{stale_session}/")))
            );
        }
        assert_eq!(h.count("POST", &format!("/api/v1/files/{SUB}/upload/abandon")), 0);
        h.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_index_repair_is_reported_without_blocking_content() {
    let mut f = Fixture::transfer(true);
    f.review_partial_repair_only = true;
    let h = Harness::new(f).await;
    let d = h.home.join("sync");
    std::fs::create_dir(&d).unwrap();
    let result = sync(&h, &d).await;
    ok(&result);
    assert_eq!(
        std::fs::read(d.join("guest.txt")).unwrap(),
        h.state.lock().unwrap().plaintext
    );
    assert_eq!(manifest(&d)["dirty_shared_roots"], json!([ROOT]));
    assert_eq!(
        h.count("PUT", &format!("/api/v1/shared-folders/{ROOT}/search-index")),
        0
    );
    let notice = String::from_utf8_lossy(&result.stderr);
    assert!(
        notice.contains("index") && notice.contains("partial") && notice.contains("deferred"),
        "missing partial/deferred notice: {notice}"
    );
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["ok"], true);
    assert_eq!(summary["failed"], 0);
    assert_eq!(summary["downloaded"], 1);
    assert_eq!(summary["index_partial"], true);
    assert_eq!(summary["index_deferred"], 1);
    assert!(summary["index_written"].as_u64().is_some());
    h.state.lock().unwrap().review_partial_repair_only = false;
    let result = sync(&h, &d).await;
    ok(&result);
    let summary: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(summary["index_partial"], false);
    assert_eq!(summary["index_deferred"], 0);
    assert_eq!(manifest(&d)["dirty_shared_roots"], json!([]));
    assert_eq!(h.count("POST", "/api/v1/uploads/init"), 0);
    h.close().await;
}

fn opened_with_pin(
    key: &beebeeb_core::kdf::FileKey,
    id: &str,
    frames: &[Vec<u8>],
    pin: beebeeb_core::chunk_stream::ExpectedChunkFormat,
) -> Result<Vec<u8>, String> {
    let mut dec = beebeeb_core::chunk_stream::ChunkDecryptor::for_push_with_file_key(
        beebeeb_core::kdf::FileKey::from_bytes(*key.as_bytes()),
        id,
        pin,
    );
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend(dec.push_frame(frame).map_err(|e| e.to_string())?.data);
    }
    dec.finish().map_err(|e| e.to_string())?;
    Ok(bytes)
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
