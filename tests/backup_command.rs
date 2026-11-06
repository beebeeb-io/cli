//! LD-0113 E2 (spec Amendment 8, Amendment 12): `bb backup add|run|status` on a headless Linux box.
//!
//! The real `bb` binary against the loopback mock vault of the push tests, which keeps rows, upload sessions
//! and versions like the server and also answers `PUT /backups/root`. The machine id and the pretty hostname
//! come from files in the scratch home (the `BB_BACKUP_MACHINE_*_FILE` seams), so the device folder is
//! `Backups/nas01`.

#[path = "support/push_vault.rs"]
mod support;

use support::*;

fn machine(home: &Path) -> Vec<(&'static str, String)> {
    let id = home.join("machine-id");
    let info = home.join("machine-info");
    std::fs::write(&id, "0123456789abcdef0123456789abcdef\n").unwrap();
    std::fs::write(&info, "PRETTY_HOSTNAME=\"nas01\"\n").unwrap();
    vec![
        ("BB_BACKUP_MACHINE_ID_FILE", id.display().to_string()),
        ("BB_BACKUP_MACHINE_INFO_FILE", info.display().to_string()),
    ]
}

async fn setup() -> (Shared, String, PathBuf, Vec<(&'static str, String)>) {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let env = machine(&home);
    write_file(&home, "data/a.txt", b"first");
    write_file(&home, "data/sub/b.txt", b"second file");
    (vault, api, home, env)
}

async fn backup(home: &Path, api: &str, env: &[(&str, String)], args: &[&str]) -> Output {
    let mut all = vec!["--api", api, "backup"];
    all.extend_from_slice(args);
    bb_env(home, &all, env).await
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The ids of `Backups`, `Backups/nas01` and `Backups/nas01/data`.
fn tree_ids(v: &Vault) -> (String, String, String) {
    let root = the_folder(v, None, "Backups");
    assert_eq!(
        v.roles.get(&root).map(String::as_str),
        Some("backups_root"),
        "the root is flagged"
    );
    let device = the_folder(v, Some(&root), "nas01");
    let data = the_folder(v, Some(&device), "data");
    (root, device, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_uploads_new_files_and_a_second_pass_uploads_none() {
    let (vault, api, home, env) = setup().await;
    let out = backup(&home, &api, &env, &["add", "data"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let inits_after_first = {
        let v = vault.lock().unwrap();
        let (_, device, data) = tree_ids(&v);
        assert_eq!(
            file_names_in(&v, &data),
            vec!["a.txt".to_string()],
            "a.txt in the category folder"
        );
        let sub = the_folder(&v, Some(&data), "sub");
        assert_eq!(file_names_in(&v, &sub), vec!["b.txt".to_string()]);
        assert_eq!(
            file_names_in(&v, &device),
            vec![".device.json".to_string()],
            "the device's manifest"
        );
        // Two files and the manifest, each completed.
        assert_eq!(v.inits, 3);
        assert!(files_named(&v, "a.txt").iter().all(|f| f.2), "completed");
        v.inits
    };
    assert!(stdout(&out).contains("2 uploaded"), "{}", stdout(&out));

    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        vault.lock().unwrap().inits,
        inits_after_first,
        "a second pass uploads 0"
    );
    assert!(stdout(&out).contains("0 uploaded"), "{}", stdout(&out));
    assert!(stdout(&out).contains("2 unchanged"), "{}", stdout(&out));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_file_uploads_as_a_new_version() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let first = {
        let v = vault.lock().unwrap();
        let files = files_named(&v, "a.txt");
        assert_eq!(files.len(), 1);
        files[0].0.clone()
    };

    write_file(&home, "data/a.txt", b"first, and then changed");
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    let files = files_named(&v, "a.txt");
    assert_eq!(files.len(), 1, "one file with its history, never two: {files:?}");
    assert_eq!(files[0].0, first, "the same file id");
    assert_eq!(v.items[&first].version, 2, "a new version");
    assert_eq!(v.items[&first].size, b"first, and then changed".len() as u64);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_under_an_excluded_dir_is_refused() {
    let (_vault, api, home, env) = setup().await;
    // The CLI's own state dir (the session and key live there).
    std::fs::create_dir_all(home.join(".config/beebeeb/inside")).unwrap();
    let out = backup(&home, &api, &env, &["add", ".config/beebeeb/inside"]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("one of bb's own folders"), "{}", stderr(&out));
    // A `bb sync` folder, or a folder inside one.
    write_file(&home, "synced/.bb-sync.json", b"{}");
    write_file(&home, "synced/photos/p.jpg", b"p");
    let out = backup(&home, &api, &env, &["add", "synced/photos"]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("a `bb sync` folder"), "{}", stderr(&out));

    let out = backup(&home, &api, &env, &["list"]).await;
    assert!(
        stdout(&out).contains("no folders are backed up"),
        "nothing was added: {}",
        stdout(&out)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_backup_request_carries_the_origin_header() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    vault.lock().unwrap().requests.clear();
    write_file(&home, "data/sub/b.txt", b"second file, changed");

    for _ in 0..2 {
        let out = backup(&home, &api, &env, &["run"]).await;
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }
    let v = vault.lock().unwrap();
    assert!(v.requests.len() > 10, "the runs made requests: {}", v.requests.len());
    let untagged: Vec<_> = v
        .requests
        .iter()
        .filter(|(_, _, o)| o.as_deref() != Some("backup"))
        .collect();
    assert!(
        untagged.is_empty(),
        "requests without X-Beebeeb-Origin: backup: {untagged:?}"
    );
    for path in ["/api/v1/backups/root", "/api/v1/uploads/init", "/api/v1/files/folder"] {
        assert!(v.requests.iter().any(|(_, p, _)| p == path), "{path} was called");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn status_prints_the_counts() {
    let (_vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["status"]).await;
    assert!(stdout(&out).contains("not run yet"), "{}", stdout(&out));
    backup(&home, &api, &env, &["run"]).await;

    let out = backup(&home, &api, &env, &["status"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("2 files backed up"), "{text}");
    assert!(
        text.contains("2 uploaded \u{b7} 0 failed \u{b7} 0 moved \u{b7} 0 unchanged \u{b7} 0 skipped"),
        "{text}"
    );
    let out = backup(&home, &api, &env, &["--json", "status"]).await;
    let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(&out)));
    assert_eq!(json["folders"][0]["backed_up"], 2);
    assert_eq!(json["folders"][0]["last_run"]["uploaded"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_deleted_elsewhere_turns_off_and_is_never_uploaded_again_by_itself() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    backup(&home, &api, &env, &["run"]).await;
    let data = {
        let mut v = vault.lock().unwrap();
        let (_, _, data) = tree_ids(&v);
        for id in subtree(&v, &data) {
            v.trashed.insert(id);
        }
        data
    };
    let inits = vault.lock().unwrap().inits;

    let words = "nas01's backup of data was deleted from another device.";
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(4), "the off code: {}", stderr(&out));
    assert!(stdout(&out).contains(words), "{}", stdout(&out));
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    let out = backup(&home, &api, &env, &["status"]).await;
    assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    assert!(stdout(&out).contains(words), "{}", stdout(&out));
    {
        let v = vault.lock().unwrap();
        // One upload only: the manifest's new version, which says the folder is off (`enabled: false`).
        assert_eq!(v.inits, inits + 1, "no file of the folder uploaded while it is off");
        assert_eq!(files_named(&v, "a.txt").len(), 1, "a.txt was not uploaded again");
        assert_eq!(files_named(&v, ".device.json").len(), 1);
    }

    // Back up again, on purpose: a first backup into a new folder.
    let out = backup(&home, &api, &env, &["add", "data"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    let root = the_folder(&v, None, "Backups");
    let device = the_folder(&v, Some(&root), "nas01");
    let live: Vec<String> = folders_named(&v, Some(&device), "data")
        .into_iter()
        .filter(|id| !v.trashed.contains(id))
        .collect();
    assert_eq!(live.len(), 1, "one live category folder: {live:?}");
    assert_ne!(live[0], data, "a new category folder");
    assert_eq!(file_names_in(&v, &live[0]), vec!["a.txt".to_string()]);
}
