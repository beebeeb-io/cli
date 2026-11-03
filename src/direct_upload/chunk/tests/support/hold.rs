//! One behaviour group of the same integration suite.

use super::*;

#[tokio::test]
async fn after_one_file_gave_up_the_direct_upload_the_next_one_does_not_try() {
    let scratch = Scratch::new("second-file");
    let (first, size, _) = scratch.file(FILE_BYTES);
    let second = scratch.dir.join("second.bin");
    std::fs::copy(&first, &second).unwrap();
    // The first grant is a bare 404: a node without the route. That file goes back to the API.
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
    let base = spawn(state.clone()).await;
    let api = crate::api::ApiClient::new_for_test(base);
    let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let one = stream_encrypt_upload(&api, key.clone(), spec(&first, Uuid::new_v4()), &NoopProgress)
        .await
        .unwrap();
    assert_eq!(one.transport, "api");
    let two = stream_encrypt_upload(&api, key, spec(&second, Uuid::new_v4()), &NoopProgress)
        .await
        .unwrap();
    assert_eq!(two.transport, "api");
    snapshot(&state, |m| {
        assert_eq!(
            m.init_headers,
            vec![Some(CAP.to_string()), None, None],
            "the first file asks, is refused and starts again; the second file does not ask"
        );
        assert_eq!(
            m.abandons.len(),
            1,
            "one abandon (and one FileDeleted to the other devices), not one per file"
        );
        assert_eq!(m.proxy_puts.len() as u32, 2 * chunk_count(size));
        assert_eq!(m.completed.len(), 2);
    });
}

// ── review round 3 (cli#105): the file's own bit and the remembered failure are two mechanisms ──

#[tokio::test]
async fn a_file_that_gave_up_the_direct_upload_does_not_ask_again_even_when_nothing_is_remembered() {
    // A hold of zero never holds, so only the per-file bit can keep this file's re-init off the direct route
    // (the remembered failure is a wall-clock-free memory with an expiry, and must not be what bounds the loop).
    let scratch = Scratch::new("own-bit");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
    let base = spawn(state.clone()).await;
    let api = crate::api::ApiClient::new_for_test_with_failure_hold(base, std::time::Duration::ZERO);
    let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let outcome = stream_encrypt_upload(&api, key, spec(&path, Uuid::new_v4()), &NoopProgress)
        .await
        .unwrap();
    assert_eq!(outcome.transport, "api");
    snapshot(&state, |m| {
        assert_eq!(
            m.init_headers,
            vec![Some(CAP.to_string()), None],
            "asked once, refused, started again WITHOUT asking"
        );
        assert_eq!(m.abandons.len(), 1);
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
    });
}

#[tokio::test]
async fn a_direct_upload_that_was_given_up_is_tried_again_once_the_hold_has_passed() {
    let scratch = Scratch::new("hold-over");
    let (first, size, _) = scratch.file(FILE_BYTES);
    let second = scratch.dir.join("second.bin");
    std::fs::copy(&first, &second).unwrap();
    let state = shared(|m| m.grant_script = VecDeque::from([Grant::Bare(404)]));
    let base = spawn(state.clone()).await;
    // The hold is over by the time the second file starts.
    let api = crate::api::ApiClient::new_for_test_with_failure_hold(base, std::time::Duration::ZERO);
    let key = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let one = stream_encrypt_upload(&api, key.clone(), spec(&first, Uuid::new_v4()), &NoopProgress)
        .await
        .unwrap();
    assert_eq!(one.transport, "api");
    let two = stream_encrypt_upload(&api, key, spec(&second, Uuid::new_v4()), &NoopProgress)
        .await
        .unwrap();
    assert_eq!(
        two.transport, "direct",
        "the server is asked again, and this time it works"
    );
    snapshot(&state, |m| {
        assert_eq!(
            m.init_headers,
            vec![Some(CAP.to_string()), None, Some(CAP.to_string())],
            "the second file asks again once nothing is held"
        );
        assert_eq!(
            m.proxy_puts.len() as u32,
            chunk_count(size),
            "only the first file went through the API"
        );
    });
}
