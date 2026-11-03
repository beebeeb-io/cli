//! One behaviour group of the same integration suite.

use super::*;

#[tokio::test]
async fn an_old_server_without_discovery_gets_todays_upload_and_no_capability_header() {
    let scratch = Scratch::new("old");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.offers = Some(Discovery::Unknown));
    let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert_eq!(outcome.transport, "api");
    snapshot(&state, |m| {
        assert_eq!(m.init_headers, vec![None], "the init is byte-identical to today's");
        assert!(m.grants.is_empty() && m.storage_puts.is_empty() && m.commits.is_empty());
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert_eq!(m.completed.len(), 1);
    });
}

#[tokio::test]
async fn a_server_with_uploads_off_gets_todays_upload() {
    let scratch = Scratch::new("off");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.offers = Some(Discovery::UploadOff));
    let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert_eq!(outcome.transport, "api");
    snapshot(&state, |m| {
        assert_eq!(m.init_headers, vec![None]);
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert!(m.grants.is_empty());
    });
}

#[tokio::test]
async fn a_server_that_does_not_select_the_direct_upload_gets_proxy_puts() {
    let scratch = Scratch::new("notselected");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| m.select_direct = false);
    let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert_eq!(outcome.transport, "api");
    snapshot(&state, |m| {
        assert_eq!(
            m.init_headers,
            vec![Some(CAP.to_string())],
            "asked, and answered without a transport"
        );
        assert!(m.grants.is_empty() && m.storage_puts.is_empty());
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert_eq!(m.completed.len(), 1);
    });
}

// ───────────────────────────── retries ─────────────────────────────

#[tokio::test]
async fn a_transport_the_cli_does_not_know_is_todays_upload() {
    let scratch = Scratch::new("unknown-transport");
    let (path, size, _) = scratch.file(FILE_BYTES);
    let state = shared(|m| {
        // A server that answers with a transport this CLI has never heard of; the session itself is the
        // plain one (the mock only accepts proxy PUTs on a session that was not opened direct).
        m.select_direct = false;
        m.init_transport = Some("object-upload-v2");
    });
    let outcome = run(&state, spec(&path, Uuid::new_v4())).await.unwrap();
    assert_eq!(outcome.transport, "api");
    snapshot(&state, |m| {
        assert!(m.grants.is_empty() && m.storage_puts.is_empty() && m.commits.is_empty());
        assert_eq!(m.proxy_puts.len() as u32, chunk_count(size));
        assert!(m.abandons.is_empty());
    });
}
