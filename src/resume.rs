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
//! Stored at `<config_dir>/beebeeb/pending-uploads.json`. A process-global mutex
//! serialises the load-modify-save so the concurrent (`buffer_unordered`) sync
//! upload tasks can't clobber each other's entries.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

static LOCK: Mutex<()> = Mutex::new(());

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
    size: u64,
    mtime_ns: u128,
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

/// Canonical-path key. Falls back to the raw path if the file can't be
/// canonicalised (e.g. it was removed) so the key stays stable.
fn key(path: &Path) -> String {
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

/// Return the stored in-progress `(file_id, upload_session_id)` for `path`, but
/// only if the recorded `(size, mtime_ns)` still match AND a v2 session id is
/// present — otherwise the local content changed (stale partial) or the entry
/// predates the v2 driver, and the caller mints a fresh id + session.
pub fn resumable_upload(path: &Path, size: u64, mtime_ns: u128) -> Option<(Uuid, Uuid)> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    load()
        .uploads
        .get(&key(path))
        .filter(|e| e.size == size && e.mtime_ns == mtime_ns)
        .and_then(|e| e.upload_session_id.map(|sid| (e.file_id, sid)))
}

/// Record an in-progress upload so an interrupted run can resume it. Called
/// once the v2 session is open, before the chunk pipeline starts; idempotent
/// (overwrites any prior entry).
pub fn record(path: &Path, file_id: Uuid, upload_session_id: Uuid, size: u64, mtime_ns: u128) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut db = load();
    db.uploads.insert(
        key(path),
        PendingEntry {
            file_id,
            upload_session_id: Some(upload_session_id),
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

        // Record, then an exact (size, mtime) match resumes with both ids.
        record(&f, id, sid, 5, 100);
        assert_eq!(resumable_upload(&f, 5, 100), Some((id, sid)));

        // A changed size or mtime must NOT resume onto stale chunks.
        assert_eq!(resumable_upload(&f, 6, 100), None);
        assert_eq!(resumable_upload(&f, 5, 101), None);

        // Re-record overwrites with a new id + session.
        let id2 = Uuid::new_v4();
        let sid2 = Uuid::new_v4();
        record(&f, id2, sid2, 5, 100);
        assert_eq!(resumable_upload(&f, 5, 100), Some((id2, sid2)));

        // Clear drops it.
        clear(&f);
        assert_eq!(resumable_upload(&f, 5, 100), None);

        unsafe {
            std::env::remove_var("BB_PENDING_UPLOADS_PATH");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
