//! Task 5 of the `--parallel` plan (spec §5.2), the direct route: a resumed direct session whose grant answers
//! `400 ... not writable: completed` is FINISHED, not failed. The driver completes the session (it is not
//! abandoned as a failure) and reports the file done; here, with no `GET /files/{id}` route, that resolves
//! through the "finished, its row is gone" path, which uploads the file again. Either way the 400 "completed" is
//! told apart from a plain error.

use std::collections::VecDeque;

use super::*;

#[tokio::test]
async fn a_completed_direct_grant_is_finished_not_failed() {
    let scratch = Scratch::new("direct-completed");
    let (path, size, mtime) = scratch.file(FILE_BYTES);
    // A resumed direct session; its first grant answers the completed 400.
    let state = shared(|m| {
        m.grant_script = VecDeque::from([Grant::Reply(
            400,
            serde_json::json!({ "error": "upload session is not writable: completed" }),
            None,
        )]);
    });
    let (file_id, sid) = resumed_session(&state, &path, size, mtime, true, &[]);

    let out = run(&state, spec(&path, file_id)).await.expect("the file is finished");

    snapshot(&state, |m| {
        assert!(
            m.completed.contains(&sid.to_string()),
            "the completed session was completed (not abandoned as a failure): {:?}",
            m.completed
        );
        assert!(m.abandons.is_empty(), "a finished session is never abandoned");
    });
    assert_eq!(out.plaintext_bytes, size);
}
