//! One behaviour group of the same integration suite.

use super::*;

#[tokio::test]
async fn a_resumed_direct_session_continues_direct_and_skips_what_the_server_holds() {
    let scratch = Scratch::new("resume-direct");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let state = shared(|_| {});
    let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[0]);
    run(&state, spec(&path, file_id)).await.unwrap();
    snapshot(&state, |m| {
        assert!(m.inits.is_empty(), "the recorded session is reused, not re-opened");
        assert_eq!(m.grants.len() as u32, chunks, "every chunk asks for a grant");
        assert_eq!(
            m.storage_puts.len() as u32,
            chunks - 1,
            "chunk 0 is skipped: nothing is sent for it"
        );
        assert_eq!(m.commits.len() as u32, chunks - 1);
        assert!(m.proxy_puts.is_empty() && m.abandons.is_empty());
        assert_eq!(m.completed.len(), 1);
    });
}

#[tokio::test]
async fn a_resumed_api_session_stays_on_the_api_even_when_the_server_now_offers_direct() {
    let scratch = Scratch::new("resume-api");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let state = shared(|_| {});
    let (file_id, _) = resumed_session(&state, &path, size, mtime, false, &[]);
    run(&state, spec(&path, file_id)).await.unwrap();
    snapshot(&state, |m| {
        assert!(m.inits.is_empty());
        assert_eq!(
            m.discovery_hits, 0,
            "an API session has no use for the capabilities, so the route is not asked"
        );
        assert!(
            m.grants.is_empty() && m.storage_puts.is_empty(),
            "an API session takes no grant"
        );
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert_eq!(m.completed.len(), 1);
    });
}

#[tokio::test]
async fn a_resumed_direct_session_is_given_up_when_direct_is_no_longer_offered() {
    let scratch = Scratch::new("resume-off");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.offers = Some(Discovery::UploadOff));
    let (file_id, old_sid) = resumed_session(&state, &path, size, mtime, true, &[0]);
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
    snapshot(&state, |m| {
        assert_eq!(
            m.abandons,
            vec![file_id.to_string()],
            "the direct session is ended, not PUT to"
        );
        assert_eq!(m.inits.len(), 1, "a fresh API session is opened");
        assert_eq!(m.init_headers, vec![None]);
        assert!(m.proxy_puts.iter().all(|(sid, _, _)| *sid != old_sid.to_string()));
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert_eq!(m.completed.len(), 1);
    });
    assert_eq!(*progress.0.lock().unwrap() as u64, outcome.ciphertext_bytes);
}

#[tokio::test]
async fn a_resumed_direct_session_the_server_swept_is_started_again_as_a_direct_one() {
    let scratch = Scratch::new("swept-grant");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let state = shared(|_| {});
    let file_id = swept_resumed_session(&state, &path, size, mtime);
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
    assert_eq!(outcome.transport, "direct");
    snapshot(&state, |m| {
        assert_eq!(m.inits.len(), 1, "one new session for the file");
        assert_eq!(
            m.init_headers,
            vec![Some(CAP.to_string())],
            "and it asks for the direct upload again"
        );
        assert_eq!(
            m.grants.len() as u32,
            chunks + 1,
            "the refused grant, then one per chunk"
        );
        assert_eq!(m.storage_puts.len() as u32, chunks);
        assert_eq!(m.commits.len() as u32, chunks);
        assert!(m.proxy_puts.is_empty() && m.abandons.is_empty());
        assert_eq!(m.completed.len(), 1);
    });
    assert_eq!(
        *progress.0.lock().unwrap() as u64,
        outcome.ciphertext_bytes,
        "the file is counted once"
    );
}

#[tokio::test]
async fn a_commit_that_finds_the_session_gone_starts_the_file_again_too() {
    let scratch = Scratch::new("swept-commit");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let gone = json!({"error": "not found"});
    let state = shared(|m| m.commit_script = VecDeque::from([Commit::Reply(404, gone, None)]));
    let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[]);
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
    assert_eq!(outcome.transport, "direct");
    snapshot(&state, |m| {
        assert_eq!(
            m.inits.len(),
            1,
            "a gone session is not committed to again: the file starts over"
        );
        assert_eq!(m.storage_puts.len() as u32, chunks + 1, "chunk 0 went to storage twice");
        assert_eq!(
            m.commits.len() as u32,
            chunks + 1,
            "the refused commit, then one per chunk"
        );
        assert_eq!(m.completed.len(), 1);
    });
    assert_eq!(
        *progress.0.lock().unwrap() as u64,
        outcome.ciphertext_bytes,
        "the file is counted once"
    );
}

#[tokio::test]
async fn the_servers_expired_session_400_is_a_gone_session_not_a_reason_to_leave_the_direct_upload() {
    let scratch = Scratch::new("expired");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    // The server's text for a session past its lease (`routes/direct_transfer.rs:579`).
    let expired = json!({"error": "upload session is not writable: expired"});
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(400, expired, None)]));
    let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[]);
    let outcome = run(&state, spec(&path, file_id)).await.unwrap();
    assert_eq!(outcome.transport, "direct", "a new direct session, not the API");
    snapshot(&state, |m| {
        assert_eq!(m.inits.len(), 1);
        assert_eq!(m.init_headers, vec![Some(CAP.to_string())]);
        assert!(m.abandons.is_empty() && m.proxy_puts.is_empty());
        assert_eq!(m.storage_puts.len() as u32, chunks);
        assert_eq!(m.completed.len(), 1);
    });
}

#[tokio::test]
async fn a_direct_session_given_up_after_some_chunks_took_their_progress_back() {
    let scratch = Scratch::new("midgiveup");
    let (path, size, _) = scratch.file(FILE_BYTES);
    // Chunk 0 is stored and committed; chunk 1's four links all fail at storage.
    let state = shared(|m| m.storage_script = VecDeque::from([200, 500, 500, 500, 500]));
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
    snapshot(&state, |m| {
        assert_eq!(m.commits.len(), 1, "chunk 0 was committed before the file was given up");
        assert_eq!(m.storage_puts.len(), 5);
    });
    // The file is counted once: what the dead session confirmed came back off before the retry.
    assert_fell_back(&state, size, &progress, &outcome).await;
}

#[tokio::test]
async fn a_direct_session_that_was_given_up_leaves_no_resume_record_even_if_the_new_init_fails() {
    let scratch = Scratch::new("giveup-record");
    let (path, _size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| {
        m.grant_script = VecDeque::from([Grant::Bare(404)]);
        m.init_script = VecDeque::from([None, Some(403)]);
    });
    let result = run(&state, spec(&path, Uuid::new_v4())).await;
    assert!(result.is_err(), "the second init is refused");
    snapshot(&state, |m| {
        assert_eq!(m.abandons.len(), 1, "the direct session was ended first")
    });
    assert_eq!(
        crate::resume::resumable_upload_now(&path),
        None,
        "a record that points at the abandoned direct session would be resumed into a dead end"
    );
}

#[tokio::test]
async fn a_commit_held_for_longer_than_the_failure_cap_is_waited_out_not_given_up() {
    let scratch = Scratch::new("held");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    // After a copy that failed the server keeps the chunk's commit lock for minutes. Nine one-second holds are more
    // than the eight failures the policy rides out: a hold is not a failure, so the chunk waits and commits.
    let held = json!({"error": "commit_in_progress"});
    let state = shared(|m| m.commit_script = (0..9).map(|_| Commit::Reply(409, held.clone(), Some("1"))).collect());
    let started = Instant::now();
    let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(9),
        "the nine holds were waited out"
    );
    assert_eq!(outcome.transport, "direct", "and the session stayed direct");
    snapshot(&state, |m| {
        assert_eq!(
            m.commits.len() as u32,
            chunks + 9,
            "nine refused commits, then one per chunk"
        );
        assert_eq!(
            m.storage_puts.len() as u32,
            chunks,
            "the staged chunk was not sent again"
        );
        assert!(m.abandons.is_empty() && m.proxy_puts.is_empty());
        assert_eq!(m.completed.len(), 1);
    });
}

// ── review round (cli#105): a resume is not given up on a lookup that nobody answered ──

#[tokio::test]
async fn a_resumed_direct_session_is_kept_when_the_servers_capabilities_cannot_be_read() {
    let scratch = Scratch::new("resume-unknown");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.offers = Some(Discovery::Fails));
    let (file_id, sid) = resumed_session(&state, &path, size, mtime, true, &[0]);
    let err = run(&state, spec(&path, file_id)).await.unwrap_err();
    assert!(
        err.to_string().contains("capabilities") && err.to_string().contains("--transport api"),
        "{err}"
    );
    snapshot(&state, |m| {
        assert!(
            m.abandons.is_empty(),
            "the session is NOT ended: it holds committed chunks"
        );
        assert!(m.inits.is_empty() && m.grants.is_empty() && m.proxy_puts.is_empty());
        assert!(m.completed.is_empty());
    });
    let record = crate::resume::resumable_upload_now(&path).expect("the record is kept");
    assert!(record.direct && record.session_id == sid, "{record:?}");
}

#[tokio::test]
async fn a_resumed_direct_session_is_given_up_when_the_server_says_it_has_no_direct_transport() {
    let scratch = Scratch::new("resume-nodirect");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.offers = Some(Discovery::NoDirect));
    let (file_id, _) = resumed_session(&state, &path, size, mtime, true, &[0]);
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, file_id), &progress).await.unwrap();
    assert_eq!(outcome.transport, "api");
    snapshot(&state, |m| {
        assert_eq!(m.abandons.len(), 1, "a definite no ends the direct session");
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert_eq!(m.completed.len(), 1);
    });
}

#[tokio::test]
async fn a_resumed_direct_session_is_kept_and_asks_for_a_login_when_the_session_is_not_valid() {
    // A 401: exactly the words every other expired session uses, and not the advice to leave the direct route
    // (that would end the session and lose the chunks it holds).
    let err = kept_resume_error("resume-401", Discovery::Unauthorized, false).await;
    assert_eq!(err, crate::api::SESSION_EXPIRED_MESSAGE);
    assert!(!err.contains("--transport"), "{err}");
}

#[tokio::test]
async fn a_resumed_direct_session_is_kept_and_reports_the_status_when_the_lookup_is_forbidden() {
    // A 403 is not an expired session (every other 403 in the CLI says its status): the real status, and no
    // advice to log in again.
    let err = kept_resume_error("resume-403", Discovery::Forbidden, false).await;
    assert!(
        err.contains("forbidden, HTTP 403") && err.contains("progress is kept"),
        "{err}"
    );
    assert!(
        !err.contains("--transport") && !err.contains(crate::api::SESSION_EXPIRED_MESSAGE),
        "{err}"
    );
    assert!(!err.contains("bb login"), "a 403 is not an expired session: {err}");
}

#[tokio::test]
async fn a_resumed_direct_session_is_kept_and_says_not_logged_in_when_there_is_no_session_at_all() {
    let err = kept_resume_error("resume-logged-out", Discovery::Offers, true).await;
    assert_eq!(err, crate::api::NOT_LOGGED_IN_MESSAGE);
    assert!(err.contains("Not logged in"), "{err}");
}

// ── review round (cli#105): direct broken server-wide costs one file, not every file ──

#[tokio::test]
async fn a_dead_session_on_a_direct_commit_says_to_log_in_again_like_the_proxy_route() {
    let scratch = Scratch::new("401");
    let (path, _, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| {
        m.commit_script = VecDeque::from([Commit::Reply(401, json!({"error": "unauthorized"}), None)]);
    });
    let error = run(&state, spec(&path, Uuid::new_v4())).await.unwrap_err();
    assert!(
        error.to_string().contains(crate::api::SESSION_EXPIRED_MESSAGE),
        "{error}"
    );
    snapshot(&state, |m| assert!(m.abandons.is_empty() && m.completed.is_empty()));
}
