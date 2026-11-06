//! Task 5 of the `--parallel` plan (spec §5.2, Review Focus 4): a resumed session the server reports already
//! completed is FINISHED, never failed and never re-initialised. It is told apart from an expired (swept)
//! session. The row decides: done here, done in another folder (upload again here), or gone (upload again).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use beebeeb_core::chunk_stream::ChunkFormat;
use beebeeb_core::kdf::MasterKey;
use serde_json::json;
use uuid::Uuid;

use super::{ReadRow, UploadOutcome, UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;
use crate::test_server::{Gone, Mock, Scratch, Shared, shared, spawn};
use crate::upload::{ChunkProgress, Recording, UploadError};

/// A `bb push` spec at one chunk at a time (the completed-session path does not depend on `--parallel`), carrying
/// the row the caller read (so `resumable_here` keeps the session instead of re-reading).
fn spec(path: &std::path::Path, file_id: Uuid, read_row: Option<ReadRow>) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id,
        resume_tag: None,
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id: None,
        concurrency: 1,
        unrecorded_resume_concurrency: 1,
        chunks_in_flight: 1,
        limits: crate::upload::limits::ChunkLimits::single(),
        interrupt: None,
        shutdown: Arc::new(AtomicBool::new(false)),
        read_row,
    }
}

async fn run(state: &Shared, spec: UploadSpec, rec: &Recording) -> Result<UploadOutcome, UploadError> {
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, mk, spec, rec).await
}

/// Record a resumed session the server reports already completed; return its file id. `direct`: a direct session
/// (its grant, not a proxy PUT, answers the 400).
fn completed_resumed(mock: &mut Mock, path: &std::path::Path, size: u64, mtime: u128, direct: bool) -> Uuid {
    let (file_id, sid) = (Uuid::new_v4(), Uuid::new_v4());
    crate::resume::record_with_transport(
        path,
        crate::item_id::Proposed::Minted(file_id),
        None,
        sid,
        ChunkFormat::V1Legacy,
        size,
        mtime,
        direct,
    );
    mock.live.insert(sid.to_string());
    mock.session_file.insert(sid.to_string(), file_id.to_string());
    mock.completed_sessions.insert(sid.to_string());
    file_id
}

fn read_root(file_id: Uuid) -> Option<ReadRow> {
    Some(ReadRow {
        file_id,
        parent_id: None,
    })
}

/// The row is in the folder asked for: the file is reported uploaded, no chunk is stored, `complete` is called
/// once (answering `already_completed`), the record is cleared, and no new session is inited.
#[tokio::test]
async fn completed_session_on_resume_is_finished() {
    let s = Scratch::new("completed-finished");
    let (path, size, mtime) = s.file(9 * 1024 * 1024);
    let mut mock = Mock::default();
    let file_id = completed_resumed(&mut mock, &path, size, mtime, false);
    // The row sits at the root, the folder this push asks for.
    mock.files
        .insert(file_id.to_string(), json!({ "id": file_id, "parent_id": null }));
    let state = shared(mock);
    let rec = Recording::default();

    let out = run(&state, spec(&path, file_id, read_root(file_id)), &rec)
        .await
        .expect("a completed session is finished, not failed");

    assert_eq!(out.server_id, file_id);
    let m = state.lock().unwrap();
    assert!(m.chunk_stored_fresh.is_empty(), "no chunk body is stored");
    assert_eq!(m.completed.len(), 1, "complete is called once");
    assert!(m.inits.is_empty(), "no new session is inited");
    assert!(m.abandons.is_empty(), "the finished session is not abandoned");
    drop(m);
    assert!(
        crate::resume::resumable_upload(&path, size, mtime).is_none(),
        "the record is cleared"
    );
    assert_eq!(rec.of("finish"), ["finish true"]);
}

/// The row finished in another folder: the record is cleared (no abandon), the file is inited again under a
/// fresh id in the folder asked for, and the line is printed verbatim.
#[tokio::test]
async fn completed_session_on_resume_in_another_folder_starts_again_here() {
    let s = Scratch::new("completed-elsewhere");
    let (path, size, mtime) = s.file(9 * 1024 * 1024);
    let mut mock = Mock::default();
    let file_id = completed_resumed(&mut mock, &path, size, mtime, false);
    // The server row is in some OTHER folder; the caller read it at the root a moment ago (so `resumable_here`
    // keeps the session), and the completed path re-reads and finds it moved.
    mock.files.insert(
        file_id.to_string(),
        json!({ "id": file_id, "parent_id": Uuid::new_v4() }),
    );
    let state = shared(mock);
    let rec = Recording::default();
    let fresh = Uuid::new_v4();

    let out = run(&state, spec(&path, fresh, read_root(file_id)), &rec)
        .await
        .expect("the file is uploaded again here");

    assert_eq!(out.server_id, fresh, "a fresh id, not the one that finished elsewhere");
    assert_ne!(out.server_id, file_id);
    let m = state.lock().unwrap();
    assert_eq!(m.inits, [fresh.to_string()], "one fresh init, in the asked folder");
    assert!(m.abandons.is_empty(), "the finished file is left as it is");
    drop(m);
    assert!(
        rec.of("note").iter().any(|l| l
            == "note err   payload.bin: the interrupted upload finished in another folder; uploading it again here"),
        "{:?}",
        rec.of("note")
    );
}

/// The row is gone (404): the file is uploaded again, with its own line.
#[tokio::test]
async fn completed_session_on_resume_gone_starts_again() {
    let s = Scratch::new("completed-gone");
    let (path, size, mtime) = s.file(9 * 1024 * 1024);
    let mut mock = Mock::default();
    let file_id = completed_resumed(&mut mock, &path, size, mtime, false);
    // No `files` row: `GET /files/{id}` answers 404.
    let state = shared(mock);
    let rec = Recording::default();

    run(&state, spec(&path, file_id, read_root(file_id)), &rec)
        .await
        .expect("the file is uploaded again");

    assert!(
        rec.of("note").iter().any(|l| l
            == "note err   payload.bin: the interrupted upload finished, but its file is gone; uploading it again"),
        "{:?}",
        rec.of("note")
    );
}

/// The server cannot say where the finished upload is (500): the record is kept and the file fails for this run.
#[tokio::test]
async fn completed_session_on_resume_unknown_keeps_the_record() {
    let s = Scratch::new("completed-unknown");
    let (path, size, mtime) = s.file(9 * 1024 * 1024);
    let mut mock = Mock::default();
    let file_id = completed_resumed(&mut mock, &path, size, mtime, false);
    mock.file_status = Some(500);
    let state = shared(mock);
    let rec = Recording::default();

    let err = run(&state, spec(&path, file_id, read_root(file_id)), &rec)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("run the command again"), "{err}");
    assert!(
        crate::resume::resumable_upload(&path, size, mtime).is_some(),
        "the record is kept for the next run"
    );
    assert!(state.lock().unwrap().inits.is_empty(), "no fresh upload this run");
}

// The direct route's grant answering the 400 "completed" is tested where the direct harness lives (it serves the
// transfer-capabilities route a resumed direct session needs): `direct_upload::chunk::tests::support::completed`.

/// An EXPIRED session (not completed) still re-inits through the swept-session recovery: the two 400s are told
/// apart.
#[tokio::test]
async fn expired_session_still_reinits() {
    let s = Scratch::new("completed-vs-expired");
    let (path, size, mtime) = s.file(9 * 1024 * 1024);
    let (file_id, sid) = (Uuid::new_v4(), Uuid::new_v4());
    let mut mock = Mock::default();
    crate::resume::record(&path, file_id, sid, ChunkFormat::V1Legacy, size, mtime);
    // The session is NOT live and answers the expired 400: the swept-session recovery re-inits it.
    mock.session_file.insert(sid.to_string(), file_id.to_string());
    mock.gone = Some(Gone::BadRequestExpired);
    mock.files
        .insert(file_id.to_string(), json!({ "id": file_id, "parent_id": null }));
    let state = shared(mock);
    let rec = Recording::default();

    run(&state, spec(&path, file_id, read_root(file_id)), &rec)
        .await
        .expect("an expired session re-inits and uploads");

    assert_eq!(
        state.lock().unwrap().inits.len(),
        1,
        "it re-inited, it did not treat expired as completed"
    );
    assert!(
        rec.of("note").iter().any(|l| l.contains("expired on the server")),
        "{:?}",
        rec.of("note")
    );
}
