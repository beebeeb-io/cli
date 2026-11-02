//! One behaviour group of the same integration suite.

use super::*;

#[test]
fn a_refusal_to_serve_this_file_directly_falls_back_to_the_api() {
    for mode in [
        Grants::DirectUnavailable,
        Grants::Bare404,
        Grants::UnlistedHost,
        Grants::WrongChunkCount,
    ] {
        let env = setup(Discovery::Offer, mode, &[], &[]);
        let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
        if mode == Grants::WrongChunkCount {
            // The metadata says 22 chunks, the manifest 21: the direct path steps aside
            // without fetching anything, and the API path's own checks decide the file
            // (its stream has 21 frames, so it refuses). Nothing is published either way.
            assert!(!out.status.success(), "{}", describe(&out));
            assert_nothing_written(&env, &out);
            assert_clean(&env, &out);
            assert_eq!(
                env.mock.log.lock().unwrap().storage.len(),
                0,
                "nothing fetched from storage"
            );
            assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 1);
            let _ = json;
            continue;
        }
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        assert_eq!(json["transport"], "api", "{}", describe(&out));
        assert_clean(&env, &out);
        let log = env.mock.log.lock().unwrap();
        assert_eq!(log.api_download_hits, 1);
        assert_eq!(log.storage.len(), 0, "no storage request for a refused file");
    }
}

#[test]
fn an_authorization_refusal_is_terminal_and_never_falls_back() {
    let env = setup(Discovery::Offer, Grants::Forbidden, &[], &[]);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("403"),
        "{}",
        describe(&out)
    );
    assert_eq!(
        env.mock.log.lock().unwrap().api_download_hits,
        0,
        "403 must not be routed around"
    );
}

#[test]
fn a_file_another_device_is_still_uploading_goes_straight_to_the_api() {
    // 409 replace_in_progress with no Retry-After: not a moment's hold. The API path answers it at
    // once; five waits of five seconds per file would make every sync pass slower.
    let env = setup(Discovery::Offer, Grants::ReplaceInProgressNoRetryAfter, &[], &[]);
    let started = std::time::Instant::now();
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "it did not wait");
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "api", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.grant_bodies.len(), 1, "one grant request, then the API");
    assert_eq!(log.api_download_hits, 1);
}

#[test]
fn a_file_whose_first_chunk_cannot_be_opened_is_left_to_the_api_paths_legacy_readers() {
    // Frame 0 fails under the string-UUID key: a pre-`bb repair` file keyed by the binary uuid. The
    // direct path must not call that "corrupt" (a refetch would fail the same way); the API path's
    // buffered fallback reads it. Chunks after the first stay corrupt (see the double-tamper test).
    let env = setup_with(fixture_legacy_binary_uuid(), Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "api", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert!(!log.grant_bodies.is_empty(), "the direct path was tried first");
    assert!(
        log.api_chunk_hits.is_empty(),
        "no pinned refetch for chunk 0: it would fail the same way"
    );
    assert_eq!(log.api_download_hits, 1, "the API stream took the file");
}

#[test]
fn a_tampered_first_chunk_of_a_v2_file_is_corrupt_and_not_handed_to_the_legacy_readers() {
    // A V2 pin has no legacy form: its reader tries only the string-UUID key. A first chunk that fails
    // there is damaged or tampered with, so it must NOT go to the API stream (where a large file would
    // end in a confusing "legacy-format file" error and, in sync, abort the pass).
    let env = setup_with(
        fixture_v2_missing_last_frame(),
        Discovery::Offer,
        Grants::Normal,
        &[(0, Fault::Tamper)],
        &[0],
    );
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("authenticate"),
        "{}",
        describe(&out)
    );
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0, "not routed around");
}

// ─────────────────────────────── bb sync --once ───────────────────────────────

#[test]
fn an_expired_link_is_renewed_once_and_the_file_stays_direct() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(3, Fault::Forbidden(1))], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert_clean(&env, &out);
    assert_eq!(storage_hits(&env, 3), 2, "one failure, one retry on a fresh link");
    let log = env.mock.log.lock().unwrap();
    assert!(
        log.grant_bodies
            .iter()
            .any(|b| b["renewal"] == true && b["start_index"] == 3 && b["count"] == 1),
        "a renewal for chunk 3 was requested: {:?}",
        log.grant_bodies
    );
    assert!(log.api_chunk_hits.is_empty() && log.api_download_hits == 0);
}

#[test]
fn a_chunk_storage_never_serves_goes_to_the_pinned_api_route() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(5, Fault::Forbidden(99))], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    assert_eq!(storage_hits(&env, 5), 4, "the first attempt plus three renewals");
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_chunk_hits, vec![5], "only that chunk, through the pinned route");
    assert_eq!(log.api_download_hits, 0, "never the full-file stream");
}

#[test]
fn broken_storage_responses_are_handled_like_any_storage_failure() {
    // A short body, a Content-Length that is not the grant's, a body with no
    // Content-Length: each chunk is retried, then fetched through the API.
    let env = setup(
        Discovery::Offer,
        Grants::Normal,
        &[
            (0, Fault::ShortBody),
            (4, Fault::WrongLength),
            (9, Fault::Chunked),
            (12, Fault::Status206),
        ],
        &[],
    );
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    let mut api = env.mock.log.lock().unwrap().api_chunk_hits.clone();
    api.sort_unstable();
    assert_eq!(api, vec![0, 4, 9, 12]);
}

#[test]
fn a_tampered_storage_chunk_is_refetched_through_the_api_and_a_double_tamper_is_fatal() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_eq!(env.mock.log.lock().unwrap().api_chunk_hits, vec![2]);

    let env = setup(Discovery::Offer, Grants::Normal, &[(2, Fault::Tamper)], &[2]);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert_clean(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("authenticate"),
        "{}",
        describe(&out)
    );
}

#[test]
fn a_storage_redirect_is_never_followed() {
    let env = setup(Discovery::Offer, Grants::Normal, &[(1, Fault::Redirect)], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.leak_hits, 0, "the client followed a redirect off the storage host");
    assert_eq!(log.api_chunk_hits, vec![1]);
}

#[test]
fn a_window_the_server_refuses_after_the_first_moves_the_rest_to_the_pinned_api_route() {
    // The mixed-node rollout case (R1.6): the first window came from a node with the route, a later
    // one from a node without it. What was written stays; the chunks without a grant come through
    // the pinned per-chunk route; the full-file stream is never used and no transport is mixed
    // WITHIN a chunk.
    let env = setup(Discovery::Offer, Grants::UnavailableAfterFirstWindow, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    assert_clean(&env, &out);
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_download_hits, 0, "no full-file stream");
    let mut api = log.api_chunk_hits.clone();
    api.sort_unstable();
    assert_eq!(
        api,
        (8..21).collect::<Vec<u32>>(),
        "exactly the chunks the first window did not cover"
    );
    let storage: std::collections::BTreeSet<u32> = log.storage.iter().map(|(i, _)| *i).collect();
    assert_eq!(storage, (0..8).collect(), "storage served exactly the granted chunks");
}

#[test]
fn an_undecodable_window_after_the_first_is_handled_like_any_window_that_cannot_be_served() {
    let env = setup(Discovery::Offer, Grants::GarbageAfterFirstWindow, &[], &[]);
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "mixed", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_download_hits, 0);
    assert_eq!(log.api_chunk_hits.len(), 13, "chunks 8..=20 through the pinned route");
}

#[test]
fn a_file_held_by_another_operation_is_waited_for_and_stays_direct() {
    // 409 replace_in_progress with Retry-After: 1 on the first grant request (R1.13): wait, ask again.
    let env = setup(Discovery::Offer, Grants::ReplaceInProgressOnce, &[], &[]);
    let started = std::time::Instant::now();
    let (out, json) = pull(&env, &[], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(900),
        "it waited for Retry-After"
    );
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    let log = env.mock.log.lock().unwrap();
    assert_eq!(
        log.grant_bodies[0], log.grant_bodies[1],
        "the same first window was asked for again"
    );
    assert_eq!(log.api_download_hits, 0);
}

#[test]
fn the_api_fallback_route_is_held_to_the_manifest_length_and_the_version_pin() {
    // Storage never serves chunk 5, so it goes through the pinned API route. A body one byte
    // short there is a failure, not a chunk: nothing per chunk is left to try, so the file
    // ends on the full API stream, from the start, with the partial output discarded.
    let env = setup(Discovery::Offer, Grants::Normal, &[(5, Fault::Forbidden(99))], &[]);
    env.mock.cfg.lock().unwrap().api_chunk_short.push(5);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "api", "{}", describe(&out));
    assert_clean(&env, &out);
    {
        let log = env.mock.log.lock().unwrap();
        assert!(
            log.api_chunk_hits.contains(&5),
            "the pinned route was tried for chunk 5"
        );
        assert_eq!(
            log.api_download_hits, 1,
            "then the whole file came through the API stream"
        );
    }

    // The pinned version gone on that route: abort, nothing written, no full-file stream.
    let env = setup(Discovery::Offer, Grants::Normal, &[(5, Fault::Forbidden(99))], &[]);
    env.mock.cfg.lock().unwrap().api_chunk_gone.push(5);
    let (out, _) = pull(&env, &[], &[]);
    assert!(!out.status.success(), "{}", describe(&out));
    assert_nothing_written(&env, &out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("changed"),
        "{}",
        describe(&out)
    );
    assert_eq!(env.mock.log.lock().unwrap().api_download_hits, 0);
}
