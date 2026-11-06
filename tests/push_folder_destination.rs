//! Task 2334: `bb push <folder>` puts everything where the command asked.
//!
//! Two defects, found on a real push of a movie folder (Guus, 2026-11-06):
//!
//! 1. Re-pushing a folder made a SECOND folder with the same name, at the top
//!    level and at every level under it: `push_directory` created each folder
//!    anew and never looked for an existing one.
//! 2. A file landed outside the folder it was pushed into. An earlier push of
//!    the same local file, interrupted, left a resume record keyed by the local
//!    path only. The re-run, into another folder, continued that session. The
//!    server fixes a file's parent when the upload is INITED and keeps it at
//!    completion, so the file completed in the earlier attempt's folder.
//!
//! Black box: the real `bb` binary against an in-process mock that, like the
//! server, keeps folders by parent, creates a file's row (with its parent) at
//! init, never changes that parent at completion, and lists a folder's
//! children. Scratch `HOME`, stdin not a terminal (a pty where a test answers
//! a question), loopback only.
//!
//! The fix reuses a same-name folder only when it is not shared on its own,
//! unless the user says yes at the terminal or passes `--reuse-shared` (lead
//! ruling 5, 2026-11-06); those branches are tested here too.

#[path = "support/push_vault.rs"]
mod vault;
use vault::*;

/// Pushing the same folder into the same place twice leaves ONE folder of that
/// name there, holding what both pushes uploaded. Before the fix,
/// `push_directory` created the pushed folder anew on every run.
#[tokio::test(flavor = "multi_thread")]
async fn repushing_a_folder_reuses_the_same_name_folder() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "first push: {}", stderr(&out));
    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "second push: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let pushed = folders_named(&v, Some(&movies), MOVIE_DIR);
    assert_eq!(
        pushed.len(),
        1,
        "a re-push must reuse the existing {MOVIE_DIR:?} in Movies, not make another: {pushed:?}"
    );
    assert_eq!(
        file_names_in(&v, &pushed[0]),
        vec![
            "A House of Dynamite (2025) (2160p) (1).mkv".to_string(),
            MOVIE.to_string()
        ],
        "both pushes' files are in that one folder (the second kept both, per --keep-both)"
    );
}

/// The same at every level under the pushed folder: a re-push that adds a new
/// episode puts it next to the first, in the ONE existing season folder.
#[tokio::test(flavor = "multi_thread")]
async fn repushing_a_folder_reuses_its_same_name_subfolders() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "Show/Season 1/e01.mkv", b"episode one");

    let out = bb(&home, &["--api", &api, "push", "--folder", "Shows", "Show"]).await;
    assert_eq!(out.status.code(), Some(0), "first push: {}", stderr(&out));
    write_file(&home, "Show/Season 1/e02.mkv", b"episode two");
    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Shows", "--keep-both", "Show"],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "second push: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let seasons = folders_named_anywhere(&v, "Season 1");
    assert_eq!(
        seasons.len(),
        1,
        "a re-push must reuse the existing nested \"Season 1\", not make another: {seasons:?}"
    );
    let names = file_names_in(&v, &seasons[0]);
    assert!(
        names.contains(&"e01.mkv".to_string()) && names.contains(&"e02.mkv".to_string()),
        "both episodes are in the one season folder: {names:?}"
    );
}

// ── Defect 2: a resumed upload lands in the earlier attempt's folder ────────

/// Guus's sequence: a push of the folder (no `--folder`) is interrupted, then
/// the same folder is pushed with `--folder Movies`. The file must complete in
/// Movies/<folder>, the folder THIS command asked for. Before the fix the
/// re-run continued the first attempt's session, whose row the server had
/// created under the first attempt's folder at the root, and the file landed
/// there.
#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_upload_lands_in_the_folder_this_push_asked_for() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), &two_chunks(1));

    push_and_kill(&vault, &home, &["--api", &api, "push", MOVIE_DIR]).await;
    let first_folder = {
        let v = vault.lock().unwrap();
        let first_folder = the_folder(&v, None, MOVIE_DIR);
        let files = files_named(&v, MOVIE);
        assert_eq!(files.len(), 1, "the interrupted upload's row: {files:?}");
        assert_eq!(files[0].1.as_deref(), Some(first_folder.as_str()));
        assert!(!files[0].2, "nothing completed before the kill");
        first_folder
    };

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "the push into Movies: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let wanted = the_folder(&v, Some(&movies), MOVIE_DIR);
    let completed: Vec<_> = files_named(&v, MOVIE).into_iter().filter(|f| f.2).collect();
    assert_eq!(completed.len(), 1, "one completed copy of the movie: {completed:?}");
    assert_eq!(
        completed[0].1.as_deref(),
        Some(wanted.as_str()),
        "the movie must complete in Movies/{MOVIE_DIR} ({wanted}), the folder this push asked for, \
         not in the interrupted attempt's {MOVIE_DIR} at the root ({first_folder})"
    );
    assert!(
        file_names_in(&v, &first_folder).is_empty(),
        "nothing is left in the first attempt's folder, not even an unfinished upload"
    );
}

/// The guard for the fix above: a re-run of an interrupted folder push into the
/// SAME place still resumes the upload (core's `resume_policy`), in the one
/// folder of that name. A fix that restarted every resumed upload, or that
/// made a new folder for the re-run, fails here.
#[tokio::test(flavor = "multi_thread")]
async fn rerunning_an_interrupted_folder_push_resumes_into_the_same_folder() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), &two_chunks(2));

    push_and_kill(&vault, &home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    let interrupted_id = {
        let v = vault.lock().unwrap();
        let files = files_named(&v, MOVIE);
        assert_eq!(files.len(), 1, "the interrupted upload's row: {files:?}");
        files[0].0.clone()
    };

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "the re-run: {}", stderr(&out));

    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let folder = the_folder(&v, Some(&movies), MOVIE_DIR);
    let files = files_named(&v, MOVIE);
    assert_eq!(files.len(), 1, "one movie, no copy: {files:?}");
    assert_eq!(files[0].0, interrupted_id, "the interrupted upload itself completed");
    assert_eq!(files[0].1.as_deref(), Some(folder.as_str()), "in the one {MOVIE_DIR}");
    assert!(files[0].2, "completed");
    let writing = chunk_format_for_pin(ExpectedChunkFormat::Unrecorded);
    match resume_policy(Some(writing), writing) {
        SessionResume::ContinueSession => {
            assert_eq!(v.inits, 1, "the stored session is continued, not re-inited");
            assert_eq!(v.abandons, 0);
            assert!(v.skipped >= 1, "chunk 0 the server already held is skipped");
        }
        SessionResume::RestartWithFreshSession => {
            assert_eq!(v.abandons, 1, "the old session is abandoned");
            assert_eq!(v.inits, 2, "and the same file is inited again from chunk 0");
        }
    }
}

// ── Several same-name folders: the oldest ───────────────────────────────────

/// Guus has two "A House of Dynamite (2025)" folders. A push picks the oldest
/// (at the `--folder` level too), and says how many there are.
#[tokio::test(flavor = "multi_thread")]
async fn of_several_same_name_folders_the_oldest_is_used() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let movies = seed_folder(&vault, None, "Movies");
    seed_folder(&vault, None, "Movies");
    let oldest = seed_folder(&vault, Some(&movies), MOVIE_DIR);
    let newer = seed_folder(&vault, Some(&movies), MOVIE_DIR);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("2 folders named {MOVIE_DIR} are here; using the oldest")),
        "{}",
        stderr(&out)
    );

    let v = vault.lock().unwrap();
    assert_eq!(file_names_in(&v, &oldest), vec![MOVIE.to_string()], "into the oldest");
    assert!(file_names_in(&v, &newer).is_empty());
    assert_eq!(folders_named(&v, Some(&movies), MOVIE_DIR).len(), 2, "no third one");
    assert_eq!(folders_named(&v, None, "Movies").len(), 2, "no third Movies");
}

// ── A same-name folder that is shared: never reused silently ────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_same_name_folder_is_not_reused_when_no_one_can_answer() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let (movies, shared) = pushed_then_shared(&vault, &api, &home).await;
    let inits_before = vault.lock().unwrap().inits;

    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(2),
        "a question no one can answer: {}",
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(
        err.contains(&format!("{MOVIE_DIR} already exists here and is shared with 1 person")),
        "{err}"
    );
    assert!(
        err.contains("--reuse-shared") && err.contains("stdin is not a terminal"),
        "{err}"
    );

    let v = vault.lock().unwrap();
    assert_eq!(v.inits, inits_before, "nothing is uploaded");
    assert_eq!(
        folders_named(&v, Some(&movies), MOVIE_DIR),
        vec![shared.clone()],
        "and no folder made"
    );
    assert_eq!(
        file_names_in(&v, &shared),
        vec![MOVIE.to_string()],
        "the shared folder is untouched"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reuse_shared_uploads_into_the_shared_folder() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let (movies, shared) = pushed_then_shared(&vault, &api, &home).await;

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "push",
            "--folder",
            "Movies",
            "--keep-both",
            "--reuse-shared",
            MOVIE_DIR,
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("{MOVIE_DIR} is shared with 1 person; uploading into it")),
        "{}",
        stderr(&out)
    );

    let v = vault.lock().unwrap();
    assert_eq!(folders_named(&v, Some(&movies), MOVIE_DIR), vec![shared.clone()]);
    assert!(file_names_in(&v, &shared).contains(&"extras.mkv".to_string()));
}

/// A subfolder shared on its own is asked about too, even inside a private
/// top folder: reusing it would show new files to its own members.
#[tokio::test(flavor = "multi_thread")]
async fn a_subfolder_shared_on_its_own_is_not_reused_when_no_one_can_answer() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, "Show/Season 1/e01.mkv", b"episode one");
    let out = bb(&home, &["--api", &api, "push", "--folder", "Shows", "Show"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let season = folders_named_anywhere(&vault.lock().unwrap(), "Season 1").remove(0);
    share_with_someone(&vault, &season);
    write_file(&home, "Show/Season 1/e02.mkv", b"episode two");
    write_file(&home, "Show/0-cover.jpg", b"a cover"); // sorts before "Season 1"
    let inits_before = vault.lock().unwrap().inits;

    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Shows", "--keep-both", "Show"],
    )
    .await;
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Show/Season 1 already exists here and is shared with 1 person"),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(file_names_in(&v, &season), vec!["e01.mkv".to_string()]);
    assert_eq!(
        v.inits, inits_before,
        "every folder is settled before any file is uploaded: nothing went up, not even Show/0-cover.jpg, which sorts first"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn at_a_terminal_yes_uploads_into_the_shared_folder() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let (movies, shared) = pushed_then_shared(&vault, &api, &home).await;

    let run = bb_at_terminal(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
        "y",
    )
    .await;
    assert_eq!(run.code, Some(0), "{run:?}");
    assert!(run.stderr.contains("[y/N]"), "it asked: {run:?}");

    let v = vault.lock().unwrap();
    assert_eq!(folders_named(&v, Some(&movies), MOVIE_DIR), vec![shared.clone()]);
    assert!(
        file_names_in(&v, &shared).contains(&"extras.mkv".to_string()),
        "{run:?}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn at_a_terminal_no_makes_a_new_folder_and_says_so() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let (movies, shared) = pushed_then_shared(&vault, &api, &home).await;

    // Enter alone is the default: No.
    let run = bb_at_terminal(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
        "",
    )
    .await;
    assert_eq!(run.code, Some(0), "{run:?}");
    assert!(
        run.stderr.contains(&format!(
            "made a new folder {MOVIE_DIR}; the shared one is left as it was"
        )),
        "{run:?}"
    );

    let new = {
        let v = vault.lock().unwrap();
        let both = folders_named(&v, Some(&movies), MOVIE_DIR);
        assert_eq!(both.len(), 2, "the shared one and a new one: {both:?}");
        let new = both.iter().find(|id| **id != shared).unwrap().clone();
        assert_eq!(
            file_names_in(&v, &shared),
            vec![MOVIE.to_string()],
            "the shared folder is untouched"
        );
        assert_eq!(
            file_names_in(&v, &new),
            vec![MOVIE.to_string(), "extras.mkv".to_string()],
            "everything went into the new folder"
        );
        new
    };

    // The "no" stays a no: the next run, with no one to ask, writes into the
    // private new folder instead of asking again or making a third.
    write_file(&home, &format!("{MOVIE_DIR}/trailer.mkv"), b"trailer");
    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!(
            "2 folders named {MOVIE_DIR} are here; using the oldest one that is not shared"
        )),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(folders_named(&v, Some(&movies), MOVIE_DIR).len(), 2, "no third one");
    assert!(file_names_in(&v, &new).contains(&"trailer.mkv".to_string()));
    assert_eq!(
        file_names_in(&v, &shared),
        vec![MOVIE.to_string()],
        "the shared folder is untouched"
    );
}

// ── Review round (code-review high, 2026-11-06) ────────────────────────────

/// A re-push with no conflict flag follows `bb push`'s conflict rule for every
/// file already in the reused folder: with no one to ask, each is refused by
/// name, nothing is uploaded again, and the command fails.
#[tokio::test(flavor = "multi_thread")]
async fn a_repush_without_a_conflict_flag_refuses_each_existing_file() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");
    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let inits_before = vault.lock().unwrap().inits;

    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_ne!(out.status.code(), Some(0), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.contains("already exists") && err.contains("--replace or --keep-both"),
        "{err}"
    );
    assert!(
        err.contains(&format!("1 of 1 files failed to upload: {MOVIE}")),
        "{err}"
    );
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, inits_before, "nothing is uploaded again");
    let movies = the_folder(&v, None, "Movies");
    the_folder(&v, Some(&movies), MOVIE_DIR);
}

/// A conflict check that cannot be read fails that file; it is never read as
/// "no file of that name" (which would upload a second one beside it).
#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_conflict_check_fails_the_file_instead_of_duplicating_it() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");
    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let inits_before = vault.lock().unwrap().inits;
    vault.lock().unwrap().conflict_status = Some(503);

    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_ne!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("could not check whether {MOVIE} already exists")),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, inits_before, "no second copy");
    assert_eq!(files_named(&v, MOVIE).len(), 1);
}

/// With `--reuse-shared` the user already chose: a share list that cannot be
/// read does not stop the push.
#[tokio::test(flavor = "multi_thread")]
async fn reuse_shared_does_not_need_the_share_lists() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let (movies, shared) = pushed_then_shared(&vault, &api, &home).await;
    vault.lock().unwrap().invites_status = Some(500);

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "push",
            "--folder",
            "Movies",
            "--keep-both",
            "--reuse-shared",
            MOVIE_DIR,
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    // Not silent: the user is told the check failed, and the owner-only notice checks this folder.
    assert!(
        stderr(&out).contains(&format!("could not check whether {MOVIE_DIR} is shared"))
            && stderr(&out).contains("uploading into it (--reuse-shared)"),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(folders_named(&v, Some(&movies), MOVIE_DIR), vec![shared.clone()]);
    assert!(file_names_in(&v, &shared).contains(&"extras.mkv".to_string()));
}

/// Without `--reuse-shared`, a share list that cannot be read stops the push
/// before anything is uploaded: a folder is never reused blind.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_share_list_stops_the_push_before_any_upload() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let (_, shared) = pushed_then_shared(&vault, &api, &home).await;
    vault.lock().unwrap().invites_status = Some(500);
    let inits_before = vault.lock().unwrap().inits;

    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_ne!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("could not check whether {MOVIE_DIR} is shared")),
        "{}",
        stderr(&out)
    );
    let v = vault.lock().unwrap();
    assert_eq!(v.inits, inits_before);
    assert_eq!(file_names_in(&v, &shared), vec![MOVIE.to_string()]);
}

/// A local subfolder name that is not UTF-8 is stored as "folder" (as before
/// 2334). It must get a folder of its own, never be merged into an unrelated
/// existing "folder" in the reused parent. Linux only: macOS refuses such
/// names. (`bb` takes no non-UTF-8 path argument at all, so the pushed folder
/// itself has a UTF-8 name.)
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_subfolder_name_that_is_not_utf8_never_merges_into_an_existing_one() {
    use std::os::unix::ffi::OsStrExt;

    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    let target = seed_folder(&vault, None, "Target");
    let album = seed_folder(&vault, Some(&target), "Album");
    let unrelated = seed_folder(&vault, Some(&album), "folder");
    let latin1 = home.join("Album").join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
    std::fs::create_dir_all(&latin1).unwrap();
    std::fs::write(latin1.join("a.txt"), b"a").unwrap();

    let out = bb(&home, &["--api", &api, "push", "--folder", "Target", "Album"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let v = vault.lock().unwrap();
    assert_eq!(
        the_folder(&v, Some(&target), "Album"),
        album,
        "the private Album is reused"
    );
    assert!(
        file_names_in(&v, &unrelated).is_empty(),
        "nothing was merged into the unrelated folder"
    );
    let folders = folders_named(&v, Some(&album), "folder");
    assert_eq!(folders.len(), 2, "the push made its own: {folders:?}");
}

/// "Not supported" is unknown, never private (lead ruling, 2026-11-06): a
/// server with no invite route cannot say who was invited, and the listing's
/// `share_count` (public links only) cannot either, so even `share_count: 0`
/// leaves the folder counted as shared.
#[tokio::test(flavor = "multi_thread")]
async fn a_folder_the_server_cannot_say_is_private_counts_as_shared() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");
    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    {
        let mut v = vault.lock().unwrap();
        v.shares_unsupported = true;
        v.listing_share_count = true;
    }
    write_file(&home, &format!("{MOVIE_DIR}/extras.mkv"), b"extras");
    let inits_before = vault.lock().unwrap().inits;

    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!(
            "{MOVIE_DIR} already exists here and may be shared: this server does not say with whom"
        )),
        "{}",
        stderr(&out)
    );
    assert_eq!(vault.lock().unwrap().inits, inits_before, "nothing is uploaded");

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "push",
            "--folder",
            "Movies",
            "--keep-both",
            "--reuse-shared",
            MOVIE_DIR,
        ],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let folder = the_folder(&v, Some(&movies), MOVIE_DIR);
    assert!(file_names_in(&v, &folder).contains(&"extras.mkv".to_string()));
}

/// Shared folders switched off for the account (folder links answer 501):
/// none of its folder links is served then, so the listing's `share_count`
/// is the link exposure, and 0 with no invite reuses the folder silently.
#[tokio::test(flavor = "multi_thread")]
async fn with_shared_folders_off_the_listings_share_count_decides() {
    let vault: Shared = Arc::default();
    let api = start_mock(vault.clone()).await;
    let home = scratch_home(&api);
    write_file(&home, &format!("{MOVIE_DIR}/{MOVIE}"), b"one movie");
    let out = bb(&home, &["--api", &api, "push", "--folder", "Movies", MOVIE_DIR]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    {
        let mut v = vault.lock().unwrap();
        v.folder_links_off = true;
        v.listing_share_count = true;
    }
    write_file(&home, &format!("{MOVIE_DIR}/extras.mkv"), b"extras");

    let out = bb(
        &home,
        &["--api", &api, "push", "--folder", "Movies", "--keep-both", MOVIE_DIR],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = vault.lock().unwrap();
    let movies = the_folder(&v, None, "Movies");
    let folder = the_folder(&v, Some(&movies), MOVIE_DIR);
    assert!(file_names_in(&v, &folder).contains(&"extras.mkv".to_string()));
}
