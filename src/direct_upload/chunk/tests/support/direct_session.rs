//! One behaviour group of the same integration suite.

use super::*;

#[tokio::test]
async fn a_direct_session_sends_every_chunk_to_storage_and_commits_it() {
    let scratch = Scratch::new("happy");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let chunks = chunk_count(size);
    assert!(chunks >= 2, "the test needs several chunks, got {chunks}");
    let state = shared(|_| {});
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
    snapshot(&state, |m| {
        assert_eq!(
            m.init_headers,
            vec![Some(CAP.to_string())],
            "init asks for the direct upload"
        );
        assert_eq!(m.grants.len() as u32, chunks);
        assert_eq!(m.storage_puts.len() as u32, chunks);
        assert_eq!(m.commits.len() as u32, chunks);
        assert!(
            m.proxy_puts.is_empty(),
            "no chunk goes through the API: {:?}",
            m.proxy_puts
        );
        assert_eq!(m.completed.len(), 1);
        // Exactly the chunk lengths the grant was asked for were sent to storage and committed.
        let granted: u64 = m.grants.iter().map(|g| g.2).sum();
        let stored: u64 = m.storage_puts.iter().map(|p| p.len as u64).sum();
        let committed: u64 = m.commits.iter().map(|c| c.2).sum();
        assert_eq!(
            (granted, stored, committed),
            (
                outcome.ciphertext_bytes,
                outcome.ciphertext_bytes,
                outcome.ciphertext_bytes
            )
        );
        for put in &m.storage_puts {
            assert_eq!(put.content_type.as_deref(), Some("application/octet-stream"));
            assert!(
                !put.had_authorization && !put.had_cookie,
                "storage learns nothing about the account"
            );
        }
        // Every chunk index reached storage, under the session it belongs to (the mock's link path is
        // `<session>/<index>/<n>.bin`).
        let indexes: std::collections::BTreeSet<u32> = m
            .storage_puts
            .iter()
            .map(|p| p.path.split('/').nth(1).unwrap().parse().unwrap())
            .collect();
        assert_eq!(indexes, (0..chunks).collect());
    });
    assert_eq!(*progress.0.lock().unwrap() as u64, outcome.ciphertext_bytes);
    assert_eq!(
        outcome.transport, "direct",
        "the outcome (and `bb push --json`) says so"
    );
    assert_eq!(
        crate::resume::resumable_upload_now(&path),
        None,
        "a finished upload leaves no record"
    );
}

// ───────────────────────────── compatibility ─────────────────────────────
