//! Task 2334: an interrupted create is continued only into the folder this write asks for.
//!
//! The server sets a new file's parent at its first init and keeps it at completion, so continuing a session
//! the first attempt opened elsewhere completes the file there. The driver reads the row's parent first: the
//! same folder continues; another folder is ended (abandon, which deletes the never-completed row) and the file
//! starts again under this write's id and parent; an answer it cannot read, or an abandon that fails, keeps the
//! record and starts nothing.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use beebeeb_core::chunk_stream::ChunkFormat;
use beebeeb_core::kdf::MasterKey;
use serde_json::json;
use uuid::Uuid;

use super::{NoopProgress, UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;
use crate::test_server::{Mock, Scratch, Shared, spawn};

fn spec(path: &std::path::Path, file_id: Uuid, parent_id: Option<Uuid>) -> UploadSpec {
    UploadSpec {
        path: path.to_path_buf(),
        file_name: "payload.bin".to_string(),
        file_id,
        resume_tag: None,
        base_version_number: None,
        replace_pin: None,
        replace_file_key: None,
        parent_id,
        concurrency: 1,
        shutdown: Arc::new(AtomicBool::new(false)),
    }
}

/// An interrupted create of `path`: a live session for `stored`, whose row the server lists in `row_parent`.
fn interrupted(s: &Scratch, row_parent: Option<Uuid>) -> (std::path::PathBuf, Uuid, Uuid, Shared) {
    let (path, size, mtime) = s.file(300 * 1024);
    let stored = Uuid::new_v4();
    let sid = Uuid::new_v4();
    crate::resume::record(&path, stored, sid, ChunkFormat::V1Legacy, size, mtime);
    let mut mock = Mock::default();
    mock.live.insert(sid.to_string());
    mock.session_file.insert(sid.to_string(), stored.to_string());
    mock.files.insert(
        stored.to_string(),
        json!({ "id": stored, "parent_id": row_parent, "is_uploading": true, "download_version": null }),
    );
    (path, stored, sid, Arc::new(Mutex::new(mock)))
}

async fn run(state: &Shared, spec: UploadSpec) -> Result<super::UploadOutcome, super::UploadError> {
    let api = ApiClient::new_for_test(spawn(state.clone()).await);
    stream_encrypt_upload(&api, Arc::new(MasterKey::from_bytes([9u8; 32])), spec, &NoopProgress).await
}

#[tokio::test]
async fn an_upload_interrupted_into_another_folder_is_ended_then_started_here() {
    let s = Scratch::new("2334-elsewhere");
    let (elsewhere, wanted) = (Uuid::new_v4(), Uuid::new_v4());
    let (path, stored, sid, state) = interrupted(&s, Some(elsewhere));
    let fresh = Uuid::new_v4();

    let out = run(&state, spec(&path, fresh, Some(wanted)))
        .await
        .expect("the upload starts again here");

    assert_eq!(out.server_id, fresh, "under this write's own id");
    let m = state.lock().unwrap();
    assert_eq!(
        m.abandons,
        vec![stored.to_string()],
        "the other folder's upload is ended"
    );
    assert_eq!(m.inits, vec![fresh.to_string()]);
    assert_eq!(
        m.init_bodies[0]["parent_id"],
        json!(wanted),
        "into the folder this write asks for"
    );
    let first_init = m.events.iter().position(|e| e.starts_with("init:")).unwrap();
    let abandon = m.events.iter().position(|e| e.starts_with("abandon:")).unwrap();
    assert!(abandon < first_init, "ended before anything starts: {:?}", m.events);
    assert!(
        m.chunk_puts.iter().all(|(s, _)| *s != sid.to_string()),
        "nothing goes to the old session: {:?}",
        m.chunk_puts
    );
    assert!(
        crate::resume::resumable_upload_now(&path).is_none(),
        "no record is left"
    );
}

/// The root is a folder too: an upload interrupted at the root does not continue into a folder (Guus's push
/// had no `--folder` the first time).
#[tokio::test]
async fn an_upload_interrupted_at_the_root_is_not_continued_into_a_folder() {
    let s = Scratch::new("2334-root");
    let (path, stored, _, state) = interrupted(&s, None);
    let wanted = Uuid::new_v4();

    run(&state, spec(&path, Uuid::new_v4(), Some(wanted)))
        .await
        .expect("the upload starts again here");

    let m = state.lock().unwrap();
    assert_eq!(m.abandons, vec![stored.to_string()]);
    assert_eq!(m.init_bodies.len(), 1);
    assert_eq!(m.init_bodies[0]["parent_id"], json!(wanted));
}

#[tokio::test]
async fn an_upload_interrupted_into_the_same_folder_is_continued() {
    let s = Scratch::new("2334-same");
    let wanted = Uuid::new_v4();
    let (path, stored, sid, state) = interrupted(&s, Some(wanted));

    let out = run(&state, spec(&path, Uuid::new_v4(), Some(wanted)))
        .await
        .expect("continued");

    assert_eq!(out.server_id, stored, "the interrupted upload itself completes");
    let m = state.lock().unwrap();
    assert!(m.abandons.is_empty() && m.inits.is_empty(), "{:?}", m.events);
    assert_eq!(m.completed, vec![sid.to_string()]);
}

#[tokio::test]
async fn when_the_folder_cannot_be_read_the_record_is_kept_and_nothing_starts() {
    let s = Scratch::new("2334-unreadable");
    let (path, stored, _, state) = interrupted(&s, Some(Uuid::new_v4()));
    state.lock().unwrap().file_status = Some(503);

    let err = run(&state, spec(&path, Uuid::new_v4(), Some(Uuid::new_v4())))
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("could not check which folder") && err.contains("progress is kept"),
        "{err}"
    );
    let m = state.lock().unwrap();
    assert!(m.events.is_empty(), "nothing is ended, inited or sent: {:?}", m.events);
    let record = crate::resume::resumable_upload_now(&path).expect("the record is kept");
    assert_eq!(record.file_id, stored);
}

#[tokio::test]
async fn when_the_other_folders_upload_cannot_be_ended_the_record_is_kept_and_nothing_starts() {
    let s = Scratch::new("2334-unended");
    let (path, stored, _, state) = interrupted(&s, Some(Uuid::new_v4()));
    state.lock().unwrap().abandon_status = Some(503);

    let err = run(&state, spec(&path, Uuid::new_v4(), Some(Uuid::new_v4())))
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("could not be ended") && err.contains("tried again next run"),
        "{err}"
    );
    let m = state.lock().unwrap();
    assert!(m.inits.is_empty() && m.chunk_puts.is_empty(), "{:?}", m.events);
    let record = crate::resume::resumable_upload_now(&path).expect("the record is kept");
    assert_eq!(record.file_id, stored, "so the next run ends it");
}
