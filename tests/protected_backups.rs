//! LD-0113 E1 (spec Amendments 8 and 8a): the Backups tree in `bb rm`, `mv`, `mkdir`, `push` and `ls`.
//!
//! The real `bb` binary against a loopback mock that keeps a vault like the server's: the root is the folder
//! whose row carries `system_role = "backups_root"`, and the server refuses a trash or move of it with
//! `409 protected_folder` (spec §5.3). Every refusal must print core's words and exit non-zero with nothing
//! sent; a device or category folder asks "Delete <device>'s backup?" first.
//!
//! The vault, in every test:
//!
//! ```text
//! Backups/                       (the flagged root)
//!   GLegion/                     (a device folder)
//!     .device.json               (its manifest)
//!     Pictures/                  (a category folder)
//!       a.jpg
//! Docs/
//!   report.txt
//! ```

#[path = "support/push_vault.rs"]
mod support;

use beebeeb_core::backup::{MSG_BADGE, MSG_DELETE, MSG_MOVE, MSG_READ_ONLY, MSG_RENAME};
use support::*;

const HINT: &str = "To back up this machine: `bb backup add <folder>`";

struct Tree {
    root: String,
    device: String,
    manifest: String,
    category: String,
}

fn seed_file(vault: &Shared, parent: &str, name: &str) -> String {
    let item = beebeeb_core::item_id::NewItemId::owner(&master()).unwrap();
    let sealed =
        beebeeb_core::encrypt::encrypt_name_for_new_item(&master(), &item, name, None, ExpectedChunkFormat::Unrecorded)
            .unwrap();
    let mut v = vault.lock().unwrap();
    v.seq += 1;
    let (id, seq) = (item.as_str().to_string(), v.seq);
    v.items.insert(
        id.clone(),
        Item {
            id: id.clone(),
            parent: Some(parent.to_string()),
            name_encrypted: sealed,
            is_folder: false,
            seq,
            uploading: false,
            version: 1,
            chunk_count: 1,
            size: 3,
        },
    );
    id
}

async fn vault_with_backups() -> (Shared, String, PathBuf, Tree) {
    let vault: Shared = Arc::default();
    let root = seed_folder(&vault, None, "Backups");
    vault.lock().unwrap().roles.insert(root.clone(), "backups_root".into());
    let device = seed_folder(&vault, Some(&root), "GLegion");
    let manifest = seed_file(&vault, &device, ".device.json");
    let category = seed_folder(&vault, Some(&device), "Pictures");
    seed_file(&vault, &category, "a.jpg");
    let docs = seed_folder(&vault, None, "Docs");
    seed_file(&vault, &docs, "report.txt");
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let tree = Tree {
        root,
        device,
        manifest,
        category,
    };
    (vault, api, home, tree)
}

fn assert_refused(out: &Output, words: &str) {
    let err = stderr(out);
    assert_eq!(out.status.code(), Some(1), "a refusal exits non-zero: {err}");
    assert!(
        err.contains(&format!("error: {words}")),
        "core's exact words, got: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rm_backups_refused_locally_nonzero_exit_exact_words() {
    let (vault, api, home, _) = vault_with_backups().await;

    let out = bb(&home, &["--api", &api, "rm", "-r", "-f", "Backups"]).await;
    assert_refused(&out, MSG_DELETE);
    assert!(
        !stderr(&out).contains(HINT),
        "a refused delete is no write: no backup hint"
    );

    let out = bb(&home, &["--api", &api, "rm", "-r", "--permanent", "Backups"]).await;
    assert_refused(&out, MSG_DELETE);

    assert_eq!(vault.lock().unwrap().trash_calls, 0, "nothing was sent to the server");
}

#[tokio::test(flavor = "multi_thread")]
async fn rm_of_a_manifest_is_refused_and_one_backed_up_file_may_go() {
    let (vault, api, home, tree) = vault_with_backups().await;

    // By id: its depth and name come from its parents.
    let out = bb(&home, &["--api", &api, "rm", "-f", &tree.manifest]).await;
    assert_refused(&out, MSG_READ_ONLY);
    assert_eq!(vault.lock().unwrap().trash_calls, 0);

    // Rule 11: one file deleted from a backup needs no device question.
    let out = bb(&home, &["--api", &api, "rm", "-f", "Backups/GLegion/Pictures/a.jpg"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(vault.lock().unwrap().trash_calls, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn mv_backups_refused() {
    let (vault, api, home, _) = vault_with_backups().await;

    let out = bb(&home, &["--api", &api, "mv", "Backups", "Old Backups"]).await;
    assert_refused(&out, MSG_RENAME);
    let out = bb(&home, &["--api", &api, "mv", "Backups", "Docs"]).await;
    assert_refused(&out, MSG_MOVE);
    // Out of the tree, and into it: both are writes the device's own pass alone makes.
    let out = bb(&home, &["--api", &api, "mv", "Backups/GLegion/Pictures", "Docs"]).await;
    assert_refused(&out, MSG_READ_ONLY);
    let out = bb(
        &home,
        &["--api", &api, "mv", "Docs/report.txt", "Backups/GLegion/Pictures"],
    )
    .await;
    assert_refused(&out, MSG_READ_ONLY);
    assert!(
        stderr(&out).contains(HINT),
        "a refused write says how to back up: {}",
        stderr(&out)
    );
    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "mv",
            "Docs/report.txt",
            "Backups/GLegion/Pictures",
            "Docs",
        ],
    )
    .await;
    assert_refused(&out, MSG_READ_ONLY);

    assert_eq!(vault.lock().unwrap().patch_calls, 0, "nothing was sent to the server");
}

#[tokio::test(flavor = "multi_thread")]
async fn server_protected_folder_answer_prints_core_words() {
    let (vault, api, home, _) = vault_with_backups().await;
    // A listing without the role (a server from before the wire field): the CLI cannot see the root, sends
    // the trash, and the server refuses it.
    vault.lock().unwrap().hide_roles = true;

    let out = bb(&home, &["--api", &api, "rm", "-r", "-f", "Backups"]).await;
    assert_refused(&out, MSG_DELETE);
    assert!(
        !stderr(&out).contains("refused by the mock server"),
        "core's words, not the server's text"
    );
    let out = bb(&home, &["--api", &api, "mv", "Backups", "Old Backups"]).await;
    assert_refused(&out, MSG_RENAME);

    let v = vault.lock().unwrap();
    assert_eq!(
        (v.trash_calls, v.patch_calls),
        (1, 1),
        "the server was asked and refused"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn push_into_backups_refused_with_the_hint() {
    let (vault, api, home, tree) = vault_with_backups().await;
    write_file(&home, "note.txt", b"note");
    write_file(&home, "Backups/stray.txt", b"stray");

    let out = bb(&home, &["--api", &api, "push", "note.txt", "--parent", &tree.category]).await;
    assert_refused(&out, MSG_READ_ONLY);
    assert!(stderr(&out).contains(HINT), "{}", stderr(&out));
    let out = bb(&home, &["--api", &api, "push", "note.txt", "--folder", "Backups"]).await;
    assert_refused(&out, MSG_READ_ONLY);
    // A local folder named like the root reuses the root at the vault root: refused before any upload.
    let out = bb(&home, &["--api", &api, "push", "Backups"]).await;
    assert_refused(&out, MSG_READ_ONLY);

    assert_eq!(vault.lock().unwrap().inits, 0, "no upload started");
    // Outside the tree a push still works.
    let out = bb(&home, &["--api", &api, "push", "note.txt", "--folder", "Docs"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[tokio::test(flavor = "multi_thread")]
async fn mkdir_under_backups_refused() {
    let (vault, api, home, tree) = vault_with_backups().await;

    let out = bb(&home, &["--api", &api, "mkdir", "Backups/GLegion/New"]).await;
    assert_refused(&out, MSG_READ_ONLY);
    assert!(stderr(&out).contains(HINT));
    let out = bb(&home, &["--api", &api, "mkdir", "-p", "Backups/GLegion/Pictures/a/b"]).await;
    assert_refused(&out, MSG_READ_ONLY);
    let out = bb(&home, &["--api", &api, "mkdir", "Backups/Another device"]).await;
    assert_refused(&out, MSG_READ_ONLY);
    assert_eq!(vault.lock().unwrap().folder_creates, 0, "nothing was created");
    let _ = tree.root;
}

#[tokio::test(flavor = "multi_thread")]
async fn rm_of_a_device_folder_asks_and_yes_answers_for_scripts() {
    let (vault, api, home, tree) = vault_with_backups().await;

    // No terminal and no --yes: refused with exit 2, never read as yes.
    let out = bb(&home, &["--api", &api, "rm", "-r", "Backups/GLegion"]).await;
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert_eq!(vault.lock().unwrap().trash_calls, 0);

    // At a terminal the question names the device, the count and what happens next; "n" removes nothing.
    let run = bb_at_terminal(&home, &["--api", &api, "rm", "-r", "Backups/GLegion"], "n").await;
    assert!(
        run.stdout.contains(
            "Delete GLegion's backup? 3 items. GLegion will stop backing up these folders until you choose Back up \
             again on it."
        ),
        "{run:?}"
    );
    assert_eq!(vault.lock().unwrap().trash_calls, 0, "{run:?}");
    let run = bb_at_terminal(&home, &["--api", &api, "rm", "-r", &tree.category], "n").await;
    assert!(
        run.stdout.contains("Delete GLegion's backup of Pictures? 1 item."),
        "{run:?}"
    );

    // --yes answers it.
    let out = bb(&home, &["--api", &api, "rm", "-r", "--yes", "Backups/GLegion"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    assert_eq!(v.trash_calls, 1);
    assert!(v.trashed.contains(&tree.device));
}

/// Review #7: `-f`/`--force` skips the soft-trash prompt only; it never answers "Delete <device>'s backup?", so
/// the habitual `bb rm -rf Backups/<device>` stops and asks for `--yes`. Outside Backups `-f` and `--force` still
/// trash without a prompt, as before, and `--yes` answers both.
#[tokio::test(flavor = "multi_thread")]
async fn rm_force_skips_the_trash_prompt_but_never_answers_the_backup_question() {
    let (vault, api, home, tree) = vault_with_backups().await;
    for flags in [&["-rf"][..], &["-r", "-f"], &["-r", "--force"]] {
        let args = [&["--api", api.as_str(), "rm"][..], flags, &["Backups/GLegion"]].concat();
        let out = bb(&home, &args).await;
        assert_eq!(out.status.code(), Some(2), "{flags:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("--yes"), "{flags:?}: {}", stderr(&out));
    }
    assert_eq!(vault.lock().unwrap().trash_calls, 0, "no backup trashed");

    let docs = the_folder(&vault.lock().unwrap(), None, "Docs");
    let two = seed_file(&vault, &docs, "two.txt");
    let three = seed_file(&vault, &docs, "three.txt");
    for (flag, name, id) in [
        ("-f", "Docs/report.txt", None),
        ("--force", "Docs/two.txt", Some(&two)),
        ("--yes", "Docs/three.txt", Some(&three)),
    ] {
        let out = bb(&home, &["--api", &api, "rm", flag, name]).await;
        assert_eq!(out.status.code(), Some(0), "{flag} {name}: {}", stderr(&out));
        if let Some(id) = id {
            assert!(vault.lock().unwrap().trashed.contains(id), "{name} trashed");
        }
    }
    assert!(!vault.lock().unwrap().trashed.contains(&tree.device));
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_shows_the_read_only_badge_and_never_the_manifest() {
    let (_vault, api, home, _) = vault_with_backups().await;

    let out = bb(&home, &["--api", &api, "ls"]).await;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let root_line = text.lines().find(|l| l.contains("Backups/")).unwrap_or_default();
    assert!(root_line.contains(&format!("[{MSG_BADGE}]")), "{text}");
    let docs_line = text.lines().find(|l| l.contains("Docs/")).unwrap_or_default();
    assert!(!docs_line.contains(MSG_BADGE), "only the root has the badge: {text}");

    let out = bb(&home, &["--api", &api, "ls", "Backups/GLegion"]).await;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.contains("Pictures/"), "{text}");
    assert!(!text.contains(".device.json"), "the manifest is never listed: {text}");
    assert!(text.contains("1 items"), "{text}");

    let out = bb(&home, &["--api", &api, "--json", "ls", "-R", "Backups"]).await;
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert!(
        names.contains(&"a.jpg") && !names.contains(&".device.json"),
        "{names:?}"
    );
    let out = bb(&home, &["--api", &api, "--json", "ls"]).await;
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let root = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Backups")
        .unwrap();
    assert_eq!(root["badge"], MSG_BADGE);
    assert_eq!(root["system_role"], "backups_root");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_shared_with_me_whose_parent_i_cannot_read_is_outside_the_tree() {
    let (vault, api, home, _) = vault_with_backups().await;
    // A share's root: its parent is the sharer's folder, which GET /files/{id} answers 404 for.
    let sharers_folder = uuid::Uuid::new_v4().to_string();
    let shared = seed_folder(&vault, Some(&sharers_folder), "Shared with me");
    write_file(&home, "note.txt", b"note");

    let out = bb(&home, &["--api", &api, "push", "note.txt", "--parent", &shared]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(vault.lock().unwrap().inits, 1);
}

/// Fail closed: when the CLI cannot read the folders above the destination, it cannot tell whether the
/// destination is in the Backups tree, so the push stops with that reason and sends nothing (a 5xx is not a
/// share boundary, unlike the 403/404 above a folder shared with this account).
#[tokio::test(flavor = "multi_thread")]
async fn a_push_whose_destination_cannot_be_placed_is_refused_and_uploads_nothing() {
    let (vault, api, home, _) = vault_with_backups().await;
    let docs = the_folder(&vault.lock().unwrap(), None, "Docs");
    let inner = seed_folder(&vault, Some(&docs), "Inner");
    vault.lock().unwrap().file_status_for.insert(docs.clone(), 503);
    write_file(&home, "note.txt", b"note");
    write_file(&home, "photos/a.jpg", b"a");

    for (what, dest) in [("the destination", &docs), ("the destination's parent", &inner)] {
        for local in ["note.txt", "photos"] {
            let out = bb(&home, &["--api", &api, "push", local, "--parent", dest]).await;
            let err = stderr(&out);
            assert_eq!(out.status.code(), Some(1), "{what} unreadable, {local}: {err}");
            assert!(
                err.contains(
                    "error: could not check whether the destination folder is inside Backups, so the push stops: \
                     failed by mock"
                ),
                "{what} unreadable, {local}: {err}"
            );
        }
    }
    let v = vault.lock().unwrap();
    assert_eq!((v.inits, v.folder_creates), (0, 0), "nothing uploaded, no folder made");
}

/// The rule reads only what the push does not already know: nothing for a folder found in a listing the push
/// read or made by the push itself, and each folder above a `--parent` once per run. `--json`: the owner-only
/// notice, which reads the destination's folders after the upload, is left out, so every read counted is the rule's.
#[tokio::test(flavor = "multi_thread")]
async fn the_backups_check_reads_each_unknown_folder_once_and_known_ones_never() {
    let (vault, api, home, _) = vault_with_backups().await;
    let docs = the_folder(&vault.lock().unwrap(), None, "Docs");
    let inner = seed_folder(&vault, Some(&docs), "Inner");
    write_file(&home, "photos/a.jpg", b"a");
    write_file(&home, "photos/2026/b.jpg", b"b");

    for args in [
        vec!["--json", "push", "photos"],
        vec!["--json", "push", "photos", "--folder", "Docs"],
        vec!["--json", "push", "photos", "--folder", "New"],
    ] {
        vault.lock().unwrap().file_reads.clear();
        let out = bb(&home, &[&["--api", api.as_str()][..], &args[..]].concat()).await;
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stderr(&out));
        let reads = vault.lock().unwrap().file_reads.clone();
        assert!(reads.is_empty(), "{args:?}: no folder row read, got {reads:?}");
    }

    vault.lock().unwrap().file_reads.clear();
    let out = bb(&home, &["--api", &api, "--json", "push", "photos", "--parent", &inner]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let reads = vault.lock().unwrap().file_reads.clone();
    assert_eq!(
        reads,
        HashMap::from([(inner.clone(), 1), (docs.clone(), 1)]),
        "Inner and Docs read once each, nothing below them"
    );
}
