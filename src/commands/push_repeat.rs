//! What a re-run of `bb push` finds already in place: this machine's own upload of the same local file (task 2334).
//!
//! A re-pushed folder is reused, so every file the earlier run touched meets its own upload as a same-name
//! file. Two of those are not conflicts:
//!
//! - an INTERRUPTED first upload of this exact file (task 1950): the resume record names that id, and the
//!   server still has it uploading with no completed version. The push resumes it;
//! - a FINISHED upload of this exact file, unchanged on both sides (lead ruling, 2026-11-06): the finished
//!   record (`resume::finished_upload`) names that id, in this folder, for the same size and mtime, and the
//!   server's row is completed at the version that upload made, with the same plaintext size. The push skips
//!   it and counts it. The CLI writes no content hash a server row carries, so this record is the proof.
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

/// Whether `existing_id`, the same-name file in `parent`, is this machine's own upload of `path`.
/// `resume_interrupted`: an interrupted upload counts (with no conflict flag; `--keep-both` keeps both).
pub(crate) async fn existing_upload(
    api: &ApiClient,
    path: &std::path::Path,
    existing_id: Uuid,
    parent: Option<Uuid>,
    resume_interrupted: bool,
) -> Existing {
    let interrupted =
        resume_interrupted && crate::resume::resumable_upload_now(path).is_some_and(|p| p.file_id == existing_id);
    let finished = crate::resume::local_signature(path).and_then(|(size, mtime_ns)| {
        crate::resume::finished_upload(path)
            .filter(|f| f.file_id == existing_id && f.parent_id == parent && f.size == size && f.mtime_ns == mtime_ns)
    });
    if !interrupted && finished.is_none() {
        return Existing::Other;
    }
    let Ok(row) = api.get_file(&existing_id.to_string()).await else {
        return Existing::Other;
    };
    if interrupted && is_unfinished_first_upload(&row) {
        let parent_id = match row.get("parent_id") {
            Some(serde_json::Value::Null) => Some(None),
            Some(serde_json::Value::String(p)) => Uuid::parse_str(p).ok().map(Some),
            _ => None,
        };
        return Existing::Interrupted(parent_id.map(|parent_id| crate::upload::ReadRow {
            file_id: existing_id,
            parent_id,
        }));
    }
    match finished {
        Some(f) if completed_as(&row, f) => Existing::AlreadyUploaded,
        _ => Existing::Other,
    }
}

/// A `GET /api/v1/files/{id}` row of an upload that never completed: still uploading, and no completed version
/// to download.
fn is_unfinished_first_upload(row: &serde_json::Value) -> bool {
    row.get("is_uploading").and_then(serde_json::Value::as_bool) == Some(true)
        && row.get("download_version").is_none_or(serde_json::Value::is_null)
}

/// The row is the version `finished` made, completed, with the same plaintext size (`size_bytes` is the
/// plaintext size: server `routes/uploads/complete.rs`). A later version (another device replaced it) is not.
fn completed_as(row: &serde_json::Value, finished: crate::resume::FinishedUpload) -> bool {
    row.get("is_uploading").and_then(serde_json::Value::as_bool) != Some(true)
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
                "is_uploading": uploading,
                "size_bytes": size,
                "download_version": version.map(|v| json!({ "version_number": v })),
            })
        };
        assert!(completed_as(&row(false, Some(1), 9), finished(1, 9)));
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
