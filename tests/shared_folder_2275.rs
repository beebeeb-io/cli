//! Task 2275 T4.6: the A-tests for owner items under collusion, CLI side.
//!
//! Spec: docs/superpowers/specs/2026-10-29-2275-owner-items-under-collusion-design.md
//! L12 (the id class decides the key path), L13 (writes), L14 (the index),
//! L15 = A (desktop and CLI never publish an owner key), §7 A-tests.
#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use beebeeb_core::kdf::FileKey;
use serde_json::{Value, json};
use sf::*;
use std::path::{Path, PathBuf};

impl Harness {
    pub async fn run(&self, args: &[&str]) -> std::process::Output {
        self.spawn(args, true).await
    }
    /// No `--json`: the human surface, where L15's owner-only sentence lives.
    pub async fn run_plain(&self, args: &[&str]) -> std::process::Output {
        self.spawn(args, false).await
    }
    async fn spawn(&self, args: &[&str], json: bool) -> std::process::Output {
        let home = self.home.clone();
        let api = self.api.clone();
        let direct = self.state.lock().unwrap().direct;
        let mut argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        if json {
            argv.push("--json".into());
        }
        tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_bb"));
            cmd.args(&argv)
                .args(["--api", &api])
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join(".config"))
                .env("BB_REFUSE_NON_LOOPBACK_API", "1")
                .env("BB_NO_UPDATE", "1")
                .env("BB_TRANSPORT", if direct { "auto" } else { "api" })
                .env_remove("APP_URL");
            cmd.output().unwrap()
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

fn dir(h: &Harness) -> PathBuf {
    let d = h.home.join("sync");
    std::fs::create_dir(&d).unwrap();
    d
}
async fn sync(h: &Harness, d: &Path) -> std::process::Output {
    h.run(&["sync", d.to_str().unwrap(), "/Shared", "--once", "--rehash"])
        .await
}
fn ok(r: std::process::Output) {
    assert!(
        r.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    );
}
fn owner_key_posts(h: &Harness, id: &str) -> usize {
    h.count("POST", &format!("/api/v1/files/{id}/owner-key"))
}
fn shared_folder(fixture: Fixture) -> Fixture {
    let mut f = fixture;
    if !f.rows.iter().any(|r| r["id"] == ROOT) {
        f.rows.insert(0, row(ROOT, None, "Shared", RowFacts { folder: true, ..Default::default() }));
    }
    f
}
fn decrypt_name(key: &FileKey, encrypted: &str) -> String {
    beebeeb_core::encrypt::decrypt_name_metadata_with_key(key, encrypted)
        .unwrap()
        .name
}
async fn pull_bytes(h: &Harness, id: &str) -> Vec<u8> {
    let out = h.home.join(format!("pull-{id}.bin"));
    ok(h.run(&["pull", id, "-o", out.to_str().unwrap()]).await);
    std::fs::read(out).unwrap()
}

/// A1: an O item listed foreign, with a member-made wrap of K' and a name under
/// K'. L12: the id class decides, attribution never - derived only, unavailable,
/// and no owner-key POST.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a1_owner_id_with_member_wrap_reads_derived_only() {
    let mut f = Fixture::default();
    f.rows.push(row(
        O_FILE,
        Some(ROOT),
        "a1.txt",
        RowFacts { foreign: true, ..Default::default() },
    ));
    f.inventory
        .push(json!({"file_id":O_FILE,"encrypted_file_key":wrap(O_FILE,false)}));
    let h = Harness::new(shared_folder(f)).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert_eq!(v["files"][0]["name"], "can't open (added via shared folder)");
    assert_eq!(owner_key_posts(&h, O_FILE), 0);
    h.close().await;
}

/// A2: the same row, but the name really is under the derived key: derived,
/// readable, and never adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a2_owner_id_with_real_name_reads_without_adoption() {
    let mut f = Fixture::default();
    let mut r = row(O_FILE, Some(ROOT), "a2.txt", RowFacts { ..Default::default() });
    r["acted_via_share_id"] = json!(BAD);
    f.rows.push(r);
    f.inventory
        .push(json!({"file_id":O_FILE,"encrypted_file_key":wrap(O_FILE,false)}));
    let h = Harness::new(shared_folder(f)).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert_eq!(v["files"][0]["name"], "a2.txt");
    assert_eq!(owner_key_posts(&h, O_FILE), 0);
    h.close().await;
}

/// A3: an O item carrying an L8 owner envelope is ignored: the derived key wins,
/// so the item still reads, and nothing is adopted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a3_owner_id_l8_envelope_is_ignored() {
    let mut f = Fixture::default();
    let derived = beebeeb_core::kdf::derive_file_key(&master(), O_FILE.as_bytes());
    let mut r = row(O_FILE, None, "a3.txt", RowFacts { ..Default::default() });
    r["owner_wrapped_file_key"] = json!(B64.encode(
        beebeeb_core::encrypt::encrypt_chunk_raw(&derived, item_key().as_bytes()).unwrap()
    ));
    f.rows.push(r);
    let h = Harness::new(shared_folder(f)).await;
    let v = h.json(&["ls"]).await;
    assert_eq!(v["files"][0]["name"], "a3.txt");
    assert_eq!(owner_key_posts(&h, O_FILE), 0);
    h.close().await;
}

/// A5: an F item whose wrap opens under the proven custody key is adopted once,
/// and the stored winner is the resolved key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a5_foreign_wrap_is_adopted_once_with_verified_winner() {
    let h = Harness::new(Fixture::transfer(true)).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    assert_eq!(owner_key_posts(&h, FILE), 1);
    let stored = h.state.lock().unwrap().rows[1]["owner_wrapped_file_key"]
        .as_str()
        .expect("the adopted envelope is stored")
        .to_string();
    let opened = beebeeb_core::owned_keys::unwrap_owner_file_key(&master(), FILE, &B64.decode(stored).unwrap())
        .expect("the winner opens");
    assert_eq!(opened.as_bytes(), item_key().as_bytes());
    h.close().await;
}

/// A9: a request-row rename seals the new name under the string convention; the
/// content stays under C.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a9_request_row_rename_seals_name_under_string_convention() {
    let mut f = Fixture::basic();
    f.plaintext = b"request body".to_vec();
    f.add_request();
    let h = Harness::new(f).await;
    ok(h.run(&["mv", "request.txt", "renamed.txt"]).await);
    let patch = {
        let state = h.state.lock().unwrap();
        state
            .requests
            .iter()
            .find(|(m, p, _)| m == "PATCH" && p.contains(SUB))
            .map(|(_, _, b)| serde_json::from_slice::<Value>(b).unwrap())
            .expect("the rename is a PATCH of the request row")
    };
    let sealed = patch["name_encrypted"].as_str().unwrap();
    let string_key = beebeeb_core::kdf::derive_file_key(&master(), SUB.as_bytes());
    assert_eq!(decrypt_name(&string_key, sealed), "renamed.txt");
    assert_eq!(pull_bytes(&h, SUB).await, b"request body");
    assert_eq!(owner_key_posts(&h, SUB), 0);
    h.close().await;
}

/// A9b: a content edit of a request row is saved as a new O item - never sealed
/// under C, and never published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a9b_request_row_content_edit_becomes_new_item() {
    let mut f = Fixture::basic();
    f.plaintext = b"request body".to_vec();
    f.add_request();
    f.review_complete_requested_id = true;
    let h = Harness::new(f).await;
    let path = h.home.join("request.txt");
    std::fs::write(&path, b"edited body").unwrap();
    ok(h.run(&["push", path.to_str().unwrap(), "--replace"]).await);
    let init = {
        let state = h.state.lock().unwrap();
        state
            .requests
            .iter()
            .find(|(m, p, _)| m == "POST" && p == "/api/v1/uploads/init")
            .map(|(_, _, b)| serde_json::from_slice::<Value>(b).unwrap())
            .expect("the edit uploads")
    };
    let uploaded = init["file_id"].as_str().unwrap();
    assert_ne!(uploaded, SUB, "a content edit is a new item, not the request row");
    assert_eq!(owner_key_posts(&h, SUB), 0);
    h.close().await;
}

/// A9c: an old client renamed a request row under the derived key; the content
/// still opens under C.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a9c_request_row_renamed_under_derived_keeps_content_under_c() {
    let mut f = Fixture::basic();
    f.plaintext = b"request body".to_vec();
    f.add_request();
    let string_key = beebeeb_core::kdf::derive_file_key(&master(), SUB.as_bytes());
    let row_index = f.rows.len() - 1;
    f.rows[row_index]["name_encrypted"] = json!(name(&string_key, "renamed.txt"));
    let h = Harness::new(f).await;
    let v = h.json(&["ls"]).await;
    assert!(v["files"].as_array().unwrap().iter().any(|r| r["name"] == "renamed.txt"));
    assert_eq!(pull_bytes(&h, SUB).await, b"request body");
    assert_eq!(owner_key_posts(&h, SUB), 0);
    h.close().await;
}

/// A10 (L14): an outside owner item listed under R, readable through its own
/// derived key but with no wrap for (R, X, kind), is not indexed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a10_index_skips_listed_item_without_wrap() {
    let mut f = Fixture::owner_file();
    f.inventory.clear();
    let h = Harness::new(f).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    assert!(h.state.lock().unwrap().shared_index.is_empty());
    h.close().await;
}

/// A11 (L14): a wrap of another item's key is skipped as wrap_key_mismatch, even
/// though the item itself reads through its owner envelope.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a11_index_skips_wrap_of_another_items_key() {
    let mut f = Fixture::transfer(true);
    let other = FileKey::from_bytes([161; 32]);
    f.inventory = vec![json!({
        "file_id": FILE,
        "encrypted_file_key": B64.encode(
            beebeeb_core::owned_keys::wrap_child_key(&folder_key(), ROOT, FILE, 1, &other).unwrap()
        ),
    })];
    f.rows[1]["owner_wrapped_file_key"] = json!(B64.encode(
        beebeeb_core::owned_keys::wrap_owner_file_key(&master(), FILE, &item_key()).unwrap()
    ));
    let h = Harness::new(f).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    assert!(h.state.lock().unwrap().shared_index.is_empty());
    h.close().await;
}

/// A12 (L14): a folder listed under R that the proven key does not open is not
/// descended into, so its (well-wrapped) child is not indexed either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a12_index_does_not_descend_into_unproven_folder() {
    let mut f = Fixture::transfer(true);
    f.rows[1]["parent_id"] = json!(F_SUB);
    let mut nested = row(F_SUB, Some(ROOT), "Nested", RowFacts { folder: true, foreign: true });
    nested["owner_wrapped_file_key"] = json!(B64.encode(
        beebeeb_core::owned_keys::wrap_owner_file_key(&master(), F_SUB, &item_key()).unwrap()
    ));
    f.rows.push(nested);
    // Only the grandchild has a wrap; F_SUB itself has none, so R's key does not
    // prove it and the repair must not descend.
    f.inventory = vec![json!({"file_id":FILE,"encrypted_file_key":wrap(FILE,false)})];
    let h = Harness::new(f).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    assert!(h.state.lock().unwrap().shared_index.is_empty());
    h.close().await;
}

/// A14b (L15 = A): an upload into a shared folder is never published, and the
/// CLI says the item is owner-only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a14b_upload_into_shared_folder_prints_owner_only() {
    let mut f = Fixture::basic();
    f.review_complete_requested_id = true;
    let h = Harness::new(f).await;
    let path = h.home.join("note.txt");
    std::fs::write(&path, b"owner only").unwrap();
    let out = h
        .run_plain(&["push", path.to_str().unwrap(), "--folder", ROOT])
        .await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Only you can open this. Share it from the web or the app."),
        "missing the owner-only sentence: {stdout}"
    );
    assert_eq!(h.count("POST", "/owner-key"), 0);
    h.close().await;
}

/// A15 (L15 = A): an item that arrived by a remote move into R publishes nothing
/// here, and stays unreadable until the owner shares it from web or mobile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a15_remote_move_into_shared_root_publishes_nothing() {
    let mut f = Fixture::default();
    let mut r = row(FILE, Some(ROOT), "moved-in.txt", RowFacts { foreign: true, ..Default::default() });
    r["acted_via_share_id"] = json!(null);
    f.rows.push(r);
    let h = Harness::new(shared_folder(f)).await;
    let v = h.json(&["ls", "Shared"]).await;
    assert_eq!(v["files"][0]["name"], "can't open (added via shared folder)");
    assert_eq!(h.count("POST", "/owner-key"), 0);
    h.close().await;
}

/// A19 (L12): moving a folder that holds F descendants adopts every F item first,
/// and refuses the move when an adoption cannot be confirmed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a19_move_out_adopts_foreign_descendants_or_refuses() {
    let mut f = Fixture::basic();
    f.rows[1]["parent_id"] = json!(F_SUB);
    f.rows.push(row(F_SUB, Some(ROOT), "Nested", RowFacts { folder: true, foreign: true }));
    f.inventory.push(json!({"file_id":F_SUB,"encrypted_file_key":wrap(F_SUB,true)}));
    let h = Harness::new(f).await;
    ok(h.run(&["mv", "Shared/Nested", "Shared/Nested2"]).await);
    assert_eq!(owner_key_posts(&h, F_SUB), 1);
    assert_eq!(owner_key_posts(&h, FILE), 1);
    h.close().await;

    let mut f = Fixture::basic();
    f.rows[1]["parent_id"] = json!(F_SUB);
    f.rows.push(row(F_SUB, Some(ROOT), "Nested", RowFacts { folder: true, foreign: true }));
    f.inventory.push(json!({"file_id":F_SUB,"encrypted_file_key":wrap(F_SUB,true)}));
    f.adopt_failures = 1;
    let h = Harness::new(f).await;
    let out = h.run(&["mv", "Shared/Nested", "Shared/Nested2"]).await;
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("move refused"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(h.count("PATCH", "/api/v1/files/"), 0);
    h.close().await;
}

/// A24: device B, shown a second wrap after device A adopted, does not post a
/// competing one - the winner check blocks it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a24_second_device_does_not_post_a_competing_wrap() {
    let mut f = Fixture::transfer(true);
    f.rows[1]["owner_wrapped_file_key"] = json!(B64.encode(
        beebeeb_core::owned_keys::wrap_owner_file_key(&master(), FILE, &item_key()).unwrap()
    ));
    let h = Harness::new(f).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    assert_eq!(owner_key_posts(&h, FILE), 0);
    h.close().await;
}
