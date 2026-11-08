//! One behaviour group of the same integration suite.

use super::*;

#[test]
fn a_version_that_goes_away_mid_transfer_restarts_and_publishes_nothing() {
    let env = setup(Discovery::Offer, Grants::VersionGoneAfterFirstWindow, &[], &[]);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("changed again"),
        "{}",
        describe(&out)
    );
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0, "never mix versions");
}

#[test]
fn a_version_published_mid_download_is_read_again_from_fresh_metadata_once() {
    let env = setup(Discovery::Offer, Grants::VersionGoneOnce, &[], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("starting again"),
        "{}",
        describe(&out)
    );
    let starts: Vec<u64> = env
        .mock
        .log
        .lock()
        .unwrap()
        .grant_bodies
        .iter()
        .map(|b| b["start_index"].as_u64().unwrap())
        .collect();
    // The first attempt: window 0, window 8 (refused: the version is gone). The second starts over.
    assert_eq!(starts, vec![0, 8, 0, 8, 16], "{starts:?}");
}

#[test]
fn a_folder_pull_reads_one_changed_file_again_and_carries_on() {
    // A new version appears while a file of the folder downloads: that file is read again from
    // fresh metadata, once; the folder pull is not aborted.
    let env = setup(Discovery::Offer, Grants::VersionGoneOnce, &[], &[]);
    let out_dir = env.cwd.join("pulled");
    let out = bb(&env, &["pull", SYNC_FOLDER_ID, "-o", out_dir.to_str().unwrap()], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(out_dir.join("multi.bin")).unwrap_or_default(),
        env.mock.fx.plaintext
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("reading it again"),
        "{}",
        describe(&out)
    );
    let starts: Vec<u64> = env
        .mock
        .log
        .lock()
        .unwrap()
        .grant_bodies
        .iter()
        .map(|b| b["start_index"].as_u64().unwrap())
        .collect();
    assert_eq!(starts, vec![0, 8, 0, 8, 16], "{starts:?}");
}

#[test]
fn a_folder_pull_does_not_swallow_a_failure_that_is_not_a_version_change() {
    // The grant route refuses (403): that is not "the version changed". The folder pull must fail,
    // exit non-zero and name the file, as it always did; skipping it silently and exiting 0 would
    // make an incomplete folder look complete.
    let env = setup(Discovery::Offer, Grants::Forbidden, &[], &[]);
    let out_dir = env.cwd.join("pulled");
    let out = bb(&env, &["pull", SYNC_FOLDER_ID, "-o", out_dir.to_str().unwrap()], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("403") && stderr.contains("multi.bin"),
        "{}",
        describe(&out)
    );
    assert!(!out_dir.join("multi.bin").exists());
}

#[test]
fn a_v2_stream_that_ends_before_its_last_frame_is_never_published() {
    let env = setup_with(
        fixture_v2_missing_last_frame(),
        Discovery::Offer,
        Grants::Normal,
        &[],
        &[],
    );
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert_clean(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("incomplete"),
        "{}",
        describe(&out)
    );
    // Control: the same fixture served whole would have decrypted; the failure above is the
    // missing last frame, not a broken fixture.
    assert_eq!(env.mock.log.lock().unwrap().storage.len(), 3);
}
