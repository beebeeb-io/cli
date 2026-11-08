//! What the driver does when the caller that reads a file in place (`ChunkProgress::before_complete`, LD-0113, LD-0137)
//! says the file changed while it was read: ask, end the session, drop the record.
//!
//! Owns: the three steps. Must not: decide when to ask (that is `upload_attempt`).
//! Tests: `source_changed_tests`.

use super::*;

/// The caller's words when it says (`ChunkProgress::before_complete`) that the file at `path` changed while it was
/// read; `None` when it does not (the default for `push` and `sync`).
pub(super) fn source_changed(progress: &dyn ChunkProgress, path: &Path) -> Option<String> {
    progress.before_complete(path).err()
}

/// Before this attempt's init: `Some(error to stop with)` when the caller says the file changed. A fresh attempt has no
/// session yet; a RESUMED one does (and its record), and it is ended like any other that can no longer complete.
pub(super) async fn changed_before_init(
    api: &ApiClient,
    progress: &dyn ChunkProgress,
    path: &Path,
    file_id: &str,
    resumed: bool,
) -> Option<AttemptErr> {
    let why = source_changed(progress, path)?;
    if resumed {
        end_session(api, path, file_id).await;
    }
    Some(AttemptErr::Other(why))
}

/// Ends the attempt's session, so nothing completes and no lease is left to refuse the next run, and drops its
/// resume record (the file is no longer the one it was made for).
pub(super) async fn end_session(api: &ApiClient, path: &Path, file_id: &str) {
    let _ = api.upload_abandon(file_id).await;
    crate::resume::clear(path);
}
