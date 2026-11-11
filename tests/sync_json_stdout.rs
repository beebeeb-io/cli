//! LD-0135: `bb --json sync` prints exactly one JSON document on stdout. The `+ remote folder X` line (a sync that
//! creates a remote folder) and its `--dry-run` twin (`would create remote folder`) went to stdout first, so
//! `bb --json sync | jq` failed on the line. Without `--json` the lines stay where they were.
//!
//! Black box: the real `bb` binary against the in-process mock of `support/upload_mock.rs` (its listing knows
//! one remote folder, `/SyncUp`; any other remote folder is created).

#[path = "support/upload_mock.rs"]
pub mod upload_mock;

use upload_mock::*;

/// `bb <flags> sync --once <local> <remote>` in a scratch home; returns (stdout, stderr). `local` gets one
/// file in a subfolder, so the pass creates `sub` as a remote folder.
async fn run_sync(flags: &[&str], sync_flags: &[&str], remote: &str) -> (String, String) {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(local.join("sub")).unwrap();
    std::fs::write(local.join("sub/a.txt"), b"hello").unwrap();

    let mut args = vec!["--api", api.as_str()];
    args.extend_from_slice(flags);
    args.push("sync");
    args.extend_from_slice(sync_flags);
    args.extend_from_slice(&["--once", local.to_str().unwrap(), remote]);
    let out = bb(&home, &args).await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    (stdout, stderr)
}

/// All of stdout is one JSON document (not a search for the first `{`).
fn assert_one_document(stdout: &str, stderr: &str) -> Value {
    serde_json::from_slice(stdout.as_bytes())
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {stdout:?}\nstderr: {stderr:?}"))
}

/// The case that creates the folder the user named: `bb --json sync` into `/BrandNew`, which does not exist yet.
#[tokio::test(flavor = "multi_thread")]
async fn json_sync_into_a_new_remote_folder_is_one_document() {
    let (stdout, stderr) = run_sync(&["--json"], &[], "/BrandNew").await;
    assert_one_document(&stdout, &stderr);
    assert!(
        stderr.contains("remote folder") && stderr.contains("BrandNew"),
        "the line moved to stderr, it was not dropped: {stderr:?}"
    );
}

/// The case that creates a folder inside the synced one (`sub`), run into the existing `/SyncUp`.
#[tokio::test(flavor = "multi_thread")]
async fn json_sync_creating_a_subfolder_is_one_document() {
    let (stdout, stderr) = run_sync(&["--json"], &[], "/SyncUp").await;
    assert_one_document(&stdout, &stderr);
    assert!(stderr.contains("remote folder") && stderr.contains("sub"), "{stderr:?}");
}

/// `--dry-run`: "would create remote folder" is not on stdout either.
#[tokio::test(flavor = "multi_thread")]
async fn json_dry_run_sync_is_one_document() {
    let (stdout, stderr) = run_sync(&["--json"], &["--dry-run"], "/SyncUp").await;
    assert_one_document(&stdout, &stderr);
    assert!(
        stderr.contains("would create remote folder") && stderr.contains("sub"),
        "{stderr:?}"
    );
}

/// Without `--json` the human output is unchanged: the lines are on stdout, none on stderr.
#[tokio::test(flavor = "multi_thread")]
async fn human_sync_keeps_the_folder_lines_on_stdout() {
    let (stdout, stderr) = run_sync(&[], &[], "/BrandNew").await;
    assert!(stdout.contains("+ remote folder BrandNew"), "{stdout:?}");
    assert!(stdout.contains("+ remote folder sub"), "{stdout:?}");
    assert!(!stderr.contains("remote folder"), "{stderr:?}");

    let (stdout, stderr) = run_sync(&[], &["--dry-run"], "/SyncUp").await;
    assert!(stdout.contains("+ would create remote folder sub"), "{stdout:?}");
    assert!(!stderr.contains("remote folder"), "{stderr:?}");
}
