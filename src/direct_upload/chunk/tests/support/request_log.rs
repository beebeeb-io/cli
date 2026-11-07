//! Task 1 of the `--parallel` plan, the direct route: at one chunk at a time a direct push makes today's
//! requests (grant, storage PUT, commit per chunk) in today's order. The goldens were recorded at cli
//! origin/main `27612df`; see `upload::pipeline::request_log_tests` for the rule and the API-route cases.

use super::*;
use crate::test_server::RequestLog;
use crate::upload::request_log_tests::{LARGE, SMALL, check_golden};

/// `spec` at one chunk at a time: what `bb push` sends today.
fn spec_one_at_a_time(path: &std::path::Path) -> UploadSpec {
    UploadSpec {
        concurrency: 1,
        ..spec(path, Uuid::new_v4())
    }
}

async fn push(path: &std::path::Path) -> RequestLog {
    let log = RequestLog::default();
    let state = shared(|m| m.requests = Some(log.clone()));
    let out = run(&state, spec_one_at_a_time(path)).await.expect("upload");
    assert_eq!(out.transport, "direct", "premise: the mock offered the direct route");
    log
}

#[tokio::test]
async fn parallel_1_matches_main_request_log_direct_small() {
    let scratch = Scratch::new("golden-direct-small");
    let (path, _, _) = scratch.file(SMALL as usize);
    let log = push(&path).await;
    check_golden("push_parallel1_direct_small", &log);
}

#[tokio::test]
async fn parallel_1_matches_main_request_log_direct_large() {
    let scratch = Scratch::new("golden-direct-large");
    let (path, _, _) = scratch.sparse_file(LARGE);
    let log = push(&path).await;
    check_golden("push_parallel1_direct_large", &log);
}
