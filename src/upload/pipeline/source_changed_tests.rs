//! LD-0137: a source file that changes while it is read breaks an attempt in ways the guard before `complete` never
//! sees (the bytes no longer fit the plan; the file is gone; its size moved since the plan was made). These tests
//! change the file at the one point the driver gives them between init and the first read: the start of the file's
//! progress, which comes after the session is open.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use beebeeb_core::kdf::MasterKey;
use uuid::Uuid;

use super::swept_session_reinit::spec;
use super::*;
use crate::test_server::{Mock, Scratch, Shared, spawn};

const GUARD_WORDS: &str = "the file changed (test guard)";

/// A caller that reads the file in place (or, with `says_changed` false, one like `push`). `rewrite` is written to the
/// path when the file's upload begins: after init, before the first byte is read.
struct InPlace {
    rewrite: Option<(PathBuf, Vec<u8>)>,
    says_changed: bool,
}

impl ChunkProgress for InPlace {
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress> {
        if let Some((path, bytes)) = &self.rewrite {
            std::fs::write(path, bytes).unwrap();
        }
        NoopProgress.begin_file(file_name, expected_ciphertext)
    }

    fn before_complete(&self, _path: &Path) -> Result<(), String> {
        if self.says_changed {
            Err(GUARD_WORDS.to_string())
        } else {
            Ok(())
        }
    }
}

async fn api_for(state: &Shared) -> ApiClient {
    ApiClient::new_for_test(spawn(state.clone()).await)
}

/// One upload of a 100 KiB file that is rewritten as `after` right after init.
async fn rewritten_after_init(tag: &str, after: Vec<u8>, says_changed: bool) -> (String, Shared) {
    let scratch = Scratch::new(tag);
    let (path, _, _) = scratch.file(100 * 1024);
    let state: Shared = Arc::new(Mutex::new(Mock::default()));
    let progress = InPlace {
        rewrite: Some((path.clone(), after)),
        says_changed,
    };
    let api = api_for(&state).await;
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let err = match stream_encrypt_upload(&api, mk, spec(&path, Uuid::new_v4()), &progress).await {
        Ok(_) => panic!("an upload of a file that changed must not complete"),
        Err(e) => e.to_string(),
    };
    (err, state)
}

#[tokio::test]
async fn a_file_that_grew_after_init_ends_its_session_whoever_asks() {
    let (err, state) = rewritten_after_init("grow-push", vec![0x5A; 100 * 1024 + 1000], false).await;
    assert!(
        err.contains("integrity check failed"),
        "no guard: the message is the driver's own: {err}"
    );
    let m = state.lock().unwrap();
    assert_eq!(
        m.abandons.len(),
        1,
        "a session that can never complete is ended, for push and sync too"
    );
    assert!(m.completed.is_empty(), "nothing completed");
    assert!(
        m.chunk_puts.is_empty(),
        "the chunk that does not fit the plan is not sent"
    );
}

#[tokio::test]
async fn a_file_that_grew_after_init_is_a_change_for_a_caller_that_says_so() {
    let (err, state) = rewritten_after_init("grow-guard", vec![0x5A; 100 * 1024 + 1000], true).await;
    assert_eq!(err, GUARD_WORDS);
    let m = state.lock().unwrap();
    assert_eq!(m.abandons.len(), 1);
    assert!(m.completed.is_empty() && m.chunk_puts.is_empty());
}

#[tokio::test]
async fn a_file_that_shrank_after_init_is_a_change_for_a_caller_that_says_so() {
    let (err, state) = rewritten_after_init("shrink-guard", vec![0x5A; 1000], true).await;
    assert_eq!(err, GUARD_WORDS);
    let m = state.lock().unwrap();
    assert_eq!(m.abandons.len(), 1);
    assert!(m.completed.is_empty());
}

#[tokio::test]
async fn a_file_that_shrank_after_init_ends_its_session_whoever_asks() {
    let (err, state) = rewritten_after_init("shrink-push", vec![0x5A; 1000], false).await;
    assert!(err.contains("integrity check failed"), "{err}");
    assert_eq!(state.lock().unwrap().abandons.len(), 1);
}

/// A read that fails (the "file" is a directory: it opens, and reading it is an I/O error) is the file's own trouble:
/// its session is kept for a resume, unless the caller says the file changed.
#[cfg(unix)]
async fn unreadable(tag: &str, says_changed: bool) -> (String, Shared) {
    let scratch = Scratch::new(tag);
    let state: Shared = Arc::new(Mutex::new(Mock::default()));
    let progress = InPlace {
        rewrite: None,
        says_changed,
    };
    let api = api_for(&state).await;
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let err = match stream_encrypt_upload(&api, mk, spec(&scratch.dir, Uuid::new_v4()), &progress).await {
        Ok(_) => panic!("a directory is not uploadable"),
        Err(e) => e.to_string(),
    };
    (err, state)
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_read_keeps_its_session_unless_the_caller_says_the_file_changed() {
    let (err, state) = unreadable("read-push", false).await;
    assert!(err.contains("I/O error"), "{err}");
    assert!(
        state.lock().unwrap().abandons.is_empty(),
        "a read error may be only an unreadable file: resumable"
    );
    let (err, state) = unreadable("read-guard", true).await;
    assert_eq!(err, GUARD_WORDS);
    assert_eq!(state.lock().unwrap().abandons.len(), 1);
}

/// The attempt itself, with the size its plan was made for: before init, so no request goes out.
async fn attempt(scratch_tag: &str, planned_size: u64, remove_file: bool, says_changed: bool) -> (String, Shared) {
    let scratch = Scratch::new(scratch_tag);
    let (path, _, _) = scratch.file(2000);
    if remove_file {
        std::fs::remove_file(&path).unwrap();
    }
    let state: Shared = Arc::new(Mutex::new(Mock::default()));
    let api = api_for(&state).await;
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    let spec = spec(&path, Uuid::new_v4());
    let plan = SessionPlan::Init {
        file_id: spec.file_id,
        fallback_file_id: None,
        is_recovery: false,
        resume_tag: None,
    };
    let progress = InPlace {
        rewrite: None,
        says_changed,
    };
    let (mut transferred, mut lowered) = (0, false);
    let outcome = upload_attempt(
        &api,
        &mk,
        &spec,
        planned_size,
        0,
        plan,
        false,
        &progress,
        &mut transferred,
        &mut lowered,
    )
    .await;
    match outcome {
        Err(AttemptErr::Other(message)) => (message, state),
        Err(_) => panic!("expected a plain failure"),
        Ok(_) => panic!("an attempt on a stale plan must not complete"),
    }
}

#[tokio::test]
async fn a_size_that_moved_since_the_plan_is_a_change_for_a_caller_that_says_so() {
    let (message, state) = attempt("stale-guard", 1500, false, true).await;
    assert_eq!(message, GUARD_WORDS);
    assert!(state.lock().unwrap().inits.is_empty(), "no session was opened");
    let (message, _) = attempt("stale-push", 1500, false, false).await;
    assert!(message.contains("changed size during scan"), "{message}");
}

#[tokio::test]
async fn a_file_that_went_away_is_a_change_for_a_caller_that_says_so() {
    let (message, _) = attempt("gone-guard", 2000, true, true).await;
    assert_eq!(message, GUARD_WORDS);
    let (message, _) = attempt("gone-push", 2000, true, false).await;
    assert!(message.starts_with("open "), "{message}");
}

#[test]
fn the_plan_holds_every_chunk_but_the_last_to_one_frame() {
    use super::parallel::planned_frame_len;
    // 3 chunks of 1000 + 28 and a last one of 300 + 28.
    let (framed, total) = (1028, 2 * 1028 + 328);
    assert_eq!(planned_frame_len(framed, 3, total, 0), 1028);
    assert_eq!(planned_frame_len(framed, 3, total, 1), 1028);
    assert_eq!(planned_frame_len(framed, 3, total, 2), 328);
    // One chunk: the whole total, an empty file included (28 bytes of overhead).
    assert_eq!(planned_frame_len(framed, 1, 45, 0), 45);
    assert_eq!(planned_frame_len(framed, 1, 28, 0), 28);
}
