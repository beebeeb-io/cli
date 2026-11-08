//! One behaviour group of the same integration suite.

use super::*;

#[test]
fn direct_download_is_byte_identical_and_never_touches_the_api_byte_route() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_identical(&env, &out);
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert_clean(&env, &out);

    let log = env.mock.log.lock().unwrap();
    assert_eq!(log.api_download_hits, 0, "the API streamed bytes");
    assert!(log.api_chunk_hits.is_empty());
    assert_eq!(log.discovery_hits, 1);
    assert_eq!(
        log.discovery_cap_headers,
        vec![None],
        "the capability header is for grants only"
    );
    // Three windows: 0..8, 8..16, 16..21; the first unpinned, the rest pinned.
    let starts: Vec<(u64, u64)> = log
        .grant_bodies
        .iter()
        .map(|b| (b["start_index"].as_u64().unwrap(), b["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(starts, vec![(0, 8), (8, 8), (16, 5)]);
    assert_eq!(log.grant_bodies[0]["object_version_id"], Value::Null);
    assert_eq!(log.grant_bodies[1]["object_version_id"], VERSION);
    assert!(
        log.grant_cap_headers
            .iter()
            .all(|h| h.as_deref() == Some("object-download-v1"))
    );
    // 21 storage requests, one per chunk, none of them carrying the account.
    assert_eq!(log.storage.len(), 21);
    for (index, names) in &log.storage {
        for banned in ["authorization", "cookie", "referer"] {
            assert!(
                !names.iter().any(|n| n == banned),
                "chunk {index} sent {banned}: {names:?}"
            );
        }
        assert!(
            !names.iter().any(|n| n.starts_with("x-beebeeb")),
            "chunk {index} sent a Beebeeb header: {names:?}"
        );
    }
}

#[test]
fn forcing_the_api_transport_by_flag_and_by_environment() {
    for (args, envs) in [
        (vec!["--transport", "api"], vec![]),
        (vec![], vec![("BB_TRANSPORT", "api")]),
    ] {
        let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
        let (out, json) = pull(&env, &args, &envs);
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        assert_eq!(json["transport"], "api", "{}", describe(&out));
        let log = env.mock.log.lock().unwrap();
        assert_eq!(log.discovery_hits, 0, "a forced API run must not even ask discovery");
        assert!(log.grant_bodies.is_empty() && log.storage.is_empty());
        assert_eq!(log.api_download_hits, 1);
    }
}

#[test]
fn the_flag_beats_the_environment_and_auto_means_direct() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &["--transport", "auto"], &[("BB_TRANSPORT", "api")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
}

#[test]
fn servers_without_direct_transport_are_served_over_the_api() {
    // An old server's unknown route, a node without the route, a server error, a
    // server that offers nothing: all end on the API, byte-identical, no error text.
    for mode in [
        Discovery::Bare404,
        Discovery::Json404,
        Discovery::ServerError,
        Discovery::Nothing,
    ] {
        let env = setup(mode, Grants::Normal, &[], &[]);
        let (out, json) = pull(&env, &[], &[]);
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        assert_eq!(json["transport"], "api", "{}", describe(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).trim().is_empty(),
            "{}",
            describe(&out)
        );
        let log = env.mock.log.lock().unwrap();
        assert_eq!(log.discovery_hits, 1, "one discovery request for one file");
        assert!(log.grant_bodies.is_empty());
        assert_eq!(log.api_download_hits, 1);
    }
}

#[test]
fn the_trace_is_off_when_the_variable_says_zero_and_an_unknown_transport_warns_once() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, _) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "0")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("transport:"),
        "{}",
        describe(&out)
    );
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, _) = pull(&env, &[], &[("BB_TRANSPORT_TRACE", "1")]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("transport:"),
        "{}",
        describe(&out)
    );

    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT", "bogus")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        json["transport"],
        "direct",
        "an unknown value means auto: {}",
        describe(&out)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(stderr.matches("ignoring BB_TRANSPORT").count(), 1, "{}", describe(&out));

    // An empty `BB_TRANSPORT=` is unset: no warning, and still auto. `--quiet` silences the warning.
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, json) = pull(&env, &[], &[("BB_TRANSPORT", "")]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(json["transport"], "direct", "{}", describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("BB_TRANSPORT"),
        "{}",
        describe(&out)
    );
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let id = env.mock.fx.file_id.clone();
    let out = bb(
        &env,
        &["pull", &id, "-o", "out.bin", "--quiet"],
        &[("BB_TRANSPORT", "bogus")],
    );
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("BB_TRANSPORT"),
        "{}",
        describe(&out)
    );
}
