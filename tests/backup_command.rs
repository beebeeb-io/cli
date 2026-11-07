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

/// This machine's `backup.json`, somewhere under the scratch home's state dir.
fn settings_file(home: &Path) -> PathBuf {
    fn find(dir: &Path) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find(&path) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|n| n == "backup.json") {
                return Some(path);
            }
        }
        None
    }
    find(home).expect("backup.json under the scratch home")
}

fn settings(home: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(settings_file(home)).unwrap()).unwrap()
}

/// Rewrite `backup.json` through `change`.
fn change_settings(home: &Path, change: impl FnOnce(&mut Value)) {
    let mut value = settings(home);
    change(&mut value);
    std::fs::write(settings_file(home), serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

/// The live (not trashed) folders named `name` in `parent`.
fn live_folders(v: &Vault, parent: &str, name: &str) -> Vec<String> {
    folders_named(v, Some(parent), name)
        .into_iter()
        .filter(|id| !v.trashed.contains(id))
        .collect()
}

/// Review #8 (spec §4.3): a file that changes after its upload began is abandoned before it completes, never
/// stored half old, half new; the run counts it as waiting for the next run, not as failed, and the next run
/// backs up what the file is now.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_changed_while_it_is_read_is_abandoned_and_waits_for_the_next_run() {
    let (vault, api, home, env) = setup().await;
    let file = write_file(&home, "solo/c.txt", b"before");
    let out = backup(&home, &api, &env, &["add", "solo"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    vault.lock().unwrap().hold_chunks = true;
    let run = {
        let (home, api, env) = (home.clone(), api.clone(), env.clone());
        tokio::spawn(async move { backup(&home, &api, &env, &["run"]).await })
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while vault.lock().unwrap().sessions.is_empty() {
        assert!(Instant::now() < deadline, "the upload never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::write(&file, b"after, and longer").unwrap();
    vault.lock().unwrap().hold_chunks = false;
    let out = run.await.unwrap();

    assert_eq!(out.status.code(), Some(0), "changed, not failed: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("0 uploaded \u{b7} 0 failed") && text.contains("1 waiting for the next run"),
        "{text}"
    );
    {
        let v = vault.lock().unwrap();
        assert_eq!(v.abandons, 1, "the session was abandoned");
        assert!(
            !files_named(&v, "c.txt").iter().any(|(_, _, done)| *done),
            "c.txt never completed"
        );
    }

    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("1 uploaded \u{b7} 0 failed"), "{}", stdout(&out));
    let v = vault.lock().unwrap();
    let done: Vec<_> = files_named(&v, "c.txt").into_iter().filter(|(_, _, d)| *d).collect();
    assert_eq!(done.len(), 1, "c.txt backed up on the next run");
    assert_eq!(v.items[&done[0].0].size, b"after, and longer".len() as u64);
}

/// Review #2: a copied configuration (a cloned VM image, a copied home) carries another machine's device
/// folder. This machine never writes into it: the run stops and says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_copied_configuration_never_backs_up_into_the_other_machines_folder() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let inits = vault.lock().unwrap().inits;

    // The same home on another machine.
    std::fs::write(home.join("machine-id"), "fedcba9876543210fedcba9876543210\n").unwrap();
    write_file(&home, "data/new.txt", b"new");
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("made on another machine"), "{}", stderr(&out));
    assert_eq!(
        vault.lock().unwrap().inits,
        inits,
        "nothing written into the other machine's folder"
    );
}

/// A `backup.json` written before the device binding names no device id: it is bound when the device folder's
/// own manifest names this machine, and refused when it names another.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_without_a_device_id_is_checked_against_the_device_manifest() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let bound = settings(&home)["device_id"].clone();
    assert!(bound.is_string(), "the run binds the store: {bound}");

    change_settings(&home, |s| {
        s.as_object_mut().unwrap().remove("device_id");
    });
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "the manifest names this machine: {}",
        stderr(&out)
    );
    assert_eq!(settings(&home)["device_id"], bound, "bound again");

    change_settings(&home, |s| {
        s.as_object_mut().unwrap().remove("device_id");
    });
    std::fs::write(home.join("machine-id"), "fedcba9876543210fedcba9876543210\n").unwrap();
    write_file(&home, "data/new.txt", b"new");
    let inits = vault.lock().unwrap().inits;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("made on another machine"), "{}", stderr(&out));
    assert_eq!(vault.lock().unwrap().inits, inits, "nothing written");
}

/// Review #4: "back up again" of a folder that turned off for another reason, after the device folder died,
/// backs up into a new device folder, instead of turning off again on the dead one.
#[tokio::test(flavor = "multi_thread")]
async fn back_up_again_after_the_device_folder_died_takes_a_new_device_folder() {
    let (vault, api, home, env) = setup().await;
    write_file(&home, "pics/p.jpg", b"picture");
    backup(&home, &api, &env, &["add", "data"]).await;
    backup(&home, &api, &env, &["add", "pics"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let (root, device) = {
        let mut v = vault.lock().unwrap();
        let (root, device, _) = tree_ids(&v);
        let pics = the_folder(&v, Some(&device), "pics");
        for id in subtree(&v, &pics) {
            v.trashed.insert(id);
        }
        (root, device)
    };
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(4), "pics is off: {}", stderr(&out));
    {
        let mut v = vault.lock().unwrap();
        for id in subtree(&v, &device) {
            v.trashed.insert(id);
        }
    }
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(4), "data is off: {}", stderr(&out));

    let out = backup(&home, &api, &env, &["add", "pics"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "pics backs up again: {}", stdout(&out));
    let v = vault.lock().unwrap();
    let devices = live_folders(&v, &root, "nas01");
    assert_eq!(devices.len(), 1, "{devices:?}");
    assert_ne!(devices[0], device, "a new device folder");
    let pics = the_folder(&v, Some(&devices[0]), "pics");
    assert_eq!(file_names_in(&v, &pics), vec!["p.jpg".to_string()]);
}

/// Review #5: adding a folder while another is still off because an OLD device folder died keeps the live
/// device folder a later "back up again" made: every folder of this machine stays in one device folder.
#[tokio::test(flavor = "multi_thread")]
async fn adding_a_folder_keeps_the_live_device_folder() {
    let (vault, api, home, env) = setup().await;
    write_file(&home, "pics/p.jpg", b"picture");
    write_file(&home, "other/o.txt", b"other");
    backup(&home, &api, &env, &["add", "data"]).await;
    backup(&home, &api, &env, &["add", "pics"]).await;
    backup(&home, &api, &env, &["run"]).await;
    let (root, old) = {
        let mut v = vault.lock().unwrap();
        let (root, device, _) = tree_ids(&v);
        for id in subtree(&v, &device) {
            v.trashed.insert(id);
        }
        (root, device)
    };
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(4), "both off: {}", stderr(&out));
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let live = {
        let v = vault.lock().unwrap();
        let devices = live_folders(&v, &root, "nas01");
        assert_eq!(devices.len(), 1, "{devices:?}");
        assert_ne!(devices[0], old);
        devices[0].clone()
    };
    assert_eq!(settings(&home)["device_folder_id"], json!(live));

    // pics is still off (its device folder died); adding another folder keeps the live one.
    let out = backup(&home, &api, &env, &["add", "other"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        settings(&home)["device_folder_id"],
        json!(live),
        "the live device folder is kept"
    );
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    assert_eq!(
        live_folders(&v, &root, "nas01"),
        vec![live.clone()],
        "no third device folder"
    );
    let other = the_folder(&v, Some(&live), "other");
    assert_eq!(file_names_in(&v, &other), vec!["o.txt".to_string()]);
}

/// Review #6: a `.device.json` write that fails after the folders ran keeps their report, says so on a line
/// of its own, and exits 3 (something was not done; tried again next run).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_manifest_write_keeps_the_report_and_exits_incomplete() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    // Two files go up; the manifest's upload is refused.
    vault.lock().unwrap().init_status_after = Some((2, 400));
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let text = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        text.contains("2 uploaded \u{b7} 0 failed"),
        "the folder's report: {text}"
    );
    assert!(
        text.contains("could not write this machine's .device.json"),
        "the manifest's own line: {text}"
    );
    let out = backup(&home, &api, &env, &["--json", "run"]).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(&out)));
    assert_eq!(json["ok"], false);
    assert!(json["manifest_error"].is_string(), "{json}");

    vault.lock().unwrap().init_status_after = None;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "written on the next run: {}", stderr(&out));
    assert_eq!(files_named(&vault.lock().unwrap(), ".device.json").len(), 1);
}

/// Every `backup-records-<n>.jsonl` under the scratch home's state dir.
fn records_files(home: &Path) -> Vec<PathBuf> {
    fn find(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                find(&path, found);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("backup-records-") && n.ends_with(".jsonl"))
            {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    find(home, &mut found);
    found
}

/// Task 2378 (core v0.2.2, LD-0123): with the records gone (a reinstall) `bb backup` has no measured clock
/// for the listing, so core's rebuild rule trusts a server copy only when it was uploaded more than 10
/// minutes after the file last changed. Both files here were uploaded seconds after they were written, so
/// the next run uploads them again rather than mark them backed up on the device clock's word.
#[tokio::test(flavor = "multi_thread")]
async fn lost_records_upload_again_a_copy_made_within_ten_minutes_of_the_change() {
    let (vault, api, home, env) = setup().await;
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("2 uploaded"), "{}", stdout(&out));

    let lost = records_files(&home);
    assert!(!lost.is_empty(), "the pass wrote its records under the scratch home");
    for file in &lost {
        std::fs::remove_file(file).unwrap();
    }

    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("2 uploaded \u{b7} 0 failed"),
        "a copy made seconds after the change is uploaded again: {}",
        stdout(&out)
    );
    assert!(stdout(&out).contains("0 unchanged"), "{}", stdout(&out));
}

/// The other side of the same rule: a copy the server stamped more than 10 minutes after the file changed is
/// trusted, so the rebuild marks it backed up and the next run uploads nothing and adds no second row.
#[tokio::test(flavor = "multi_thread")]
async fn lost_records_trust_a_copy_made_more_than_ten_minutes_after_the_change() {
    let (vault, api, home, env) = setup().await;
    vault.lock().unwrap().uploaded_at_offset_secs = 11 * 60;
    backup(&home, &api, &env, &["add", "data"]).await;
    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("2 uploaded"), "{}", stdout(&out));

    for file in records_files(&home) {
        std::fs::remove_file(file).unwrap();
    }

    let out = backup(&home, &api, &env, &["run"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("0 uploaded \u{b7} 0 failed") && stdout(&out).contains("2 unchanged"),
        "a copy made 11 minutes after the change is trusted: {}",
        stdout(&out)
    );
    assert_eq!(files_named(&vault.lock().unwrap(), "a.txt").len(), 1, "no second row");
}
