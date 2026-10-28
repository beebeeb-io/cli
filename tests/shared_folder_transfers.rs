#[path = "support/shared_folder.rs"]
mod sf;
use serde_json::json;
use sf::*;
use sha2::{Digest, Sha256};
fn hash(v: &[u8]) -> Vec<u8> {
    Sha256::digest(v).to_vec()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_pull_random_key() {
    let h = Harness::new(Fixture::transfer(false)).await;
    let out = h.home.join("out");
    let r = h.run(&["pull", FILE, "-o", out.to_str().unwrap()]).await;
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let download_hash = hash(&std::fs::read(out).unwrap());
    let guest_hash = hash(&h.state.lock().unwrap().plaintext);
    assert_eq!(download_hash, guest_hash);
    h.close().await;
    let mut f = Fixture::transfer(false);
    f.add_request();
    let h = Harness::new(f).await;
    let out = h.home.join("request");
    let r = h.run(&["pull", SUB, "-o", out.to_str().unwrap()]).await;
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(
        hash(&std::fs::read(out).unwrap()),
        hash(&h.state.lock().unwrap().plaintext)
    );
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_pull_random_key() {
    let mut f = Fixture::transfer(false);
    f.direct = true;
    let h = Harness::new(f).await;
    let out = h.home.join("out");
    let r = h.run(&["pull", FILE, "-o", out.to_str().unwrap()]).await;
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(
        hash(&std::fs::read(out).unwrap()),
        hash(&h.state.lock().unwrap().plaintext)
    );
    let direct_gets = h.count("GET", "/fixture/");
    assert!(direct_gets > 0);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_zip_and_safe_names() {
    let mut f = Fixture::transfer(false);
    let mut bad = row(BAD, Some(ROOT), "../escape", false, true);
    bad["name_encrypted"] = json!(name(&item_key(), "../escape"));
    f.rows.push(bad);
    f.inventory
        .push(json!({"file_id":BAD,"encrypted_file_key":wrap(BAD,false)}));
    let h = Harness::new(f).await;
    let dir = h.home.join("folder");
    let r = h.run(&["pull", ROOT, "-o", dir.to_str().unwrap()]).await;
    assert_eq!(r.status.code(), Some(3));
    assert!(dir.join("guest.txt").exists());
    assert!(!h.home.join("escape").exists());
    let archive = h.home.join("out.zip");
    let r = h
        .run(&["pull", "Shared", "--zip", "-o", archive.to_str().unwrap()])
        .await;
    assert_eq!(r.status.code(), Some(3));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).unwrap()).unwrap();
    let mut bytes = vec![];
    std::io::Read::read_to_end(&mut zip.by_name("Shared/guest.txt").unwrap(), &mut bytes).unwrap();
    assert_eq!(hash(&bytes), hash(&h.state.lock().unwrap().plaintext));
    assert_eq!(zip.len(), 1);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_or_bad_tag_no_publish() {
    for truncate in [false, true] {
        let mut f = Fixture::transfer(true);
        if truncate {
            f.frames.pop();
            f.rows[1]["chunk_count"] = json!(f.frames.len());
        } else {
            f.frames[1][15] ^= 1;
        }
        let h = Harness::new(f).await;
        let failed_output = h.home.join("out");
        let r = h.run(&["pull", FILE, "-o", failed_output.to_str().unwrap()]).await;
        assert!(!r.status.success());
        assert!(!failed_output.exists());
        assert_eq!(
            std::fs::read_dir(&h.home)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
                .count(),
            0
        );
        h.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_pin_and_changed_object() {
    let mut f = Fixture::transfer(true);
    f.direct = true;
    let h = Harness::new(f).await;
    let out = h.home.join("out");
    let r = h.run(&["pull", FILE, "-o", out.to_str().unwrap()]).await;
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(h.count("GET", "/fixture/") > 0);
    h.close().await;
    let mut f = Fixture::transfer(false);
    f.rows[1]["name_encrypted"] = json!(
        beebeeb_core::encrypt::encrypt_name_for(
            &item_key(),
            "guest.txt",
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::V2
        )
        .unwrap()
    );
    let h = Harness::new(f).await;
    let out = h.home.join("out");
    assert!(
        !h.run(&["pull", FILE, "-o", out.to_str().unwrap()])
            .await
            .status
            .success()
    );
    assert!(!out.exists());
    h.close().await;
    let mut f = Fixture::transfer(true);
    f.direct = true;
    f.changed = true;
    let h = Harness::new(f).await;
    let out = h.home.join("out");
    assert!(
        !h.run(&["pull", FILE, "-o", out.to_str().unwrap()])
            .await
            .status
            .success()
    );
    assert!(!out.exists());
    assert!(h.count("POST", "/api/v1/files/") <= 2);
    h.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replace_keeps_key_and_history_id() {
    let mut f = Fixture::transfer(true);
    f.rows[1]["parent_id"] = json!(null);
    f.rows[1]["owner_wrapped_file_key"] = json!(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        beebeeb_core::owned_keys::wrap_key_to_owner(&master(), FILE, &item_key()).unwrap()
    ));
    let h = Harness::new(f).await;
    let path = h.home.join("guest.txt");
    std::fs::write(&path, b"replacement").unwrap();
    let r = h.run(&["push", path.to_str().unwrap(), "--replace"]).await;
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    {
        let state = h.state.lock().unwrap();
        let init: serde_json::Value = serde_json::from_slice(
            &state
                .requests
                .iter()
                .find(|(m, p, _)| m == "POST" && p == "/api/v1/uploads/init")
                .unwrap()
                .2,
        )
        .unwrap();
        let replacement_id = init["file_id"].as_str().unwrap();
        assert_eq!(replacement_id, FILE);
        assert_eq!(init["base_version_number"], 1);
        let wire = &state
            .requests
            .iter()
            .find(|(m, p, _)| m == "PUT" && p.contains("/chunks/"))
            .unwrap()
            .2;
        let mut dec = beebeeb_core::chunk_stream::ChunkDecryptor::for_push_with_file_key(
            item_key(),
            FILE,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::V2,
        );
        assert_eq!(dec.push_frame(wire).unwrap().data, b"replacement");
        dec.finish().unwrap();
    }
    h.close().await;
}

impl Harness {
    pub async fn run(&self, args: &[&str]) -> std::process::Output {
        let home = self.home.clone();
        let api = self.api.clone();
        let direct = self.state.lock().unwrap().direct;
        let args: Vec<_> = args.iter().map(|s| s.to_string()).collect();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_bb"))
                .args(args)
                .args(["--api", &api, "--json"])
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("BB_REFUSE_NON_LOOPBACK_API", "1")
                .env("BB_NO_UPDATE", "1")
                .env("BB_TRANSPORT", if direct { "auto" } else { "api" })
                .env_remove("APP_URL")
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    }
}
