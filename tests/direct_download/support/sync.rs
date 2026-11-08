//! One behaviour group of the same integration suite.

use super::*;

#[test]
fn a_sync_pass_downloads_over_the_direct_transport() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, dir) = sync_once(&env, &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(dir.join("multi.bin")).unwrap_or_default(),
        env.mock.fx.plaintext
    );
    let log = env.mock.log.lock().unwrap();
    assert!(
        !log.grant_bodies.is_empty() && log.api_download_hits == 0,
        "it went direct"
    );
}

#[test]
fn a_sync_pass_survives_a_file_that_never_authenticates() {
    // The chunk fails AEAD from storage and again from the pinned API route: the direct driver's
    // `Corrupt`. The API path treats a file it cannot decrypt as "unreadable, retry next pass"
    // (task 1916); the direct path must too. If this ended the pass, launchd and systemd would
    // restart the daemon every 30 s for ever and the files after it would never arrive.
    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[2]);
    let (watching, stdout, stderr) = sync_until_watching(&env);
    assert!(
        watching,
        "the daemon must reach watch mode\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let dir = env.cwd.join("syncdir");
    assert!(
        !dir.join("multi.bin").exists(),
        "nothing is published for a file that did not authenticate"
    );
    sync_left_nothing(&dir);
    assert!(
        stderr.contains("did not authenticate") && stderr.contains("next pass"),
        "{stderr}"
    );
    assert!(!stderr.contains("sync incomplete"), "{stderr}");

    // The one-shot form reports the incomplete pass with its own exit code (3), not as an error (1).
    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[2]);
    let (out, dir) = sync_once(&env, &[]);
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 failed"),
        "{}",
        describe(&out)
    );
    assert!(!dir.join("multi.bin").exists());
    assert_clean(&env, &out);
}

#[test]
fn a_sync_pass_survives_a_download_that_ends_before_its_last_frame() {
    // Every frame authenticates, but the stream never reaches the frame marked last: the copy is cut
    // short or tampered with. That ends THIS file's turn (retry next pass), not the pass.
    let env = setup_with(
        fixture_v2_missing_last_frame(),
        Discovery::Offer,
        Grants::Normal,
        &[],
        &[],
    );
    let (watching, stdout, stderr) = sync_until_watching(&env);
    assert!(
        watching,
        "the daemon must reach watch mode\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    sync_left_nothing(&env.cwd.join("syncdir"));
    assert!(
        stderr.contains("download incomplete") && stderr.contains("next pass"),
        "{stderr}"
    );
}

#[test]
fn a_sync_pass_defers_a_file_whose_version_changed_under_it() {
    let env = setup(Discovery::Offer, Grants::VersionGoneAfterFirstWindow, &[], &[]);
    let (watching, stdout, stderr) = sync_until_watching(&env);
    assert!(
        watching,
        "the daemon must reach watch mode\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let dir = env.cwd.join("syncdir");
    assert!(!dir.join("multi.bin").exists());
    sync_left_nothing(&dir);
    assert!(stderr.contains("changed") && stderr.contains("next pass"), "{stderr}");
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0, "never mix versions");

    // Deferred is not a failure: a one-shot pass that only deferred it is complete.
    let env = setup(Discovery::Offer, Grants::VersionGoneAfterFirstWindow, &[], &[]);
    let (out, dir) = sync_once(&env, &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(!dir.join("multi.bin").exists());
}
