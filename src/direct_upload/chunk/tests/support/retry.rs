//! One behaviour group of the same integration suite.

use super::*;

#[tokio::test]
async fn a_failed_storage_put_is_granted_again_and_the_chunk_still_lands_once() {
    let scratch = Scratch::new("putfail");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let state = shared(|m| m.storage_script = VecDeque::from([403]));
    run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    snapshot(&state, |m| {
        assert_eq!(
            m.grants.len() as u32,
            chunks + 1,
            "one extra link for the chunk whose PUT failed"
        );
        assert_eq!(m.storage_puts.len() as u32, chunks + 1);
        assert_eq!(m.commits.len() as u32, chunks, "only the PUT that worked was committed");
        assert!(m.proxy_puts.is_empty());
        assert!(m.abandons.is_empty());
    });
}

#[tokio::test]
async fn a_chunk_that_was_not_stored_is_granted_again() {
    let scratch = Scratch::new("notstored");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let state = shared(|m| {
        m.commit_script = VecDeque::from([Commit::Reply(409, json!({"error": "chunk_not_stored"}), None)]);
    });
    run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    snapshot(&state, |m| {
        assert_eq!(m.grants.len() as u32, chunks + 1);
        assert_eq!(m.storage_puts.len() as u32, chunks + 1);
        assert_eq!(m.commits.len() as u32, chunks + 1);
        assert!(m.abandons.is_empty() && m.proxy_puts.is_empty());
    });
}

#[tokio::test]
async fn a_busy_commit_waits_the_stated_time_and_commits_the_same_chunk_again() {
    let scratch = Scratch::new("busy");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let state = shared(|m| {
        m.commit_script = VecDeque::from([Commit::Reply(409, json!({"error": "commit_in_progress"}), Some("1"))]);
    });
    let started = Instant::now();
    run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "it waited the stated second"
    );
    snapshot(&state, |m| {
        assert_eq!(
            m.grants.len() as u32,
            chunks,
            "no new link: the staged chunk is still there"
        );
        assert_eq!(m.storage_puts.len() as u32, chunks);
        assert_eq!(m.commits.len() as u32, chunks + 1);
    });
}

#[tokio::test]
async fn a_rate_limited_grant_waits_what_the_server_says() {
    let scratch = Scratch::new("429");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    let limited =
        json!({"error": "rate_limited", "message": "Too many requests. Try again in 1 seconds.", "retry_after": 1});
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(429, limited, Some("1"))]));
    let started = Instant::now();
    run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    snapshot(&state, |m| {
        assert_eq!(
            m.grants.len() as u32,
            chunks + 1,
            "the refused grant is asked for again"
        );
        assert_eq!(m.storage_puts.len() as u32, chunks);
        assert!(m.abandons.is_empty());
    });
}

#[tokio::test]
async fn a_chunk_that_is_waiting_for_a_rate_limit_stops_when_the_run_is_interrupted() {
    let scratch = Scratch::new("interrupt");
    let (path, _size, _) = scratch.file(FILE_BYTES);
    let limited =
        json!({"error": "rate_limited", "message": "Too many requests. Try again in 30 seconds.", "retry_after": 30});
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(429, limited, Some("30"))]));
    let spec = spec(&path, Uuid::new_v4());
    let flag = spec.shutdown.clone();
    let watched = state.clone();
    tokio::spawn(async move {
        // Interrupt once the first grant has been refused and the chunk is waiting (encrypting a chunk
        // takes a machine-dependent time, so a fixed delay could fire before the first request).
        loop {
            let asked = watched.lock().unwrap().grants.len();
            if asked >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let started = Instant::now();
    let result = run(&state, spec).await;
    // The stated wait is 30 s; an interrupt must not sit it out (Ctrl-C has to feel immediate).
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "waited {:?}",
        started.elapsed()
    );
    assert_eq!(result.unwrap_err(), crate::upload::UploadError::Interrupted);
    snapshot(&state, |m| {
        assert_eq!(m.grants.len(), 1, "one grant, refused, then the wait");
        assert!(m.storage_puts.is_empty() && m.commits.is_empty() && m.completed.is_empty());
    });
    // The interrupted session stays resumable, and the record says it is a direct one: a proxy PUT
    // would be refused on it.
    let record = crate::resume::resumable_upload_now(&path).expect("an interrupted upload leaves its record");
    assert!(record.direct, "{record:?}");
}

// ───────────────────────────── falling back ─────────────────────────────
