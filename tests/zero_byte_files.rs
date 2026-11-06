//! Zero-byte files (flow "CLI end to end", flow-6, P1).
//!
//! What the flow found against a real local stack:
//! - `bb push folder2` where `folder2` = [`0-empty.txt`, `a.txt`, `z.txt`] hit the
//!   server's `file_size_bytes must be positive` reject on the empty file and
//!   stopped: `a.txt` and `z.txt` were never uploaded (`bb ls folder2` → empty).
//! - `bb sync --once` printed `! upload failed: file_size_bytes must be positive`
//!   without saying WHICH file.
//!
//! The server half (accepting a 0-byte file on `/uploads/init`) is server PR
//! #103. These tests pin the client half, against an in-process mock API:
//! - a folder push carries on past a failed file, names it, uploads the rest,
//!   and exits non-zero;
//! - against a server that accepts empty files, an empty file goes up as the
//!   canonical one-chunk plan (`plan_chunks(0)` → 1 chunk holding the 28-byte
//!   AEAD of zero bytes) and the push exits 0;
//! - `bb sync` names the file whose upload failed.
//!
//! Every run uses a scratch `HOME` / `XDG_CONFIG_HOME` (the developer's real
//! config may hold a live session) and `BB_NO_UPDATE=1`.

#[path = "support/upload_mock.rs"]
mod upload_mock;

use upload_mock::*;

#[tokio::test(flavor = "multi_thread")]
async fn folder_push_carries_on_past_a_rejected_file_and_names_it() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "push", "folder2"]).await;
    let (stdout, stderr) = text(&out);

    assert_eq!(
        *mock.completes.lock().unwrap(),
        2,
        "a.txt and z.txt must still be uploaded after 0-empty.txt is rejected\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(mock.inits.lock().unwrap().len(), 3, "all three files must be attempted");
    assert!(
        !out.status.success(),
        "a push that left a file behind must not exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("0-empty.txt") && stderr.contains("file_size_bytes must be positive"),
        "stderr must name the failed file and the reason, got: {stderr}"
    );
    assert!(
        stderr.contains("1 of 3 files failed"),
        "stderr must carry the failure count, got: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn folder_push_json_lists_the_failed_file() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2"]).await;
    let (stdout, _stderr) = text(&out);

    assert!(!out.status.success(), "must not exit 0");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert_eq!(v["total_files"], 2);
    assert_eq!(v["total_failed"], 1);
    assert_eq!(v["failed"][0]["path"], "0-empty.txt");
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_file_uploads_as_one_aead_chunk_when_the_server_accepts_it() {
    // Server PR #103 behaviour: 0-byte init accepted.
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "push", "folder2"]).await;
    let (stdout, stderr) = text(&out);

    assert!(
        out.status.success(),
        "push must succeed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(*mock.completes.lock().unwrap(), 3, "all three files complete");
    let inits = mock.inits.lock().unwrap().clone();
    let empty = inits
        .iter()
        .find(|b| b["file_size_bytes"] == 0)
        .expect("an init with file_size_bytes 0 for the empty file");
    assert_eq!(empty["chunk_count"], 1, "plan_chunks(0) is ONE chunk: {empty}");
    let sizes = mock.chunk_sizes.lock().unwrap().clone();
    // 28-byte frames: the empty file (0 + 28). a.txt is 5 + 28, z.txt 4 + 28.
    assert_eq!(sizes.len(), 3, "one chunk per file, got {sizes:?}");
    assert!(
        sizes.contains(&28),
        "the empty file's chunk is the 28-byte AEAD of b\"\": {sizes:?}"
    );
}

/// Task 2173: `bb push --json` says which route each file took (`"direct"` or `"api"`), like `bb pull --json`.
/// This mock offers no direct upload, so every file went through the API.
#[tokio::test(flavor = "multi_thread")]
async fn push_json_reports_the_transport_of_every_file() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(out.status.success(), "{stdout}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let files = v["files"].as_array().expect("files");
    assert_eq!(files.len(), 3, "{v}");
    for file in files {
        assert_eq!(file["transport"], "api", "{file}");
    }

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2/a.txt"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(out.status.success(), "{stdout}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    assert_eq!(v["files"][0]["transport"], "api", "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_names_the_file_whose_upload_failed() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("empty.txt"), b"").unwrap();

    let out = bb(
        &home,
        &["--api", &api, "sync", "--once", local.to_str().unwrap(), "/SyncUp"],
    )
    .await;
    let (_stdout, stderr) = text(&out);

    assert!(
        !mock.inits.lock().unwrap().is_empty(),
        "the upload must have been attempted"
    );
    assert!(
        stderr.contains("upload failed: empty.txt: file_size_bytes must be positive"),
        "the failure must name the file, got: {stderr}"
    );
}

/// Task 2333 (spec §7, §9.5): `bb --json push <file>` carries the run's `transfer` object.
#[tokio::test(flavor = "multi_thread")]
async fn push_json_reports_transfer_stats() {
    let mock = Mock {
        init_extra: Some(json!({
            "storage_pool_id": "3e15382b-0000-4000-8000-000000000001",
            "storage_location": {"country": "Germany", "country_code": "DE"},
        })),
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2/a.txt"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(out.status.success(), "{stdout}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let t = &v["transfer"];
    assert_eq!(t["direction"], "upload", "{v}");
    assert_eq!(
        (t["files_done"].clone(), t["files_total"].clone()),
        (json!(1), json!(1)),
        "{t}"
    );
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(1), json!(1)),
        "{t}"
    );
    assert_eq!(t["chunks_already_stored"], 0, "{t}");
    assert_eq!(t["bytes_transferred"], v["total_bytes"], "{v}");
    assert_eq!(t["bytes_done"], t["bytes_total"], "{t}");
    assert_eq!(t["peak_chunks_in_flight"], 1, "{t}");
    assert_eq!(t["max_chunks_in_flight"], 1, "{t}");
    assert!(t["avg_mbps"].is_number(), "{t}");
    assert_eq!(t["routes"], json!({"direct": 0, "api": 1}), "{t}");
    assert_eq!(t["retries"], 0, "{t}");
    assert_eq!(
        t["pools"],
        json!([{"id": "3e15382b-0000-4000-8000-000000000001", "country": "Germany", "country_code": "DE", "chunks": 1}]),
        "{t}"
    );
    // The existing keys keep their names and meaning.
    assert_eq!(v["total_files"], 1, "{v}");
    assert_eq!(v["files"][0]["transport"], "api", "{v}");
}

/// Task 2333 (spec §9.5): a folder's `transfer` sums every file's plan.
#[tokio::test(flavor = "multi_thread")]
async fn push_json_folder_sums_chunks() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(out.status.success(), "{stdout}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let t = &v["transfer"];
    assert_eq!(
        (t["files_done"].clone(), t["files_total"].clone()),
        (json!(3), json!(3)),
        "{t}"
    );
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(3), json!(3)),
        "{t}"
    );
    assert_eq!(t["bytes_done"], t["bytes_total"], "{t}");
    assert_eq!(t["bytes_done"], v["total_bytes"], "{v}");
    assert_eq!(t["routes"]["api"], 3, "{t}");
}

/// Review Focus 2: a zero-byte file is one 28-byte chunk; its average is a number or null, never a string.
#[tokio::test(flavor = "multi_thread")]
async fn push_json_empty_file_counts_one_chunk() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2/0-empty.txt"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(out.status.success(), "{stdout}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let t = &v["transfer"];
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(1), json!(1)),
        "{t}"
    );
    assert_eq!(
        (t["bytes_done"].clone(), t["bytes_total"].clone()),
        (json!(28), json!(28)),
        "{t}"
    );
    assert!(t["avg_mbps"].is_number() || t["avg_mbps"].is_null(), "{t}");
}
