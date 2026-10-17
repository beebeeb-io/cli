//! Deletion-safety rules for `bb sync` (tasks 1915 + 1916, deletion-safety
//! spec 2026-10-17, rules R4 and R5).
//!
//! The principle: **absence is not deletion.** A file missing from a remote
//! listing proves nothing - the listing may be empty, short, unreadable, from
//! the wrong folder or the wrong account. `bb sync` removes a local file only
//! when the server positively reports that exact file as trashed (a tombstone),
//! from a listing it has reason to trust, and never in bulk without an explicit
//! confirmation. A removal moves the file into a recoverable folder inside the
//! sync root; it is never a plain unlink.
//!
//! This module holds the parts that need no manifest types, so they are unit
//! tested on their own: the mass-delete breaker, the listing-trust verdict, the
//! tombstone test and the recoverable move.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use uuid::Uuid;

use crate::api::ApiClient;

/// Folder (inside the sync root) that receives every file `bb sync` removes
/// because of a remote deletion. Dot-prefixed, so the scanners already skip it
/// and it is never uploaded.
pub const RECOVERY_DIR: &str = ".bb-trash";

/// A single pass may remove at most `max(BREAKER_FLOOR, 10 % of the tracked
/// files)` local files before the breaker stops it and asks for confirmation.
pub const BREAKER_FLOOR: usize = 50;

/// The most local removals one pass may perform without explicit confirmation.
pub fn breaker_limit(tracked: usize) -> usize {
    BREAKER_FLOOR.max(tracked.div_ceil(10))
}

/// Would `removals` local deletions out of `tracked` synced files trip the breaker?
pub fn breaker_trips(removals: usize, tracked: usize) -> bool {
    removals > breaker_limit(tracked)
}

/// What the remote walk could not see. Any of it makes the listing unfit to
/// conclude "this file was deleted" from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListingHealth {
    /// Entries skipped because the name did not decrypt (wrong key, damaged
    /// blob) or was unsafe as a local path segment.
    pub unreadable: u32,
    /// The listing hit the client's entry cap, so entries may be missing.
    pub truncated: bool,
}

/// Did the listing response say the client cut it short? (`list_all_files`
/// adds `"truncated": true` when it hits its safety cap.)
pub fn listing_truncated(listing: &Value) -> bool {
    listing.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Is this listing fit to conclude deletions from? `None` = yes, `Some(why)` =
/// no, and nothing may be removed locally on its account.
pub fn untrusted_reason(
    health: &ListingHealth,
    remote_file_count: usize,
    tracked: usize,
    root_changed: bool,
) -> Option<String> {
    if root_changed {
        return Some("the remote folder is not the one this directory last synced with".to_string());
    }
    if health.truncated {
        return Some("the remote listing hit the client's entry cap, so entries may be missing".to_string());
    }
    if health.unreadable > 0 {
        let n = health.unreadable;
        let s = if n == 1 { "y" } else { "ies" };
        return Some(format!(
            "{n} remote entr{s} could not be read (name did not decrypt, or was unsafe)"
        ));
    }
    if remote_file_count == 0 && tracked > 0 {
        return Some("the remote listing is empty".to_string());
    }
    None
}

/// The server's positive deletion record for `id`: the file's own record, read
/// by id, says it is trashed. A 404, an error, or a record that is not trashed
/// (moved, renamed, a short listing, another account) is NOT a tombstone.
pub fn is_tombstone(record: &Value, id: &Uuid) -> bool {
    let same_id = match record.get("id").and_then(|v| v.as_str()) {
        Some(s) => s.parse::<Uuid>().map(|u| u == *id).unwrap_or(false),
        None => false,
    };
    same_id && record.get("is_trashed").and_then(|v| v.as_bool()) == Some(true)
}

/// Ask the server about each candidate by id and return the ones it positively
/// reports trashed. Stops early once more than `stop_after` are confirmed (the
/// breaker has tripped by then, so the exact count is no longer needed).
pub async fn confirm_tombstones(api: &ApiClient, candidates: &[Uuid], stop_after: Option<usize>) -> HashSet<Uuid> {
    use futures_util::stream::{self, StreamExt};
    let mut confirmed = HashSet::new();
    for batch in candidates.chunks(64) {
        let results: Vec<Option<Uuid>> = stream::iter(batch.iter().copied())
            .map(|id| async move {
                match api.get_file(&id.to_string()).await {
                    Ok(record) if is_tombstone(&record, &id) => Some(id),
                    _ => None,
                }
            })
            .buffer_unordered(8)
            .collect()
            .await;
        confirmed.extend(results.into_iter().flatten());
        if stop_after.is_some_and(|n| confirmed.len() > n) {
            break;
        }
    }
    confirmed
}

/// Move `src` (a file inside `root`, whose sync-relative path is `rel`) into
/// `<root>/.bb-trash/<UTC stamp>/<rel>`. Same filesystem, so this is a rename:
/// the bytes are not copied and not lost. Never falls back to deleting - if the
/// move fails the file stays where it is.
pub fn move_to_recovery(root: &Path, src: &Path, rel: &str) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let base = root.join(RECOVERY_DIR).join(stamp).join(rel);
    let mut dest = base.clone();
    let mut n = 1u32;
    while dest.exists() {
        let mut name = base.file_name().map(|s| s.to_os_string()).unwrap_or_default();
        name.push(format!(".{n}"));
        dest = base.with_file_name(name);
        n += 1;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    std::fs::rename(src, &dest).map_err(|e| format!("move {} aside: {e}", src.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn breaker_limit_is_fifty_or_ten_percent() {
        assert_eq!(breaker_limit(0), 50);
        assert_eq!(breaker_limit(10), 50);
        assert_eq!(breaker_limit(500), 50);
        assert_eq!(breaker_limit(501), 51); // ceil(10 %)
        assert_eq!(breaker_limit(5_000), 500);
    }

    #[test]
    fn breaker_trips_strictly_above_the_limit() {
        assert!(!breaker_trips(50, 100), "exactly 50 is allowed");
        assert!(breaker_trips(51, 100), "51 of 100 trips");
        assert!(!breaker_trips(500, 5_000), "exactly 10 % of 5000 is allowed");
        assert!(breaker_trips(501, 5_000));
        assert!(!breaker_trips(0, 0));
    }

    #[test]
    fn healthy_listing_is_trusted() {
        assert_eq!(untrusted_reason(&ListingHealth::default(), 3, 3, false), None);
        // An empty listing of an empty manifest is just a new sync.
        assert_eq!(untrusted_reason(&ListingHealth::default(), 0, 0, false), None);
    }

    #[test]
    fn empty_listing_with_tracked_files_is_untrusted() {
        let why = untrusted_reason(&ListingHealth::default(), 0, 7, false).unwrap();
        assert!(why.contains("empty"), "{why}");
    }

    #[test]
    fn truncated_unreadable_and_changed_root_are_untrusted() {
        let t = ListingHealth {
            truncated: true,
            ..Default::default()
        };
        assert!(untrusted_reason(&t, 10, 10, false).unwrap().contains("cap"));
        let u = ListingHealth {
            unreadable: 2,
            ..Default::default()
        };
        assert!(
            untrusted_reason(&u, 10, 10, false)
                .unwrap()
                .contains("2 remote entries")
        );
        let one = ListingHealth {
            unreadable: 1,
            ..Default::default()
        };
        assert!(
            untrusted_reason(&one, 10, 10, false)
                .unwrap()
                .contains("1 remote entry")
        );
        assert!(
            untrusted_reason(&ListingHealth::default(), 10, 10, true)
                .unwrap()
                .contains("last synced")
        );
    }

    #[test]
    fn listing_truncated_reads_the_flag() {
        assert!(listing_truncated(&json!({ "files": [], "truncated": true })));
        assert!(!listing_truncated(&json!({ "files": [], "truncated": false })));
        assert!(!listing_truncated(&json!({ "files": [] })));
    }

    #[test]
    fn only_a_trashed_record_for_that_id_is_a_tombstone() {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert!(is_tombstone(&json!({ "id": id.to_string(), "is_trashed": true }), &id));
        assert!(!is_tombstone(
            &json!({ "id": id.to_string(), "is_trashed": false }),
            &id
        ));
        assert!(
            !is_tombstone(&json!({ "id": id.to_string() }), &id),
            "no flag => no tombstone"
        );
        assert!(
            !is_tombstone(&json!({ "id": other.to_string(), "is_trashed": true }), &id),
            "a record for a different id is not this file's tombstone"
        );
        assert!(
            !is_tombstone(&json!({ "is_trashed": true }), &id),
            "no id => not verifiable"
        );
        assert!(!is_tombstone(&json!({ "error": "not_found" }), &id));
    }

    #[test]
    fn move_to_recovery_keeps_the_bytes_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!("bb-recovery-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let src = root.join("sub").join("a.txt");

        std::fs::write(&src, b"first").unwrap();
        let d1 = move_to_recovery(&root, &src, "sub/a.txt").unwrap();
        assert!(!src.exists(), "moved, not copied");
        assert_eq!(std::fs::read(&d1).unwrap(), b"first");
        assert!(d1.starts_with(root.join(RECOVERY_DIR)));
        assert!(d1.ends_with("sub/a.txt"));

        // Same file name, same second: must not clobber the earlier copy.
        std::fs::write(&src, b"second").unwrap();
        let d2 = move_to_recovery(&root, &src, "sub/a.txt").unwrap();
        assert_ne!(d1, d2);
        assert_eq!(std::fs::read(&d1).unwrap(), b"first", "earlier recovery copy untouched");
        assert_eq!(std::fs::read(&d2).unwrap(), b"second");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn move_to_recovery_failure_leaves_the_file_in_place() {
        // A source that does not exist: an error, not a panic or a silent success.
        let root = std::env::temp_dir().join(format!("bb-recovery-fail-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(move_to_recovery(&root, &root.join("not-there.txt"), "not-there.txt").is_err());
        std::fs::remove_dir_all(&root).ok();

        // Block the recovery dir with a plain file: the move must fail, not unlink.
        let root = std::env::temp_dir().join(format!("bb-recovery-block-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("keep.txt");
        std::fs::write(&src, b"precious").unwrap();
        std::fs::write(root.join(RECOVERY_DIR), b"i am a file, not a dir").unwrap();
        assert!(move_to_recovery(&root, &src, "keep.txt").is_err());
        assert_eq!(
            std::fs::read(&src).unwrap(),
            b"precious",
            "a failed move must keep the file"
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
