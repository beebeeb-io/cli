//! What a re-run of `bb push` finds already in place: this machine's own upload of the same local file (task 2334).
//!
//! A re-pushed folder is reused, so every file the earlier run touched meets its own upload as a same-name
//! file. Two of those are not conflicts:
//!
//! - an INTERRUPTED first upload of this exact file (task 1950): the resume record names that id, and the
//!   server still has it uploading with no completed version. The push resumes it;
//! - a FINISHED upload of this exact file, unchanged on both sides (lead ruling, 2026-11-06): the finished
//!   record (`resume::finished_upload`) says this file, at this size and mtime, went into this folder as some
//!   id, and the server's row for that id is still there, not trashed, completed at the version that upload
//!   made, with the same plaintext size. The push skips it and counts it. That id may be the same-name file or
//!   a `--keep-both` copy beside it. The CLI writes no content hash a server row carries, so this record is the
//!   proof.
//!
//! Anything else, including a row that cannot be read now, is not proven to be ours: the usual conflict rule
//! applies, so a changed file is never skipped silently. The row is read at most once, and handed on to the
//! upload driver so it does not read it again.

use uuid::Uuid;

use crate::api::ApiClient;

/// A same-name file a push meets in the folder it writes into.
pub(crate) enum Existing {
    /// Not proven to be this push's own: the usual conflict rule applies.
    Other,
    /// This machine's interrupted first upload of this file: resume it. The row as it was just read (`None`
    /// when its folder could not be read from it: the driver then asks again).
    Interrupted(Option<crate::upload::ReadRow>),
    /// This machine's finished upload of this file, unchanged on both sides: skip it.
    AlreadyUploaded,
}

/// What the same-name file `existing_id` in `parent` says about `path`: this machine's interrupted upload of
/// it, or its finished upload (that file, or a `--keep-both` copy beside it), or neither.
/// `resume_interrupted`: an interrupted upload counts (with no conflict flag; `--keep-both` keeps both).
pub(crate) async fn existing_upload(
    api: &ApiClient,
    path: &std::path::Path,
    existing_id: Uuid,
    parent: Option<Uuid>,
    resume_interrupted: bool,
) -> Existing {
    let mut existing_row = None;
    if resume_interrupted && crate::resume::resumable_upload_now(path).is_some_and(|p| p.file_id == existing_id) {
        if let Ok(row) = api.get_file(&existing_id.to_string()).await {
            if is_unfinished_first_upload(&row) || finished_behind_the_record(&row) {
                let read = crate::upload::row_parent(&row)
                    .ok()
                    .map(|parent_id| crate::upload::ReadRow {
                        file_id: existing_id,
                        parent_id,
                    });
                return Existing::Interrupted(read);
            }
            existing_row = Some(row);
        }
    }
    let finished = crate::resume::local_signature(path).and_then(|(size, mtime_ns)| {
        crate::resume::finished_upload(path)
            .filter(|f| f.parent_id == parent && f.size == size && f.mtime_ns == mtime_ns)
    });
    let Some(finished) = finished else {
        return Existing::Other;
    };
    let row = match existing_row.filter(|_| finished.file_id == existing_id) {
        Some(row) => row,
        None => match api.get_file(&finished.file_id.to_string()).await {
            Ok(row) => row,
            Err(_) => return Existing::Other,
        },
    };
    if completed_as(&row, finished) {
        Existing::AlreadyUploaded
    } else {
        Existing::Other
    }
}

/// A `GET /api/v1/files/{id}` row this machine's resume record names, completed (not uploading, a version to
/// download, not trashed): its session finished out from under an interrupted run (a Ctrl-C or a kill while
/// `complete` was in flight, `--parallel` spec §5.2). The driver resumes the recorded session, meets the server's
/// "completed" answer and finishes the file (clearing the record), so the same command neither refuses the row
/// as a conflict nor uploads it twice.
fn finished_behind_the_record(row: &serde_json::Value) -> bool {
    row.get("is_uploading").and_then(serde_json::Value::as_bool) != Some(true)
        && row.get("download_version").is_some_and(serde_json::Value::is_object)
        && row.get("is_trashed").and_then(serde_json::Value::as_bool) != Some(true)
}

/// A `GET /api/v1/files/{id}` row of an upload that never completed: still uploading, and no completed version
/// to download.
fn is_unfinished_first_upload(row: &serde_json::Value) -> bool {
    row.get("is_uploading").and_then(serde_json::Value::as_bool) == Some(true)
        && row.get("download_version").is_none_or(serde_json::Value::is_null)
}

/// The row is still in the folder `finished` went to (which [`existing_upload`] requires to be the folder this
/// push writes into), not trashed, completed at the version it made, with the same plaintext size (`size_bytes` is the plaintext size: server `routes/uploads/complete.rs`). A later
/// version (another device replaced it), a move or the trash is not.
fn completed_as(row: &serde_json::Value, finished: crate::resume::FinishedUpload) -> bool {
    crate::upload::row_parent(row) == Ok(finished.parent_id)
        && row.get("is_trashed").and_then(serde_json::Value::as_bool) != Some(true)
        && row.get("is_uploading").and_then(serde_json::Value::as_bool) != Some(true)
        && row.get("download_version").is_some_and(serde_json::Value::is_object)
        && crate::api::current_version_number(row) == Some(finished.version)
        && row.get("size_bytes").and_then(serde_json::Value::as_u64) == Some(finished.size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn finished(version: i64, size: u64) -> crate::resume::FinishedUpload {
        crate::resume::FinishedUpload {
            file_id: Uuid::from_u128(1),
            parent_id: None,
            version,
            size,
            mtime_ns: 1,
        }
    }

    #[test]
    fn only_the_completed_version_that_upload_made_counts_as_already_uploaded() {
        let row = |uploading: bool, version: Option<i64>, size: u64| {
            json!({
                "parent_id": null,
                "is_uploading": uploading,
                "size_bytes": size,
                "download_version": version.map(|v| json!({ "version_number": v })),
            })
        };
        assert!(completed_as(&row(false, Some(1), 9), finished(1, 9)));
        let mut moved = row(false, Some(1), 9);
        moved["parent_id"] = json!(Uuid::from_u128(5));
        assert!(!completed_as(&moved, finished(1, 9)), "moved to another folder");
        let mut trashed = row(false, Some(1), 9);
        trashed["is_trashed"] = json!(true);
        assert!(!completed_as(&trashed, finished(1, 9)), "in the trash");
        assert!(!completed_as(&row(false, Some(2), 9), finished(1, 9)), "replaced since");
        assert!(!completed_as(&row(false, Some(1), 8), finished(1, 9)), "another size");
        assert!(
            !completed_as(&row(true, Some(1), 9), finished(1, 9)),
            "a new version is uploading"
        );
        assert!(
            !completed_as(&row(false, None, 9), finished(1, 9)),
            "nothing to download"
        );
    }

    /// Only a row that is still uploading AND has no completed version is our interrupted first upload;
    /// anything with a downloadable version is a real name conflict (task 1950; moved here from `push.rs`).
    #[test]
    fn only_an_uploading_row_without_a_version_is_unfinished() {
        assert!(is_unfinished_first_upload(
            &json!({ "is_uploading": true, "download_version": null })
        ));
        assert!(is_unfinished_first_upload(&json!({ "is_uploading": true })));
        assert!(!is_unfinished_first_upload(
            &json!({ "is_uploading": true, "download_version": { "version_number": 1 } })
        ));
        assert!(!is_unfinished_first_upload(
            &json!({ "is_uploading": false, "download_version": null })
        ));
        assert!(!is_unfinished_first_upload(&json!({ "download_version": null })));
        assert!(!is_unfinished_first_upload(&json!({})));
    }
}
