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
        f.rows.insert(
            0,
            row(
                ROOT,
                None,
                "Shared",
                RowFacts {
                    folder: true,
                    ..Default::default()
                },
            ),
        );
    }
    f
}
/// The sync pass's JSON report sits among its log lines: take the object span.
fn sync_report(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    match (stdout.find('{'), stdout.rfind('}')) {
        (Some(a), Some(b)) if a <= b => serde_json::from_str(&stdout[a..=b]).unwrap_or(Value::Null),
        _ => Value::Null,
    }
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
        RowFacts {
            foreign: true,
            ..Default::default()
        },
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

/// A3 (P2-9): an O item carrying a WELL-FORMED historical L8 owner envelope —
/// one that really opens under its key, with the name sealed under that same
/// key — is ignored: the id class decides, the derived key wins, the name does
/// not authenticate under it, and nothing is adopted. If the envelope were
/// used the item would read "a3.txt", so this test can fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a3_owner_id_l8_envelope_is_ignored() {
    let mut f = Fixture::default();
    let mut r = row(O_FILE, None, "a3.txt", RowFacts { ..Default::default() });
    r["name_encrypted"] = json!(name(&item_key(), "a3.txt"));
    r["owner_wrapped_file_key"] = json!(l8_envelope(O_FILE, &item_key()));
    f.rows.push(r);
    let h = Harness::new(shared_folder(f)).await;
    let v = h.json(&["ls"]).await;
    // The envelope's key really would open this name: the test can fail.
    let sealed = {
        let state = h.state.lock().unwrap();
        state.rows.iter().find(|r| r["id"] == O_FILE).unwrap()["name_encrypted"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(decrypt_name(&item_key(), &sealed), "a3.txt");
    assert_eq!(
        v["files"][0]["name"], "can't open (added via shared folder)",
        "the L8 envelope must not open an O item: {v}"
    );
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

/// A9b (sync): a content edit of a request row is saved as a NEW O item by the
/// sync path too — never sealed under C as a version of the request row, and the
/// request row is left untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a9b_sync_request_row_edit_becomes_new_item() {
    let mut f = Fixture::basic();
    f.plaintext = b"request body".to_vec();
    f.add_request();
    f.rows.last_mut().unwrap()["parent_id"] = json!(ROOT);
    f.review_complete_requested_id = true;
    let h = Harness::new(f).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    let local = d.join("request.txt");
    assert_eq!(std::fs::read(&local).unwrap(), b"request body");
    std::fs::write(&local, b"edited body").unwrap();
    ok(sync(&h, &d).await);

    let (uploaded, chunks) = {
        let state = h.state.lock().unwrap();
        let body = state.upload_request.clone();
        (
            body["file_id"].as_str().unwrap().to_string(),
            state.uploaded_chunks.clone(),
        )
    };
    assert_ne!(
        uploaded, SUB,
        "a content edit is a new item, not a version of the request row"
    );
    assert_eq!(
        h.count("PATCH", &format!("/api/v1/files/{SUB}")),
        0,
        "the request row is not renamed"
    );
    assert_eq!(
        h.count("DELETE", &format!("/api/v1/files/{SUB}")),
        0,
        "the request row is not trashed"
    );
    assert_eq!(
        pull_bytes(&h, SUB).await,
        b"request body",
        "the request row's content is untouched"
    );
    // The edit was sealed under the new owner id, never under C.
    let new_key = beebeeb_core::kdf::derive_file_key(&master(), uploaded.as_bytes());
    assert_eq!(
        beebeeb_core::encrypt::decrypt_chunk_raw(&new_key, &chunks[0]).unwrap(),
        b"edited body"
    );
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
    assert!(
        v["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["name"] == "renamed.txt")
    );
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
    let out = sync(&h, &d).await;
    assert_eq!(sync_report(&out)["index_skip_reasons"]["no_wrap"], 1, "reason");
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
    f.rows[1]["owner_wrapped_file_key"] =
        json!(B64.encode(beebeeb_core::owned_keys::wrap_owner_file_key(&master(), FILE, &item_key()).unwrap()));
    let h = Harness::new(f).await;
    let d = dir(&h);
    let out = sync(&h, &d).await;
    assert_eq!(
        sync_report(&out)["index_skip_reasons"]["wrap_key_mismatch"],
        1,
        "the wrap names another key"
    );
    assert!(h.state.lock().unwrap().shared_index.is_empty());
    h.close().await;
}

/// A10/A11 (L14, two roots): an item listed under R1 but wrapped only in R2's
/// inventory is skipped — R2's wrap proves nothing about R1's custody — so it
/// is never indexed under R1. Reason: `wrap_does_not_open` (a wrap exists, but
/// not the one this root would need).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a10_index_skips_item_wrapped_only_in_another_root() {
    let r2_key = FileKey::from_bytes([129; 32]);
    let mut f = Fixture::transfer(true);
    // The victim, listed under R1 (ROOT), reads through its owner envelope.
    f.rows[1]["owner_wrapped_file_key"] =
        json!(B64.encode(beebeeb_core::owned_keys::wrap_owner_file_key(&master(), FILE, &item_key()).unwrap()));
    // R2 is a proven root the walk reaches (a folder under R1 whose child
    // resolves through R2's custody), and its inventory wraps FILE - an item
    // that lives under R1 - and its own child F_BAD.
    let mut r2 = row(
        F_SUB,
        Some(ROOT),
        "Other",
        RowFacts {
            folder: true,
            foreign: true,
        },
    );
    r2["owner_wrapped_file_key"] =
        json!(B64.encode(beebeeb_core::owned_keys::wrap_owner_file_key(&master(), F_SUB, &item_key()).unwrap()));
    f.rows.push(r2);
    f.rows.push(row(
        F_BAD,
        Some(F_SUB),
        "inner",
        RowFacts {
            folder: true,
            foreign: true,
        },
    ));
    f.inventory.clear();
    f.extra_root_keys.insert(F_SUB.into(), *r2_key.as_bytes());
    f.extra_root_inventories.insert(
        F_SUB.into(),
        vec![
            json!({"file_id":FILE,"encrypted_file_key":B64.encode(beebeeb_core::owned_keys::wrap_child_key(&r2_key, F_SUB, FILE, 1, &item_key()).unwrap())}),
            json!({"file_id":F_BAD,"encrypted_file_key":B64.encode(beebeeb_core::owned_keys::wrap_child_key(&r2_key, F_SUB, F_BAD, 2, &item_key()).unwrap())}),
        ],
    );
    let h = Harness::new(shared_folder(f)).await;
    let d = dir(&h);
    let out = sync(&h, &d).await;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let report: Value = match (stdout.find('{'), stdout.rfind('}')) {
        (Some(a), Some(b)) if a <= b => serde_json::from_str(&stdout[a..=b]).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    ok(out);
    assert_eq!(
        report["index_skip_reasons"]["wrap_does_not_open"], 1,
        "the other root's wrap must not make the item indexable here: {stdout}"
    );
    h.close().await;
}

/// A12 (L14): a folder listed under R that the proven key does not open is not
/// descended into, so its (well-wrapped) child is not indexed either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a12_index_does_not_descend_into_unproven_folder() {
    let mut f = Fixture::transfer(true);
    f.rows[1]["parent_id"] = json!(F_SUB);
    let mut nested = row(
        F_SUB,
        Some(ROOT),
        "Nested",
        RowFacts {
            folder: true,
            foreign: true,
        },
    );
    nested["owner_wrapped_file_key"] =
        json!(B64.encode(beebeeb_core::owned_keys::wrap_owner_file_key(&master(), F_SUB, &item_key()).unwrap()));
    f.rows.push(nested);
    // Only the grandchild has a wrap; F_SUB itself has none, so R's key does not
    // prove it and the repair must not descend.
    f.inventory = vec![json!({"file_id":FILE,"encrypted_file_key":wrap(FILE,false)})];
    let h = Harness::new(f).await;
    let d = dir(&h);
    let out = sync(&h, &d).await;
    assert_eq!(
        sync_report(&out)["index_skip_reasons"]["no_wrap"],
        1,
        "the unproven folder itself is skipped with its reason"
    );
    assert!(h.state.lock().unwrap().shared_index.is_empty());
    h.close().await;
}

/// A14b (L15 = A): an upload into a shared folder is never published, and the
/// CLI says the item is owner-only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a14b_upload_into_shared_folder_prints_owner_only() {
    const HOST: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    let mut f = Fixture::basic();
    f.rows.push(row(HOST, None, "host.txt", RowFacts::default()));
    f.review_complete_requested_id = true;
    let h = Harness::new(f).await;
    let path = h.home.join("note.txt");
    std::fs::write(&path, b"owner only").unwrap();
    let out = h.run_plain(&["push", path.to_str().unwrap(), "--folder", ROOT]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Only you can open this. Share it from the web or the app."),
        "missing the owner-only sentence: {stdout}"
    );
    // mkdir into the shared root says the same sentence.
    let out = h.run_plain(&["mkdir", "/Shared/only-mine"]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Only you can open this. Share it from the web or the app."),
        "mkdir lacks the owner-only sentence: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // and so does a move into it.
    let out = h.run_plain(&["mv", "/host.txt", "/Shared"]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Only you can open this. Share it from the web or the app."),
        "mv lacks the owner-only sentence: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(h.count("POST", "/owner-key"), 0);
    h.close().await;
}

/// A15 (L15 = A): an item that arrived by a remote move into R publishes nothing
/// here, and stays unreadable until the owner shares it from web or mobile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a15_remote_move_into_shared_root_publishes_nothing() {
    let mut f = Fixture::default();
    let mut r = row(
        FILE,
        Some(ROOT),
        "moved-in.txt",
        RowFacts {
            foreign: true,
            ..Default::default()
        },
    );
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
    const NESTED_FILE: &str = "99999999-9999-8999-9999-999999999999";
    let mut f = Fixture::basic();
    f.rows[1]["parent_id"] = json!(F_SUB);
    f.rows.push(row(
        F_SUB,
        Some(ROOT),
        "Nested",
        RowFacts {
            folder: true,
            foreign: true,
        },
    ));
    f.rows.push(row(
        NESTED_FILE,
        Some(F_SUB),
        "inner.txt",
        RowFacts {
            foreign: true,
            ..Default::default()
        },
    ));
    f.inventory
        .push(json!({"file_id":F_SUB,"encrypted_file_key":wrap(F_SUB,true)}));
    f.inventory
        .push(json!({"file_id":NESTED_FILE,"encrypted_file_key":wrap(NESTED_FILE,false)}));
    // A real move OUT of the shared root, to a folder at the vault root.
    const DEST: &str = "66666666-6666-4666-8666-666666666666";
    f.rows.push(row(
        DEST,
        None,
        "Dest",
        RowFacts {
            folder: true,
            ..Default::default()
        },
    ));
    let h = Harness::new(f).await;
    let out = h.run(&["mv", "Shared/Nested", "Dest/Nested"]).await;
    let trace: Vec<String> = h
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .map(|(m, p, _)| format!("{m} {p}"))
        .collect();
    assert!(
        out.status.success(),
        "{} / {trace:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(owner_key_posts(&h, F_SUB), 1);
    assert_eq!(owner_key_posts(&h, FILE), 1);
    let patched = h
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .find(|(m, p, _)| m == "PATCH" && p == &format!("/api/v1/files/{F_SUB}"))
        .map(|(_, _, b)| serde_json::from_slice::<Value>(b).unwrap())
        .expect("the folder is moved out");
    assert_eq!(patched["parent_id"], json!(DEST), "moved out of the shared root");
    h.close().await;

    let mut f = Fixture::basic();
    f.rows[1]["parent_id"] = json!(F_SUB);
    f.rows.push(row(
        F_SUB,
        Some(ROOT),
        "Nested",
        RowFacts {
            folder: true,
            foreign: true,
        },
    ));
    f.inventory
        .push(json!({"file_id":F_SUB,"encrypted_file_key":wrap(F_SUB,true)}));
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
    f.rows[1]["owner_wrapped_file_key"] =
        json!(B64.encode(beebeeb_core::owned_keys::wrap_owner_file_key(&master(), FILE, &item_key()).unwrap()));
    let h = Harness::new(f).await;
    let d = dir(&h);
    ok(sync(&h, &d).await);
    assert_eq!(owner_key_posts(&h, FILE), 0);
    h.close().await;
}

/// A27 (R7): a share link for a legacy binary-convention O item carries the key
/// that actually opens it — the one its name authenticated under — never the
/// string-convention key the old code derived for every row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a27_share_link_carries_the_key_that_opens_a_binary_convention_item() {
    let h = Harness::new(shared_folder(Fixture::owner_file_json_blob())).await;
    let out = h.json(&["share", O_FILE]).await;
    let k_c = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(out["decryption_key"].as_str().unwrap())
        .unwrap();
    assert_eq!(k_c.len(), 32, "the printed link carries a 32-byte K_c");
    let mut k_c_arr = [0u8; 32];
    k_c_arr.copy_from_slice(&k_c);
    let body = h
        .state
        .lock()
        .unwrap()
        .requests
        .iter()
        .rev()
        .find(|(m, p, _)| m == "POST" && p == "/api/v1/shares")
        .map(|(_, _, b)| b.clone())
        .expect("share create body");
    let body: Value = serde_json::from_slice(&body).unwrap();
    let wrapped = B64.decode(body["wrapped_file_key"].as_str().unwrap()).unwrap();
    let opened = beebeeb_core::encrypt::decrypt_chunk_raw(&FileKey::from_bytes(k_c_arr), &wrapped).unwrap();
    assert_eq!(opened.len(), 32);
    let mut fk = [0u8; 32];
    fk.copy_from_slice(&opened);

    let uuid = uuid::Uuid::parse_str(O_FILE).unwrap();
    let binary = beebeeb_core::kdf::derive_file_key(&master(), uuid.as_bytes());
    let string = beebeeb_core::kdf::derive_file_key(&master(), O_FILE.as_bytes());
    assert_eq!(
        opened,
        binary.as_bytes().to_vec(),
        "the link must carry the binary-convention key that opens the item"
    );
    assert_ne!(
        opened,
        string.as_bytes().to_vec(),
        "R7: never the string-convention key that does not open it"
    );
    let row = {
        let state = h.state.lock().unwrap();
        state
            .canonical
            .get(O_FILE)
            .cloned()
            .or_else(|| state.rows.iter().find(|r| r["id"] == O_FILE).cloned())
            .expect("the shared row")
    };
    assert_eq!(
        decrypt_name(&FileKey::from_bytes(fk), row["name_encrypted"].as_str().unwrap()),
        "legacy.txt",
        "the disclosed key is the one the served name authenticated under"
    );
    h.close().await;
}

/// A27/R5: a request row has no key this account can prove, so `bb share`
/// refuses it and says why instead of wrapping some derived guess.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a27_share_refuses_a_request_row() {
    let mut f = Fixture::basic();
    f.plaintext = b"request body".to_vec();
    f.add_request();
    let h = Harness::new(shared_folder(f)).await;
    let out = h.run(&["share", SUB]).await;
    assert!(!out.status.success(), "a request row must not be shareable");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("request upload"), "{text}");
    assert_eq!(h.count("POST", "/api/v1/shares"), 0, "nothing was published");
    h.close().await;
}

/// A19 (P2-5): the OTHER move surfaces adopt too. Bulk `bb mv` is not the
/// single-source path: every source leaving a shared root adopts its foreign
/// subtree (confirmed by a readback) BEFORE the move, and the move then lands in
/// the destination outside the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a19_bulk_move_out_of_a_shared_root_adopts_each_source_first() {
    const DEST: &str = "66666666-6666-4666-8666-666666666666";
    const NESTED_FILE: &str = "99999999-9999-8999-9999-999999999999";
    let mut f = Fixture::basic();
    f.rows.push(row(
        F_SUB,
        Some(ROOT),
        "Nested",
        RowFacts {
            folder: true,
            foreign: true,
        },
    ));
    f.rows.push(row(
        NESTED_FILE,
        Some(F_SUB),
        "inner.txt",
        RowFacts {
            foreign: true,
            ..Default::default()
        },
    ));
    f.inventory
        .push(json!({"file_id":F_SUB,"encrypted_file_key":wrap(F_SUB,true)}));
    f.inventory
        .push(json!({"file_id":NESTED_FILE,"encrypted_file_key":wrap(NESTED_FILE,false)}));
    // A destination at the vault root, outside every shared folder.
    f.rows.push(row(
        DEST,
        None,
        "Dest",
        RowFacts {
            folder: true,
            ..Default::default()
        },
    ));
    let h = Harness::new(f).await;
    ok(h.run(&["mv", "Shared/Nested", "Shared/guest.txt", "Dest/"]).await);

    assert_eq!(owner_key_posts(&h, F_SUB), 1, "the foreign folder is adopted first");
    assert_eq!(owner_key_posts(&h, FILE), 1, "the foreign file is adopted first");
    let requests = h.state.lock().unwrap().requests.clone();
    let index = |m: &str, p: &str| {
        requests
            .iter()
            .position(|(rm, rp, _)| rm == m && rp == p)
            .unwrap_or_else(|| panic!("no {m} {p} in {requests:?}"))
    };
    for id in [F_SUB, FILE] {
        assert!(
            index("POST", &format!("/api/v1/files/{id}/owner-key")) < index("PATCH", &format!("/api/v1/files/{id}")),
            "each source is adopted before it moves"
        );
        let patch = requests
            .iter()
            .find(|(m, p, _)| m == "PATCH" && p == &format!("/api/v1/files/{id}"))
            .expect("the source is moved");
        let body: Value = serde_json::from_slice(&patch.2).unwrap();
        assert_eq!(body["parent_id"], json!(DEST), "moved into the destination: {body}");
    }
    h.close().await;
}

/// A1–A3 through the SYNC path (P2-9): the three O-item shapes (a member-made
/// wrap, a real derived name, and a well-formed historical L8 envelope) are
/// listed by a sync pass too, and none of them posts a child wrap. `bb ls` alone
/// never exercised the index walk.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a1_to_a3_through_sync_post_no_child_wrap() {
    const A2: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const A3: &str = "77777777-7777-4777-8777-777777777777";
    let mut f = Fixture::default();
    // A1: an O item listed foreign, name under K', member-made wrap of K'.
    f.rows.push(row(
        O_FILE,
        Some(ROOT),
        "a1.txt",
        RowFacts {
            foreign: true,
            ..Default::default()
        },
    ));
    f.inventory
        .push(json!({"file_id":O_FILE,"encrypted_file_key":wrap(O_FILE,false)}));
    // A2: an O item whose name really is under its derived key.
    let mut a2 = row(A2, Some(ROOT), "a2.txt", RowFacts::default());
    a2["acted_via_share_id"] = json!(BAD);
    f.rows.push(a2);
    f.inventory
        .push(json!({"file_id":A2,"encrypted_file_key":wrap(A2,false)}));
    // A3: an O item carrying a well-formed historical L8 envelope.
    let mut a3 = row(A3, Some(ROOT), "a3.txt", RowFacts::default());
    a3["name_encrypted"] = json!(name(&item_key(), "a3.txt"));
    a3["owner_wrapped_file_key"] = json!(l8_envelope(A3, &item_key()));
    f.rows.push(a3);
    f.inventory
        .push(json!({"file_id":A3,"encrypted_file_key":wrap(A3,false)}));
    let h = Harness::new(shared_folder(f)).await;
    let d = dir(&h);
    let out = sync(&h, &d).await;
    assert!(
        Path::new(&d).exists(),
        "the sync pass ran: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(h.count("POST", "/owner-key"), 0, "no O item is ever adopted");
    h.close().await;
}

/// P2-11 (L15 = A): the owner-only sentence is printed ONCE per command for a
/// write into a shared root — bulk `bb mv` and a directory `bb push` included,
/// where the per-file notice is suppressed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_owner_only_notice_is_printed_once_per_command() {
    const NOTICE: &str = "Only you can open this. Share it from the web or the app.";
    const A: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const B: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let mut f = Fixture::basic();
    f.review_complete_requested_id = true;
    f.rows.push(row(A, None, "one.txt", RowFacts::default()));
    f.rows.push(row(B, None, "two.txt", RowFacts::default()));
    let h = Harness::new(f).await;

    // Bulk move of two items into the shared root: one sentence, not two.
    let out = h.run_plain(&["mv", "/one.txt", "/two.txt", "/Shared"]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).matches(NOTICE).count(),
        1,
        "one notice for the command: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // A directory push into the shared folder: one sentence for the batch.
    let dir = h.home.join("batch");
    std::fs::create_dir(&dir).unwrap();
    for name in ["x.txt", "y.txt"] {
        std::fs::write(dir.join(name), b"owner only").unwrap();
    }
    let out = h.run_plain(&["push", dir.to_str().unwrap(), "--folder", ROOT]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).matches(NOTICE).count(),
        1,
        "one notice for the batch: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(h.count("POST", "/owner-key"), 0);
    h.close().await;
}
