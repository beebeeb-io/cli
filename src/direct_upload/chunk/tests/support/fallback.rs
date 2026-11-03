//! One behaviour group of the same integration suite.

use super::*;

#[tokio::test]
async fn direct_unavailable_on_a_grant_sends_the_whole_file_through_the_api() {
    let scratch = Scratch::new("du");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let unavailable = json!({"error": "direct_unavailable", "reason": "disabled"});
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Reply(409, unavailable, None)]));
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
    assert_fell_back(&state, size, &progress, &outcome).await;
}

#[tokio::test]
async fn a_bare_404_on_the_grant_route_sends_the_whole_file_through_the_api() {
    let scratch = Scratch::new("bare404");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
    assert_fell_back(&state, size, &progress, &outcome).await;
}

#[tokio::test]
async fn four_failed_links_for_one_chunk_send_the_whole_file_through_the_api() {
    let scratch = Scratch::new("fourlinks");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.storage_script = VecDeque::from([500, 500, 500, 500]));
    let progress = Counting::default();
    let started = Instant::now();
    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "1 s + 2 s + 4 s of waiting, not more"
    );
    snapshot(&state, |m| {
        assert_eq!(
            m.grants.len(),
            4,
            "the server issues four links per chunk and the client asks for no fifth"
        );
        assert_eq!(m.storage_puts.len(), 4);
        assert!(m.commits.is_empty());
    });
    assert_fell_back(&state, size, &progress, &outcome).await;
}

#[tokio::test]
async fn a_link_to_a_host_nobody_listed_is_never_followed() {
    let scratch = Scratch::new("evil");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::LinkTo("evil.example")]));
    let progress = Counting::default();
    let outcome = run_with(&state, spec(&path, Uuid::new_v4()), &progress).await.unwrap();
    // The first link named a host discovery does not list: refused, nothing sent to it (the mock's storage
    // server saw no PUT of the refused link; the API path that followed sends no byte to storage at all).
    snapshot(&state, |m| {
        assert_eq!(m.storage_puts.len(), 0, "no byte was sent to storage");
    });
    assert_fell_back(&state, size, &progress, &outcome).await;
}

// ───────────────────────────── terminal answers ─────────────────────────────

#[tokio::test]
async fn an_authorization_answer_on_commit_is_not_papered_over_and_prints_no_link() {
    let scratch = Scratch::new("403");
    let (path, _, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| {
        m.commit_script = VecDeque::from([Commit::Reply(403, json!({"error": "forbidden"}), None)]);
    });
    let error = run(&state, spec(&path, Uuid::new_v4())).await.unwrap_err();
    assert!(error.to_string().contains("forbidden"), "{error}");
    assert!(
        !error.to_string().contains("X-Amz")
            && !error.to_string().contains("SIGSECRET")
            && !error.to_string().contains("127.0.0.1"),
        "{error}"
    );
    snapshot(&state, |m| {
        assert!(m.abandons.is_empty(), "no fallback after an authorization answer");
        assert!(m.proxy_puts.is_empty());
        assert!(m.completed.is_empty());
    });
}

// ───────────────────────────── resume ─────────────────────────────
