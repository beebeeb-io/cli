//! A swept session is re-initialised once, by file id and then from chunk 0 — never
//! retried against the dead session.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use beebeeb_core::kdf::MasterKey;
use serde_json::json;
use uuid::Uuid;

use super::{ChunkProgress, FileProgress, NoopProgress, UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;

// One shared mock of the upload routes (L13 round 2, L8).
use crate::test_server::{Gone, Mock, Scratch, Shared, spawn};

fn spec(path: &std::path::Path, file_id: Uuid) -> UploadSpec {
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
        read_row: None,
    }
}

async fn run(state: &Shared, spec: UploadSpec) -> Result<super::UploadOutcome, super::UploadError> {
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, mk, spec, &NoopProgress).await
}

async fn run_with_progress(
    state: &Shared,
    spec: UploadSpec,
    progress: &dyn ChunkProgress,
) -> Result<super::UploadOutcome, super::UploadError> {
    let base = spawn(state.clone()).await;
    let api = ApiClient::new_for_test(base);
    let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
    stream_encrypt_upload(&api, mk, spec, progress).await
}

/// Records exactly the two run-wide numbers a REAL `ChunkProgress`
/// consumer (`BarProgress`) exposes to the user: the shared ciphertext
/// total after every `chunk_confirmed` and every `rollback`, and how many
/// times a file was reported finished successfully. Proves Codex thread
/// #2: a swept-session retry — a brand-new `FileProgress` per attempt,
/// restarting at chunk 0 — must not leave either number counting the same
/// logical file twice.
#[derive(Default)]
struct RecordingProgress {
    overall_bytes: Arc<std::sync::atomic::AtomicU64>,
    files_done: Arc<std::sync::atomic::AtomicU64>,
}

impl ChunkProgress for RecordingProgress {
    fn begin_file(&self, _file_name: &str, _expected_ciphertext: u64) -> Box<dyn FileProgress> {
        Box::new(RecordingFileProgress {
            overall_bytes: Arc::clone(&self.overall_bytes),
            files_done: Arc::clone(&self.files_done),
        })
    }

    fn rollback(&self, ciphertext_bytes: u64) {
        let _ = self.overall_bytes.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |v| Some(v.saturating_sub(ciphertext_bytes)),
        );
    }
}

struct RecordingFileProgress {
    overall_bytes: Arc<std::sync::atomic::AtomicU64>,
    files_done: Arc<std::sync::atomic::AtomicU64>,
}

impl FileProgress for RecordingFileProgress {
    fn chunk_confirmed(&self, ciphertext_bytes: u64) {
        self.overall_bytes
            .fetch_add(ciphertext_bytes, std::sync::atomic::Ordering::Relaxed);
    }

    fn finish(self: Box<Self>, success: bool) {
        if success {
            self.files_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Shared body of the two "swept on the first chunk PUT" cases.
async fn swept_on_chunk_recovers(gone: Gone, tag: &str) {
    let s = Scratch::new(tag);
    let (path, size, mtime) = s.file(300 * 1024);
    let stored_fid = Uuid::new_v4();
    let dead_sid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        dead_sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );

    let state: Shared = Arc::new(Mutex::new(Mock {
        gone: Some(gone),
        ..Default::default()
    }));
    let fresh_fid = Uuid::new_v4();
    let out = run(&state, spec(&path, fresh_fid)).await.expect("upload must recover");

    let m = state.lock().unwrap();
    // The dead session was tried first (the resume), exactly once.
    assert_eq!(m.chunk_puts.first().map(|c| c.0.clone()), Some(dead_sid.to_string()));
    assert_eq!(m.chunk_puts.iter().filter(|c| c.0 == dead_sid.to_string()).count(), 1);
    // Exactly one re-init, with the STORED file_id.
    assert_eq!(m.inits, vec![stored_fid.to_string()]);
    // The new session got every chunk from 0 and was completed.
    let new_sid = m.completed.first().cloned().expect("new session completed");
    let idx: Vec<u32> = m.chunk_puts.iter().filter(|c| c.0 == new_sid).map(|c| c.1).collect();
    assert_eq!(idx.first(), Some(&0), "re-upload starts at chunk 0");
    assert!(idx.windows(2).all(|w| w[1] == w[0] + 1));
    assert_eq!(out.server_id, stored_fid);
    // The stale record is gone (and the success cleared the new one).
    assert_eq!(crate::resume::resumable_upload(&path, size, mtime), None);
}

#[tokio::test]
async fn resumed_session_404_on_chunk_reinits_with_stored_id() {
    swept_on_chunk_recovers(Gone::NotFound, "chunk404").await;
}

#[tokio::test]
async fn resumed_session_400_expired_on_chunk_reinits_with_stored_id() {
    swept_on_chunk_recovers(Gone::BadRequestExpired, "chunk400").await;
}

#[tokio::test]
async fn resumed_session_404_on_complete_reinits() {
    let s = Scratch::new("complete404");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    let sid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.live.insert(sid.to_string());
    mock.complete_gone.insert(sid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let out = run(&state, spec(&path, Uuid::new_v4()))
        .await
        .expect("upload must recover");
    let m = state.lock().unwrap();
    assert_eq!(m.inits, vec![stored_fid.to_string()]);
    assert_eq!(m.completed.len(), 1);
    assert_ne!(m.completed[0], sid.to_string());
    assert_eq!(out.server_id, stored_fid);
    assert_eq!(crate::resume::resumable_upload(&path, size, mtime), None);
}

/// Codex thread #2, the `complete` trigger: every chunk of the first
/// attempt landed (so `finish(true)` would have already fired under the
/// PRE-fix ordering) before `complete` answered "gone". The retry re-sends
/// every chunk from 0 under a brand-new `FileProgress`. The run-wide
/// totals must reflect ONE finished file at its real size — not two
/// finished-file counts, and not double the ciphertext.
#[tokio::test]
async fn swept_at_complete_does_not_double_count_progress() {
    let s = Scratch::new("complete404-progress");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    let sid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.live.insert(sid.to_string());
    mock.complete_gone.insert(sid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));
    let progress = RecordingProgress::default();

    let out = run_with_progress(&state, spec(&path, Uuid::new_v4()), &progress)
        .await
        .expect("upload must recover");

    assert_eq!(
        progress.files_done.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "one logical file finished once, not twice (finish(true) must not fire before complete() succeeds)"
    );
    assert_eq!(
        progress.overall_bytes.load(std::sync::atomic::Ordering::Relaxed),
        out.ciphertext_bytes,
        "the failed attempt's fully-confirmed bytes must be rolled back, not left under the retry's own total"
    );
}

/// Codex thread #2, the chunk-PUT trigger: the resumed session accepts
/// its FIRST chunk, then goes gone on the second — a genuine MID-pipeline
/// sweep with a nonzero partial byte count reported before the failure.
/// Needs >1 chunk, so the payload crosses the 4 MiB minimum chunk size.
#[tokio::test]
async fn swept_mid_pipeline_does_not_double_count_progress() {
    let s = Scratch::new("midchunk-progress");
    let (path, size, mtime) = s.file(5 * 1024 * 1024);
    let stored_fid = Uuid::new_v4();
    let dead_sid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        dead_sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.live.insert(dead_sid.to_string());
    mock.dies_after_chunks.insert(dead_sid.to_string(), 1);
    let state: Shared = Arc::new(Mutex::new(mock));
    let progress = RecordingProgress::default();

    let out = run_with_progress(&state, spec(&path, Uuid::new_v4()), &progress)
        .await
        .expect("upload must recover");

    {
        let m = state.lock().unwrap();
        let dead_chunks: Vec<u32> = m
            .chunk_puts
            .iter()
            .filter(|c| c.0 == dead_sid.to_string())
            .map(|c| c.1)
            .collect();
        assert!(
            dead_chunks.len() >= 2,
            "premise: the dead session must take a successful chunk 0 before dying on chunk 1: {dead_chunks:?}"
        );
    }
    assert_eq!(
        progress.files_done.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "one logical file finished once"
    );
    assert_eq!(
        progress.overall_bytes.load(std::sync::atomic::Ordering::Relaxed),
        out.ciphertext_bytes,
        "chunk 0's bytes from the failed attempt must be rolled back before the retry re-sends everything from 0"
    );
}

#[tokio::test]
async fn reinit_happens_at_most_once_per_file_per_run() {
    let s = Scratch::new("once");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    let dead_sid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        dead_sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    // Every session, including the re-inited one, is swept at once.
    let state: Shared = Arc::new(Mutex::new(Mock {
        kill_new_sessions: true,
        ..Default::default()
    }));

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run(&state, spec(&path, Uuid::new_v4())),
    )
    .await
    .expect("must not loop");
    assert!(res.is_err(), "a second swept session is an error, not a retry");
    let m = state.lock().unwrap();
    assert_eq!(m.inits.len(), 1, "exactly one re-init: {:?}", m.inits);
    assert!(m.completed.is_empty());
    // The dead original record was cleared; the one left is the new
    // session's, so the next run gets its own single re-init.
    let left = crate::resume::resumable_upload(&path, size, mtime);
    assert!(left.is_some_and(|p| p.session_id != dead_sid), "left: {left:?}");
}

#[tokio::test]
async fn refused_stored_id_falls_back_to_one_fresh_id() {
    // 404 is the ONE status the v2 init handler returns that PROVES the
    // stored id is unusable (routes/uploads.rs: an explicit file_id the
    // caller can no longer read at all) — see `fresh_id_can_fix`. It is
    // the only status this fallback still fires on after Codex thread #1
    // (a 400 or a 5xx no longer does; see the two tests below).
    let s = Scratch::new("fallback");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.init_refuse.insert(stored_fid.to_string(), 404);
    let state: Shared = Arc::new(Mutex::new(mock));
    let fresh_fid = Uuid::new_v4();

    let out = run(&state, spec(&path, fresh_fid))
        .await
        .expect("fresh id must succeed");
    let m = state.lock().unwrap();
    assert_eq!(m.inits, vec![stored_fid.to_string(), fresh_fid.to_string()]);
    assert_eq!(out.server_id, fresh_fid);
    assert_eq!(m.completed.len(), 1);
}

/// Codex thread #1: a 400 from the stored-id init is GENERIC request
/// validation (file name / chunk-plan shape) that a fresh id would fail
/// identically — it says nothing about the id being bad, so (unlike 404)
/// it must be surfaced, never used to justify a fresh-id fallback.
#[tokio::test]
async fn refused_stored_id_400_is_surfaced_not_papered_over() {
    let s = Scratch::new("400-surfaced");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.init_refuse.insert(stored_fid.to_string(), 400);
    let state: Shared = Arc::new(Mutex::new(mock));

    let res = run(&state, spec(&path, Uuid::new_v4())).await;
    assert!(res.is_err(), "a 400 must be surfaced, not silently worked around");
    let m = state.lock().unwrap();
    assert_eq!(
        m.inits,
        vec![stored_fid.to_string()],
        "no fresh-id fallback for a 400: {:?}",
        m.inits
    );
}

/// Codex thread #1: a 5xx from the stored-id init proves nothing about the
/// id — it can be transient — so it must be retried with the SAME id
/// (bounded, with backoff), never swapped for a fresh one.
#[tokio::test]
async fn stored_id_5xx_is_retried_with_same_id_then_succeeds() {
    let s = Scratch::new("5xx-retry-succeeds");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    // Fails with 500 twice, then succeeds on the third attempt — well
    // within STORED_ID_INIT_RETRIES.
    mock.init_fail_then_succeed.insert(stored_fid.to_string(), (500, 2));
    let state: Shared = Arc::new(Mutex::new(mock));

    let out = run(&state, spec(&path, Uuid::new_v4()))
        .await
        .expect("must succeed once the transient 5xx clears, on the SAME id");
    let m = state.lock().unwrap();
    assert_eq!(
        m.inits,
        vec![stored_fid.to_string(); 3],
        "every retry must use the STORED id, never a fresh one: {:?}",
        m.inits
    );
    assert_eq!(out.server_id, stored_fid);
    assert_eq!(m.completed.len(), 1);
}

/// Codex thread #1 follow-up (found reviewing the fix at 3cb4daf): a
/// `--replace` upload's stored id EQUALS `spec.file_id` (both are the
/// existing target file's id — there is no OTHER id a replace could use),
/// so `fallback_file_id` is always `None` for it. The bounded SAME-id 5xx
/// retry must not be gated on a fallback id being available, or a
/// replace's re-init surfaces the FIRST transient 5xx instead of riding
/// it out like every other swept-session recovery does.
#[tokio::test]
async fn replace_upload_5xx_is_retried_with_same_id_even_with_no_fallback_available() {
    let s = Scratch::new("replace-5xx-retry");
    let (path, size, mtime) = s.file(64 * 1024);
    let target_fid = Uuid::new_v4();
    // The record a replace leaves behind names the row it lists (L13 L6:
    // not a self-minted create record).
    crate::resume::record_with_transport(
        &path,
        crate::item_id::Proposed::Listed(target_fid),
        None,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
        false,
    );
    let mut mock = Mock::default();
    mock.init_fail_then_succeed.insert(target_fid.to_string(), (500, 2));
    let state: Shared = Arc::new(Mutex::new(mock));

    // SAME id as the stored one — a replace can never fall back to a
    // fresh id, since it must land on this exact existing file.
    let mut replace_spec = spec(&path, target_fid);
    replace_spec.base_version_number = Some(3);

    let out = run(&state, replace_spec)
        .await
        .expect("a transient 5xx on a replace's stored-id re-init must be retried, not surfaced on the first one");
    let m = state.lock().unwrap();
    assert_eq!(
        m.inits,
        vec![target_fid.to_string(); 3],
        "every retry must reuse the SAME id — a replace has no fresh id to fall back to: {:?}",
        m.inits
    );
    assert_eq!(out.server_id, target_fid);
}

/// The bound: once every retry is spent, a persistent 5xx is surfaced —
/// still never falling back to a fresh id (which could leave a live
/// session under the stored id and complete a duplicate file under the
/// fresh one, per the review comment).
#[tokio::test]
async fn stored_id_5xx_exhausts_retries_and_is_surfaced() {
    let s = Scratch::new("5xx-exhausted");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.init_refuse.insert(stored_fid.to_string(), 503);
    let state: Shared = Arc::new(Mutex::new(mock));

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run(&state, spec(&path, Uuid::new_v4())),
    )
    .await
    .expect("bounded retries must not hang");
    assert!(res.is_err(), "a persistent 5xx must eventually be surfaced");
    let m = state.lock().unwrap();
    assert_eq!(
        m.inits,
        vec![stored_fid.to_string(); 1 + super::STORED_ID_INIT_RETRIES as usize],
        "exactly the initial attempt plus the bounded retries, all on the STORED id: {:?}",
        m.inits
    );
    assert!(m.completed.is_empty());
}

#[tokio::test]
async fn conflict_on_reinit_is_not_papered_over_with_a_fresh_id() {
    let s = Scratch::new("conflict");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    crate::resume::record(
        &path,
        stored_fid,
        Uuid::new_v4(),
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
    );
    let mut mock = Mock::default();
    mock.init_refuse.insert(stored_fid.to_string(), 409);
    let state: Shared = Arc::new(Mutex::new(mock));

    let res = run(&state, spec(&path, Uuid::new_v4())).await;
    assert!(res.is_err());
    let m = state.lock().unwrap();
    assert_eq!(
        m.inits,
        vec![stored_fid.to_string()],
        "no duplicate file under a fresh id"
    );
    // The dead record is gone either way.
    assert_eq!(crate::resume::resumable_upload(&path, size, mtime), None);
}

#[tokio::test]
async fn a_session_this_run_opened_is_not_reinited() {
    let s = Scratch::new("fresh");
    let (path, _size, _mtime) = s.file(64 * 1024);
    let state: Shared = Arc::new(Mutex::new(Mock {
        kill_new_sessions: true,
        ..Default::default()
    }));
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        run(&state, spec(&path, Uuid::new_v4())),
    )
    .await
    .expect("must not loop");
    assert!(res.is_err());
    assert_eq!(
        state.lock().unwrap().inits.len(),
        1,
        "no re-init for a session this run opened"
    );
}

#[test]
fn session_gone_classifier() {
    use crate::api::{ApiError, is_upload_session_gone};
    let e = |status: u16, msg: &str| ApiError {
        code: Some(msg.to_string()),
        message: msg.to_string(),
        status,
    };
    assert!(is_upload_session_gone(&e(404, "not found")));
    assert!(is_upload_session_gone(&e(
        400,
        "upload session is not writable: expired"
    )));
    assert!(!is_upload_session_gone(&e(
        400,
        "upload session is not writable: completed"
    )));
    assert!(!is_upload_session_gone(&e(
        400,
        "chunk index 9 out of range (expected 0..3)"
    )));
    assert!(!is_upload_session_gone(&e(
        409,
        "upload is already in progress for this file"
    )));
    assert!(!is_upload_session_gone(&e(500, "internal")));
    assert!(!is_upload_session_gone(&e(0, "Can't reach the Beebeeb API")));
}

/// Codex thread #1: 404 is the ONLY status that proves the stored id is
/// unusable (see `fresh_id_can_fix`'s doc comment for why each other
/// status is excluded, grounded in the v2 init handler).
#[test]
fn fresh_id_can_fix_classifier() {
    use super::fresh_id_can_fix;
    assert!(fresh_id_can_fix(404));
    assert!(
        !fresh_id_can_fix(400),
        "generic validation — a fresh id fails identically"
    );
    assert!(!fresh_id_can_fix(401), "auth — never a reason to change ids");
    assert!(!fresh_id_can_fix(403), "auth — never a reason to change ids");
    assert!(
        !fresh_id_can_fix(409),
        "a live upload or stale base version — a fresh id would create a duplicate"
    );
    assert!(!fresh_id_can_fix(402), "quota");
    assert!(!fresh_id_can_fix(413), "quota");
    assert!(!fresh_id_can_fix(429), "rate limiting, handled in `api`");
    assert!(
        !fresh_id_can_fix(500),
        "a 5xx proves nothing about the id — retried with the SAME id instead"
    );
    assert!(
        !fresh_id_can_fix(503),
        "a 5xx proves nothing about the id — retried with the SAME id instead"
    );
}

// ── Task 1943: the writer pin and the 1908 resume rule ─────────────────

use beebeeb_core::chunk_stream::{ChunkDecryptor, ChunkFormat, ExpectedChunkFormat};

/// The master key [`run`] uploads with.
fn run_mk() -> MasterKey {
    MasterKey::from_bytes([9u8; 32])
}

/// The chunk format frame 0 of `session` authenticates as, read by core's
/// pin-aware reader under `pin`; `None` if it does not open at all.
fn frame0_format(m: &Mock, session: &str, file_id: Uuid, pin: ExpectedChunkFormat) -> Option<ChunkFormat> {
    let frame = m.chunk_bodies.get(&(session.to_string(), 0))?;
    let mut dec = ChunkDecryptor::for_push(&run_mk(), &file_id.to_string(), pin);
    dec.push_frame(frame).ok()?;
    dec.format()
}

/// Spec 4: the new-file path NEVER hands `V2` to `with_recorded_pin` (a v2
/// file is only ever written by replacing a v2-pinned one), even when a
/// caller mistakenly fills in `replace_pin`; a replace takes the existing
/// file's pin.
#[test]
fn new_file_path_never_passes_v2_to_with_recorded_pin() {
    use ExpectedChunkFormat::{Unrecorded, V2};
    let p = std::path::Path::new("payload.bin");
    let mut new_file = spec(p, Uuid::new_v4());
    assert_eq!(super::writer_pin(&new_file), Unrecorded);
    new_file.replace_pin = Some(V2);
    assert_eq!(
        super::writer_pin(&new_file),
        Unrecorded,
        "a new file never gets a v2 pin"
    );

    let mut replace = spec(p, Uuid::new_v4());
    replace.base_version_number = Some(3);
    replace.replace_pin = Some(V2);
    assert_eq!(super::writer_pin(&replace), V2, "a v2-pinned file is replaced as v2");
    replace.replace_pin = Some(Unrecorded);
    assert_eq!(super::writer_pin(&replace), Unrecorded);
    replace.replace_pin = None;
    assert_eq!(super::writer_pin(&replace), Unrecorded);
}

/// End to end: a NEW file uploaded with a stray `replace_pin: V2` records no
/// pin in its metadata and is written in core's `WRITE_FORMAT` (v1).
#[tokio::test]
async fn new_file_upload_records_no_pin_and_writes_v1() {
    let s = Scratch::new("1943-new");
    let (path, _, _) = s.file(64 * 1024);
    let fid = Uuid::new_v4();
    let state: Shared = Arc::new(Mutex::new(Mock::default()));
    let mut new_file = spec(&path, fid);
    new_file.replace_pin = Some(ExpectedChunkFormat::V2);

    run(&state, new_file).await.expect("upload succeeds");

    let m = state.lock().unwrap();
    assert_eq!(
        crate::crypto::content_pin(&run_mk(), &fid.to_string(), &m.init_names[0]),
        Ok(ExpectedChunkFormat::Unrecorded)
    );
    let sid = &m.chunk_puts[0].0;
    assert_eq!(
        frame0_format(&m, sid, fid, ExpectedChunkFormat::Unrecorded),
        Some(ChunkFormat::V1Legacy)
    );
}

/// End to end: replacing a v2-pinned file writes v2 frames and keeps the v2
/// pin in the metadata (core: a v1 replace would complete and never open).
#[tokio::test]
async fn replace_of_a_v2_pinned_file_writes_v2_and_keeps_the_pin() {
    let s = Scratch::new("1943-replace");
    let (path, _, _) = s.file(64 * 1024);
    let fid = Uuid::new_v4();
    let state: Shared = Arc::new(Mutex::new(Mock::default()));
    let mut replace = spec(&path, fid);
    replace.base_version_number = Some(1);
    replace.replace_pin = Some(ExpectedChunkFormat::V2);

    run(&state, replace).await.expect("upload succeeds");

    let m = state.lock().unwrap();
    assert_eq!(
        crate::crypto::content_pin(&run_mk(), &fid.to_string(), &m.init_names[0]),
        Ok(ExpectedChunkFormat::V2)
    );
    let sid = &m.chunk_puts[0].0;
    assert_eq!(
        frame0_format(&m, sid, fid, ExpectedChunkFormat::V2),
        Some(ChunkFormat::V2)
    );
}

/// The resume rule (core task 1908 round 3, P1-A): an interrupted upload
/// whose session holds v1 frames, resumed by a writer that must write v2 (a
/// replace of a v2-pinned file), does NOT continue the old session. It
/// abandons it, never PUTs to it, inits the same file id fresh and uploads
/// from chunk 0 — a v2 file must not hold frames of two version ids.
#[tokio::test]
async fn v1_session_resumed_under_a_v2_pin_restarts_from_chunk_0() {
    let s = Scratch::new("1943-resume-v2");
    let (path, size, mtime) = s.file(64 * 1024);
    let fid = Uuid::new_v4();
    let old_sid = Uuid::new_v4();
    crate::resume::record(&path, fid, old_sid, ChunkFormat::V1Legacy, size, mtime);
    let mut mock = Mock::default();
    mock.live.insert(old_sid.to_string());
    mock.session_file.insert(old_sid.to_string(), fid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));
    let mut replace = spec(&path, fid);
    replace.base_version_number = Some(1);
    replace.replace_pin = Some(ExpectedChunkFormat::V2);

    let out = run(&state, replace).await.expect("upload succeeds");

    let m = state.lock().unwrap();
    assert!(
        m.chunk_puts.iter().all(|(sid, _)| *sid != old_sid.to_string()),
        "the v1 session must never be PUT to again: {:?}",
        m.chunk_puts
    );
    assert_eq!(m.abandons, vec![fid.to_string()], "the old session is abandoned first");
    assert_eq!(m.inits, vec![fid.to_string()], "then the SAME file id is inited fresh");
    let new_sid = m.chunk_puts[0].0.clone();
    let idx: Vec<u32> = m.chunk_puts.iter().map(|(_, i)| *i).collect();
    assert_eq!(idx[0], 0, "the restart uploads from chunk 0");
    assert!(idx.windows(2).all(|w| w[1] == w[0] + 1), "{idx:?}");
    assert_eq!(
        frame0_format(&m, &new_sid, fid, ExpectedChunkFormat::V2),
        Some(ChunkFormat::V2)
    );
    assert_eq!(m.completed, vec![new_sid]);
    assert_eq!(out.server_id, fid);
}

/// A record from an older CLI does not say its chunk format (and, L13, no
/// self-minted marker): it is abandoned and restarted under a FRESH own id —
/// never continued.
#[tokio::test]
async fn a_record_without_a_chunk_format_restarts_instead_of_continuing() {
    let s = Scratch::new("1943-resume-legacy");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    let old_sid = Uuid::new_v4();
    let canonical = std::fs::canonicalize(&path).unwrap().to_string_lossy().into_owned();
    std::fs::write(
        s.dir.join("pending.json"),
        json!({"uploads": {canonical: {
            "file_id": stored_fid, "upload_session_id": old_sid, "size": size, "mtime_ns": mtime as u64,
        }}})
        .to_string(),
    )
    .unwrap();
    let mut mock = Mock::default();
    mock.live.insert(old_sid.to_string());
    mock.session_file.insert(old_sid.to_string(), stored_fid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let fresh = Uuid::new_v4();
    run(&state, spec(&path, fresh)).await.expect("upload succeeds");

    let m = state.lock().unwrap();
    assert!(
        m.chunk_puts.iter().all(|(sid, _)| *sid != old_sid.to_string()),
        "{:?}",
        m.chunk_puts
    );
    assert_eq!(
        m.abandons,
        vec![stored_fid.to_string()],
        "the legacy session is abandoned"
    );
    assert_eq!(m.inits, vec![fresh.to_string()], "restarted under this run's own id");
    assert_eq!(m.chunk_puts[0].1, 0);
}

/// The one case core lets continue: a v1 session resumed by a v1 writer
/// keeps its session (no abandon, no init).
#[tokio::test]
async fn v1_session_with_a_v1_writer_continues_without_abandon() {
    let s = Scratch::new("1943-resume-v1");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    let sid = Uuid::new_v4();
    crate::resume::record(&path, stored_fid, sid, ChunkFormat::V1Legacy, size, mtime);
    let mut mock = Mock::default();
    mock.live.insert(sid.to_string());
    mock.session_file.insert(sid.to_string(), stored_fid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    run(&state, spec(&path, Uuid::new_v4())).await.expect("upload succeeds");

    let m = state.lock().unwrap();
    assert!(m.abandons.is_empty(), "{:?}", m.abandons);
    assert!(m.inits.is_empty(), "{:?}", m.inits);
    assert!(m.chunk_puts.iter().all(|(s, _)| *s == sid.to_string()));
    assert_eq!(m.completed, vec![sid.to_string()]);
}

// ── L13 (SF-0001): writes only under ids the client chose ───────────────

use crate::item_id::CHANGED_ID_MESSAGE;

/// A7 + L4: a new upload's init answers a different id. Abort BEFORE any
/// chunk is PUT, nothing completed, the session it opened ended once by our
/// id, and the message is plain.
#[tokio::test]
async fn init_echo_of_a_different_id_aborts_before_any_chunk_put() {
    let s = Scratch::new("a7-init");
    let (path, ..) = s.file(64 * 1024);
    let mine = Uuid::new_v4();
    let other = Uuid::new_v4();
    let state: Shared = Arc::new(Mutex::new(Mock {
        init_echo: Some(other.to_string()),
        ..Default::default()
    }));

    let err = run(&state, spec(&path, mine)).await.expect_err("must abort");
    assert_eq!(err.to_string(), CHANGED_ID_MESSAGE);
    let m = state.lock().unwrap();
    assert_eq!(m.inits, vec![mine.to_string()], "the client's own id was proposed");
    assert!(
        m.chunk_puts.is_empty(),
        "no chunk under the echoed id: {:?}",
        m.chunk_puts
    );
    assert!(m.completed.is_empty());
    assert_eq!(
        m.abandons,
        vec![mine.to_string()],
        "the session it opened is ended, by our owner id, exactly once"
    );
}

/// A7 + L4 for a replace: the listed row's id is sent, and a different echo
/// aborts before any chunk PUT (nothing new is sealed under the echo); the
/// session it opened is ended once, by the listed row's id.
#[tokio::test]
async fn replace_init_echo_of_a_different_id_aborts_before_any_chunk_put() {
    let s = Scratch::new("a7-replace");
    let (path, ..) = s.file(64 * 1024);
    let listed = Uuid::new_v4();
    let mut replace_spec = spec(&path, listed);
    replace_spec.base_version_number = Some(2);
    replace_spec.replace_pin = Some(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded);
    replace_spec.replace_file_key = Some(Arc::new(beebeeb_core::kdf::FileKey::from_bytes([3u8; 32])));
    let state: Shared = Arc::new(Mutex::new(Mock {
        init_echo: Some(Uuid::new_v4().to_string()),
        ..Default::default()
    }));

    let err = run(&state, replace_spec).await.expect_err("must abort");
    assert_eq!(err.to_string(), CHANGED_ID_MESSAGE);
    let m = state.lock().unwrap();
    assert_eq!(m.inits, vec![listed.to_string()], "the listed row's id was sent");
    assert!(m.chunk_puts.is_empty());
    assert!(m.completed.is_empty());
    assert_eq!(m.abandons, vec![listed.to_string()], "exactly one abandon");
}

/// A7 + L4 + L5: the completion answer names a different item. Abort with
/// the message and end the session once, by our id. The resume record is
/// KEPT, so the next run stays on this client's own id: it never mints a
/// fresh one and uploads the file a second time beside the first.
#[tokio::test]
async fn completion_echo_of_a_different_id_aborts_and_keeps_the_retry_on_our_id() {
    let s = Scratch::new("a7-complete");
    let (path, size, mtime) = s.file(64 * 1024);
    let mine = Uuid::new_v4();
    let state: Shared = Arc::new(Mutex::new(Mock {
        complete_echo: Some(Uuid::new_v4().to_string()),
        ..Default::default()
    }));

    let err = run(&state, spec(&path, mine)).await.expect_err("must abort");
    assert_eq!(err.to_string(), CHANGED_ID_MESSAGE);
    {
        let m = state.lock().unwrap();
        assert_eq!(m.completed.len(), 1, "the session was completed before the check");
        assert_eq!(
            m.abandons,
            vec![mine.to_string()],
            "the mislabelled session is ended exactly once, by our id"
        );
    }
    assert_eq!(
        crate::resume::resumable_upload(&path, size, mtime).map(|p| p.file_id),
        Some(mine),
        "the record still names this client's id"
    );

    // The next run (the server answers honestly now) gets a fresh id from
    // its caller, as every sync pass does. It must not use it.
    state.lock().unwrap().complete_echo = None;
    let fresh = Uuid::new_v4();
    let outcome = run(&state, spec(&path, fresh)).await.expect("the retry completes");
    assert_eq!(outcome.server_id, mine);
    let m = state.lock().unwrap();
    assert!(
        m.inits.iter().all(|id| *id == mine.to_string()),
        "every init is under our first id, none under a new one: {:?}",
        m.inits
    );
    assert_eq!(
        crate::resume::resumable_upload(&path, size, mtime),
        None,
        "cleared on success"
    );
}

/// A7c: a persisted create session from a pre-update CLI (no self-minted
/// marker) is abandoned and restarted under this run's own fresh id.
#[tokio::test]
async fn legacy_record_without_a_marker_is_abandoned_and_restarted() {
    let s = Scratch::new("a7c-legacy");
    let (path, size, mtime) = s.file(64 * 1024);
    let legacy_fid = Uuid::new_v4();
    let legacy_sid = Uuid::new_v4();
    write_raw_record(&s, &path, legacy_fid, legacy_sid, size, mtime, None);
    let mut mock = Mock::default();
    mock.live.insert(legacy_sid.to_string());
    mock.session_file.insert(legacy_sid.to_string(), legacy_fid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let fresh = Uuid::new_v4();
    run(&state, spec(&path, fresh)).await.expect("restarts and succeeds");

    let m = state.lock().unwrap();
    assert_eq!(
        m.abandons,
        vec![legacy_fid.to_string()],
        "the legacy session is abandoned"
    );
    assert_eq!(m.inits, vec![fresh.to_string()], "restarted under the client's own id");
    assert!(
        m.chunk_puts.iter().all(|(sid, _)| *sid != legacy_sid.to_string()),
        "never resumed: {:?}",
        m.chunk_puts
    );
    assert_eq!(m.completed.len(), 1);
}

/// A7c: a record whose self-minted marker names a DIFFERENT id is treated
/// the same way — abandoned, never resumed.
#[tokio::test]
async fn record_with_a_different_marker_is_abandoned_and_restarted() {
    let s = Scratch::new("a7c-other-marker");
    let (path, size, mtime) = s.file(64 * 1024);
    let stored_fid = Uuid::new_v4();
    let sid = Uuid::new_v4();
    write_raw_record(&s, &path, stored_fid, sid, size, mtime, Some(Uuid::new_v4()));
    let mut mock = Mock::default();
    mock.live.insert(sid.to_string());
    mock.session_file.insert(sid.to_string(), stored_fid.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let fresh = Uuid::new_v4();
    run(&state, spec(&path, fresh)).await.expect("restarts and succeeds");
    let m = state.lock().unwrap();
    assert_eq!(m.abandons, vec![stored_fid.to_string()]);
    assert_eq!(m.inits, vec![fresh.to_string()]);
}

/// L7a: an explicit replacement whose pre-update record names the SAME row
/// ends that session before the record is dropped — the record is not this
/// write's session and would otherwise 409 every retry.
#[tokio::test]
async fn a_replace_whose_record_names_the_listed_row_abandons_it_first() {
    let s = Scratch::new("l7a-replace-record");
    let (path, size, mtime) = s.file(64 * 1024);
    let listed = Uuid::new_v4();
    let stale_sid = Uuid::new_v4();
    write_raw_record(&s, &path, listed, stale_sid, size, mtime, None);
    let mut mock = Mock::default();
    mock.live.insert(stale_sid.to_string());
    mock.session_file.insert(stale_sid.to_string(), listed.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let mut replace_spec = spec(&path, listed);
    replace_spec.base_version_number = Some(2);
    replace_spec.replace_pin = Some(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded);
    replace_spec.replace_file_key = Some(Arc::new(beebeeb_core::kdf::FileKey::from_bytes([3u8; 32])));
    run(&state, replace_spec).await.expect("the replacement uploads");

    let m = state.lock().unwrap();
    assert_eq!(
        m.abandons,
        vec![listed.to_string()],
        "the listed row's stale session is ended, once, before the record is dropped"
    );
    assert!(
        m.chunk_puts.iter().all(|(sid, _)| *sid != stale_sid.to_string()),
        "the stale session is never PUT to: {:?}",
        m.chunk_puts
    );
    assert_eq!(m.completed.len(), 1);
}

/// L7b: the abandon runs BEFORE the record is cleared, and a transient
/// failure keeps the record and fails the file for this run (an upload now
/// would overwrite the record, and nothing would end that session). The
/// next run retries the abandon, then uploads under its own fresh id.
#[tokio::test]
async fn a_transient_abandon_failure_keeps_the_record_for_the_next_run() {
    let s = Scratch::new("l7b-abandon-503");
    let (path, size, mtime) = s.file(64 * 1024);
    let legacy_fid = Uuid::new_v4();
    let legacy_sid = Uuid::new_v4();
    write_raw_record(&s, &path, legacy_fid, legacy_sid, size, mtime, None);
    let mut mock = Mock::default();
    mock.live.insert(legacy_sid.to_string());
    mock.session_file.insert(legacy_sid.to_string(), legacy_fid.to_string());
    mock.abandon_status = Some(503);
    let state: Shared = Arc::new(Mutex::new(mock));

    // As every caller does, each run brings a fresh id of its own.
    let first = Uuid::new_v4();
    let err = run(&state, spec(&path, first))
        .await
        .expect_err("the session could not be ended");
    assert!(err.to_string().contains("tried again next run"), "{err}");
    assert!(
        crate::resume::unresumable_upload(&path, size, mtime, None).is_some(),
        "the record is kept so the next run retries the abandon"
    );
    assert!(state.lock().unwrap().inits.is_empty(), "nothing is uploaded over it");

    state.lock().unwrap().abandon_status = None;
    let second = Uuid::new_v4();
    run(&state, spec(&path, second)).await.expect("retried and completed");
    assert!(
        crate::resume::unresumable_upload(&path, size, mtime, None).is_none(),
        "cleared only once the abandon succeeded"
    );
    let m = state.lock().unwrap();
    assert_eq!(
        m.abandons,
        vec![legacy_fid.to_string(), legacy_fid.to_string()],
        "one abandon attempt per run"
    );
    assert_eq!(
        m.inits,
        vec![second.to_string()],
        "uploaded under the second run's own id"
    );
    assert_eq!(m.completed.len(), 1);
}

/// L6: an interrupted REPLACE leaves a record that names the row it lists,
/// an id this client did not mint. A later create of the same path never
/// resumes it: it ends that session and uploads under its own id.
#[tokio::test]
async fn an_interrupted_replace_record_is_not_resumed_by_a_later_create() {
    let s = Scratch::new("l6-replace-then-create");
    let (path, size, mtime) = s.file(64 * 1024);
    let listed = Uuid::new_v4();
    let replace_sid = Uuid::new_v4();
    crate::resume::record_with_transport(
        &path,
        crate::item_id::Proposed::Listed(listed),
        None,
        replace_sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
        false,
    );
    let mut mock = Mock::default();
    mock.live.insert(replace_sid.to_string());
    mock.session_file.insert(replace_sid.to_string(), listed.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let fresh = Uuid::new_v4();
    run(&state, spec(&path, fresh)).await.expect("the create uploads");
    let m = state.lock().unwrap();
    assert!(
        m.chunk_puts.iter().all(|(sid, _)| *sid != replace_sid.to_string()),
        "the replace's session is never PUT to: {:?}",
        m.chunk_puts
    );
    assert_eq!(m.inits, vec![fresh.to_string()], "the create runs under its own id");
    assert_eq!(
        m.abandons,
        vec![listed.to_string()],
        "the replace's session is ended once"
    );
    assert_eq!(m.completed.len(), 1);
}

/// L6: the same record IS resumed by a replace of the row it names.
#[tokio::test]
async fn an_interrupted_replace_record_is_resumed_by_the_same_replace() {
    let s = Scratch::new("l6-replace-resume");
    let (path, size, mtime) = s.file(64 * 1024);
    let listed = Uuid::new_v4();
    let replace_sid = Uuid::new_v4();
    crate::resume::record_with_transport(
        &path,
        crate::item_id::Proposed::Listed(listed),
        None,
        replace_sid,
        beebeeb_core::chunk_stream::ChunkFormat::V1Legacy,
        size,
        mtime,
        false,
    );
    let mut mock = Mock::default();
    mock.live.insert(replace_sid.to_string());
    mock.session_file.insert(replace_sid.to_string(), listed.to_string());
    let state: Shared = Arc::new(Mutex::new(mock));

    let mut replace_spec = spec(&path, listed);
    replace_spec.base_version_number = Some(2);
    replace_spec.replace_pin = Some(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded);
    replace_spec.replace_file_key = Some(Arc::new(beebeeb_core::kdf::FileKey::from_bytes([3u8; 32])));
    run(&state, replace_spec).await.expect("the replacement resumes");
    let m = state.lock().unwrap();
    assert!(m.inits.is_empty(), "resumed, not re-inited: {:?}", m.inits);
    assert!(m.abandons.is_empty());
    assert!(m.chunk_puts.iter().all(|(sid, _)| *sid == replace_sid.to_string()));
    assert_eq!(m.completed, vec![replace_sid.to_string()]);
}

/// Write exactly what a pre-update CLI left behind (or a record whose marker
/// names another id): the raw sidecar, no `self_minted` marker unless given.
fn write_raw_record(
    s: &Scratch,
    path: &std::path::Path,
    file_id: Uuid,
    session_id: Uuid,
    size: u64,
    mtime_ns: u128,
    marker: Option<Uuid>,
) {
    let mut entry = serde_json::json!({
        "file_id": file_id,
        "upload_session_id": session_id,
        "chunk_format": beebeeb_core::chunk_stream::ChunkFormat::V1Legacy.as_u8(),
        "size": size,
        "mtime_ns": mtime_ns,
    });
    if let Some(m) = marker {
        entry["self_minted"] = serde_json::json!(m);
    }
    let db = serde_json::json!({"uploads": {crate::resume::key(path): entry}});
    std::fs::write(s.dir.join("pending.json"), db.to_string()).unwrap();
}
