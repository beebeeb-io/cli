//! Resumable-upload sidecar.
//!
//! `bb push`/`bb sync` mint a fresh `file_id` on every run, so an upload that is
//! interrupted part-way (process killed, link dropped) would otherwise restart
//! from byte 0 next time — a brand-new `file_id` with zero server-side chunks.
//!
//! This sidecar persists the in-progress `file_id` **and the v2
//! `upload_session_id`** keyed by the file's canonical path. On the next run,
//! [`resumable_upload`] returns that pair **iff** the file's `(size, mtime)` are
//! unchanged — the same signal `bb sync` itself uses for change detection — so
//! changed content never resumes onto stale chunks. The upload driver then skips
//! `upload_init` and re-PUTs every chunk to the stored session; the v2 chunk
//! route is idempotent (an already-stored chunk returns `{skipped:true}` without
//! re-writing the blob), so the server short-circuits the ones it already has
//! (see `upload::stream_encrypt_upload`).
//!
//! Each record also keeps the **chunk format** of the frames the session holds
//! (task 1943): core's resume rule (`chunk_stream::resume_policy`, task 1908)
//! lets an upload continue its old session only when those frames and the new
//! encryptor are both v1. A v2 upload's frames all bind ONE version id (frame
//! 0's nonce), so continuing would store frames of two versions side by side.
//! A record without a format (an older CLI) is restarted, never continued.
//!
//! Stored at `<config_dir>/beebeeb/pending-uploads.json`. A process-global mutex
//! serialises the load-modify-save so the concurrent (`buffer_unordered`) sync
//! upload tasks can't clobber each other's entries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use beebeeb_core::chunk_stream::ChunkFormat;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static LOCK: Mutex<()> = Mutex::new(());

/// Test-only: serialises every test that points `BB_PENDING_UPLOADS_PATH` at a
/// scratch file (a process-global env var), across modules.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize, Default)]
struct PendingDb {
    #[serde(default)]
    uploads: HashMap<String, PendingEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct PendingEntry {
    file_id: Uuid,
    /// The v2 upload session opened for this in-progress upload. Absent on
    /// entries written by an older CLI (v1 file_id-only); such an entry can't be
    /// resumed onto the v2 path, so it is treated as non-resumable.
    #[serde(default)]
    upload_session_id: Option<Uuid>,
    /// `ChunkFormat::as_u8` of the frames this session holds. Absent on entries
    /// written before task 1943, and an unknown code reads as absent: both make
    /// core's resume rule restart the upload.
    #[serde(default)]
    chunk_format: Option<u8>,
    /// L13 (SF-0001): the id this client minted for this write (a create).
    /// `Some(file_id)` only when THIS CLI generated it; a record written before
    /// the update (or one whose marker names another id) is never resumed by a
    /// create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    self_minted: Option<Uuid>,
    /// L13 (SF-0001 round 2, L6): the listed row this write REPLACES (an id this
    /// client did not mint). Only a replace of that same row may resume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replaces: Option<Uuid>,
    /// Task 2173: the session is a direct one (`object-upload-v1`). A direct session takes no proxy
    /// chunk PUT and an API session takes no grant, so a resumed upload must continue on the
    /// transport it began on. Absent on records from an older CLI, which only had the API route.
    #[serde(default)]
    direct: bool,
    size: u64,
    mtime_ns: u128,
}

/// A resumable in-progress upload: see [`resumable_upload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingUpload {
    pub file_id: Uuid,
    pub session_id: Uuid,
    /// The chunk format of the frames the session already holds, as recorded
    /// when it was opened; `None` = the record does not say (restart).
    pub chunk_format: Option<ChunkFormat>,
    /// The session was opened as a direct upload (task 2173); see [`PendingEntry`].
    pub direct: bool,
}

fn db_path() -> PathBuf {
    // Internal test seam: lets unit tests point the sidecar at a temp file
    // instead of the user's real config dir.
    if let Ok(p) = std::env::var("BB_PENDING_UPLOADS_PATH") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beebeeb")
        .join("pending-uploads.json")
}

/// The directory holding the resume state (and the upload locks of
/// `upload_lock`, which follow the same test seam).
pub(crate) fn state_dir() -> PathBuf {
    db_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Canonical-path key. Falls back to the raw path if the file can't be
/// canonicalised (e.g. it was removed) so the key stays stable.
pub(crate) fn key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn load() -> PendingDb {
    std::fs::read(db_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(db: &PendingDb) {
    let path = db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(db) {
        let _ = std::fs::write(&path, bytes);
    }
}

/// Whether the write asking (`replacing`: the listed row a replace versions,
/// `None` for a create) may continue `e`'s session. L13 (SF-0001): a create
/// continues only a create record whose id this client minted; a replace only
/// this CLI's replace record of the same row. A record from a pre-update CLI
/// (neither marker) continues nothing.
fn resumable_by(e: &PendingEntry, replacing: Option<Uuid>) -> bool {
    match replacing {
        None => e.self_minted == Some(e.file_id),
        Some(row) => e.file_id == row && e.replaces == Some(row),
    }
}

/// Return the stored in-progress upload of a CREATE for `path`, but only if the
/// recorded `(size, mtime_ns)` still match, a v2 session id is present, and the
/// record carries L13's self-minted marker naming its own id — otherwise the
/// local content changed (stale partial), the entry predates the v2 driver, or
/// it is not a create this client minted (a pre-update CLI's record, or a
/// replace's), and the caller mints a fresh id + session (abandoning the old one
/// through [`unresumable_upload`]). Whether the session may then be CONTINUED is
/// core's resume rule, applied by the caller to `chunk_format`.
pub fn resumable_upload(path: &Path, size: u64, mtime_ns: u128) -> Option<PendingUpload> {
    resumable_upload_for(path, size, mtime_ns, None)
}

/// [`resumable_upload`] for the write that asks: `replacing` is the listed row a
/// replace versions (`None` for a create). See [`resumable_by`].
pub fn resumable_upload_for(path: &Path, size: u64, mtime_ns: u128, replacing: Option<Uuid>) -> Option<PendingUpload> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load()
        .uploads
        .get(&key(path))
        .filter(|e| e.size == size && e.mtime_ns == mtime_ns && resumable_by(e, replacing))
        .and_then(PendingUpload::from_entry)
}

/// L13 (SF-0001): the record for `path` (same `(size, mtime_ns)` signature) that
/// the write asking may NOT resume (see [`resumable_by`]): a record from a
/// pre-update CLI (no marker), one whose marker names a different id, or another
/// write's (a replace record met by a create, a create record met by a replace,
/// or a replace of another row). The caller ends or discards it and starts under
/// its own id; it is never resumed and never left looping.
pub fn unresumable_upload(path: &Path, size: u64, mtime_ns: u128, replacing: Option<Uuid>) -> Option<PendingUpload> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load()
        .uploads
        .get(&key(path))
        .filter(|e| e.size == size && e.mtime_ns == mtime_ns && !resumable_by(e, replacing))
        .and_then(PendingUpload::from_entry)
}

impl PendingUpload {
    fn from_entry(e: &PendingEntry) -> Option<Self> {
        e.upload_session_id.map(|session_id| PendingUpload {
            file_id: e.file_id,
            session_id,
            chunk_format: e.chunk_format.and_then(|c| ChunkFormat::from_u8(c).ok()),
            direct: e.direct,
        })
    }
}

/// [`resumable_upload`] for `path` as it is on disk right now (same size and
/// mtime derivation as the upload driver). `None` if it cannot be stat'ed.
pub fn resumable_upload_now(path: &Path) -> Option<PendingUpload> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    resumable_upload(path, meta.len(), mtime_ns)
}

/// Record an in-progress upload so an interrupted run can resume it. Called
/// once the v2 session is open, before the chunk pipeline starts; idempotent
/// (overwrites any prior entry). `chunk_format` is the session encryptor's
/// `ChunkEncryptor::format`.
#[cfg(test)]
pub fn record(
    path: &Path,
    file_id: Uuid,
    upload_session_id: Uuid,
    chunk_format: ChunkFormat,
    size: u64,
    mtime_ns: u128,
) {
    record_with_transport(
        path,
        crate::item_id::Proposed::Minted(file_id),
        upload_session_id,
        chunk_format,
        size,
        mtime_ns,
        false,
    );
}

/// [`record`] for the write `proposed` names (a create's minted id, or the row a
/// replace lists: L13 marks only the first as self-minted), saying whether the
/// session is a direct upload (task 2173).
pub fn record_with_transport(
    path: &Path,
    proposed: crate::item_id::Proposed,
    upload_session_id: Uuid,
    chunk_format: ChunkFormat,
    size: u64,
    mtime_ns: u128,
    direct: bool,
) {
    use crate::item_id::Proposed;
    let file_id = proposed.id();
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = load();
    db.uploads.insert(
        key(path),
        PendingEntry {
            file_id,
            self_minted: matches!(proposed, Proposed::Minted(_)).then_some(file_id),
            replaces: matches!(proposed, Proposed::Listed(_)).then_some(file_id),
            upload_session_id: Some(upload_session_id),
            chunk_format: Some(chunk_format.as_u8()),
            direct,
            size,
            mtime_ns,
        },
    );
    save(&db);
}

/// Drop the record for `path` — on successful completion, or when abandoning a
/// stale partial the server no longer has in progress.
pub fn clear(path: &Path) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = load();
    if db.uploads.remove(&key(path)).is_some() {
        save(&db);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // All assertions live in ONE test: the sidecar path is set via a
    // process-global env var, so splitting across parallel tests would race.
    #[test]
    fn record_lookup_clear_with_signature_guard() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("bb-resume-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("pending.json");
        // SAFETY: single-threaded within this test; no other code reads the var.
        unsafe {
            std::env::set_var("BB_PENDING_UPLOADS_PATH", &db);
        }
        let _ = std::fs::remove_file(&db);

        let f = dir.join("file.bin");
        std::fs::write(&f, b"hello").unwrap();
        let id = Uuid::new_v4();
        let sid = Uuid::new_v4();

        // Nothing recorded yet.
        assert_eq!(resumable_upload(&f, 5, 100), None);

        // Record, then an exact (size, mtime) match resumes with both ids and
        // the session's chunk format.
        let pending = |file_id, session_id, chunk_format| PendingUpload {
            file_id,
            session_id,
            chunk_format,
            direct: false,
        };
        record(&f, id, sid, ChunkFormat::V1Legacy, 5, 100);
        assert_eq!(
            resumable_upload(&f, 5, 100),
            Some(pending(id, sid, Some(ChunkFormat::V1Legacy)))
        );

        // Task 2173: a direct session is remembered as one (a proxy PUT would be refused on it).
        record_with_transport(
            &f,
            crate::item_id::Proposed::Minted(id),
            sid,
            ChunkFormat::V1Legacy,
            5,
            100,
            true,
        );
        assert_eq!(
            resumable_upload(&f, 5, 100),
            Some(PendingUpload {
                direct: true,
                ..pending(id, sid, Some(ChunkFormat::V1Legacy))
            })
        );
        // (The older-record case, with no `direct` field at all, is the raw-JSON loop below: it reads as an API session.)
        record(&f, id, sid, ChunkFormat::V1Legacy, 5, 100);

        // A changed size or mtime must NOT resume onto stale chunks.
        assert_eq!(resumable_upload(&f, 6, 100), None);
        assert_eq!(resumable_upload(&f, 5, 101), None);

        // Re-record overwrites with a new id + session.
        let id2 = Uuid::new_v4();
        let sid2 = Uuid::new_v4();
        record(&f, id2, sid2, ChunkFormat::V2, 5, 100);
        assert_eq!(
            resumable_upload(&f, 5, 100),
            Some(pending(id2, sid2, Some(ChunkFormat::V2)))
        );

        // Clear drops it.
        clear(&f);
        assert_eq!(resumable_upload(&f, 5, 100), None);

        // L13 (SF-0001): this CLI's own records carry the self-minted marker,
        // so they stay resumable.
        record(&f, id, sid, ChunkFormat::V1Legacy, 5, 100);
        assert_eq!(unresumable_upload(&f, 5, 100, None), None);
        let raw_own = serde_json::to_value(load().uploads.get(&key(&f)).unwrap()).unwrap();
        assert_eq!(raw_own["self_minted"], serde_json::json!(id));

        // Task 1943: a record from an older CLI (no `chunk_format`) or with a
        // code this CLI does not know still resumes the ids, but WITHOUT a
        // format, which core's resume rule restarts — provided it carries
        // L13's self-minted marker.
        let key = key(&f);
        for raw in [
            serde_json::json!({"file_id": id, "upload_session_id": sid, "self_minted": id, "size": 5, "mtime_ns": 100}),
            serde_json::json!({"file_id": id, "upload_session_id": sid, "self_minted": id, "chunk_format": 9, "size": 5, "mtime_ns": 100}),
        ] {
            std::fs::write(&db, serde_json::json!({"uploads": {&key: raw}}).to_string()).unwrap();
            assert_eq!(resumable_upload(&f, 5, 100), Some(pending(id, sid, None)), "{raw}");
        }

        // L13 (SF-0001): a pre-update record (no marker) or one whose marker
        // names a different id is NEVER resumable — the caller abandons it.
        for raw in [
            serde_json::json!({"file_id": id, "upload_session_id": sid, "size": 5, "mtime_ns": 100}),
            serde_json::json!({"file_id": id, "upload_session_id": sid, "self_minted": Uuid::new_v4(), "size": 5, "mtime_ns": 100}),
        ] {
            std::fs::write(&db, serde_json::json!({"uploads": {&key: raw}}).to_string()).unwrap();
            assert_eq!(resumable_upload(&f, 5, 100), None, "must not resume: {raw}");
            assert_eq!(
                unresumable_upload(&f, 5, 100, None),
                Some(pending(id, sid, None)),
                "must be found for abandoning: {raw}"
            );
        }
        // A changed signature is not a stale record, just a different file state.
        assert_eq!(unresumable_upload(&f, 6, 100, None), None);

        // L13 round 2 (L6): a REPLACE record names a row this client listed,
        // not one it minted. It is never marked self-minted, a create never
        // resumes it, and only a replace of that same row does.
        let row = Uuid::new_v4();
        record_with_transport(
            &f,
            crate::item_id::Proposed::Listed(row),
            sid,
            ChunkFormat::V1Legacy,
            5,
            100,
            false,
        );
        let raw_replace = serde_json::to_value(load().uploads.get(&key).unwrap()).unwrap();
        assert!(raw_replace.get("self_minted").is_none(), "{raw_replace}");
        assert_eq!(raw_replace["replaces"], serde_json::json!(row));
        assert_eq!(resumable_upload(&f, 5, 100), None, "a create never resumes a replace");
        assert_eq!(
            unresumable_upload(&f, 5, 100, None),
            Some(pending(row, sid, Some(ChunkFormat::V1Legacy)))
        );
        assert_eq!(
            resumable_upload_for(&f, 5, 100, Some(row)),
            Some(pending(row, sid, Some(ChunkFormat::V1Legacy))),
            "the replace of the same row resumes it"
        );
        assert_eq!(resumable_upload_for(&f, 5, 100, Some(Uuid::new_v4())), None);
        // ...and a replace never resumes a create record (nor a pre-update one)
        // of its row.
        record(&f, row, sid, ChunkFormat::V1Legacy, 5, 100);
        assert_eq!(resumable_upload_for(&f, 5, 100, Some(row)), None);
        assert!(unresumable_upload(&f, 5, 100, Some(row)).is_some());

        unsafe {
            std::env::remove_var("BB_PENDING_UPLOADS_PATH");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
