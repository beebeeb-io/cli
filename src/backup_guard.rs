//! The Backups tree in the CLI (LD-0113, spec Amendments 8 and 8a): a device's
//! backup is written by that device's own backup pass and by nothing else.
//!
//! `bb rm`, `mv`, `mkdir` and `push` ask core's app rule
//! ([`beebeeb_core::backup::check_app_mutation`]) before they change anything
//! under the root, and print core's words when it refuses. The root is the
//! folder whose server row carries `system_role = "backups_root"`, never a
//! folder found by its name. `bb backup run` never goes through these checks:
//! it is the one writer they protect the tree for.
//!
//! Owns: a row's depth below the flagged root, the refusal words (core's, plus
//! the `bb backup add` hint on a refused write), and a server
//! `409 protected_folder` answer turned into core's words.
//! Must not: decide the rule itself (that is core's), or name the folder by
//! anything but core's words.

use beebeeb_core::backup::{BackupPosition, Mutation, check_app_mutation};
use beebeeb_types::backup::{PROTECTED_FOLDER_ERROR, SystemRole};
use serde_json::Value;
use std::collections::HashMap;

use crate::api::{ApiClient, ApiError};

/// What a refused write says after core's words (spec Amendment 8).
pub(crate) const BACKUP_HINT: &str = "To back up this machine: `bb backup add <folder>`";

/// The most parents [`depth_of`] reads before it gives up: a tree deeper than
/// this, or a parent loop, is corrupt metadata, never "outside the tree".
const MAX_ANCESTORS: usize = 256;

/// Whether a server row is the account's flagged Backups root. Only the exact
/// wire role counts; a folder NAMED "Backups" is not the root.
pub(crate) fn is_backups_root(row: &Value) -> bool {
    row.get("system_role")
        .and_then(Value::as_str)
        .and_then(SystemRole::parse)
        == Some(SystemRole::BackupsRoot)
}

/// The depth below the flagged root of `row`, listed in a folder whose own
/// depth is `parent` (`None`: outside the tree, or the vault root).
pub(crate) fn child_depth(parent: Option<usize>, row: &Value) -> Option<usize> {
    if is_backups_root(row) {
        Some(0)
    } else {
        parent.map(|d| d + 1)
    }
}

/// Core's words for a refusal, plus the `bb backup add` hint when what was
/// refused is a write (a new item, or a new version of one).
pub(crate) fn refusal(message: &str, m: Mutation) -> String {
    if matches!(m, Mutation::Create | Mutation::Write) {
        format!("{message}\n  {BACKUP_HINT}")
    } else {
        message.to_string()
    }
}

/// Core's app rule on a row at `position`, as the CLI's error.
pub(crate) fn check(position: BackupPosition, m: Mutation) -> Result<(), String> {
    check_app_mutation(position, m).map_err(|refused| refusal(&refused.message, m))
}

/// A server error, with a `409 protected_folder` answer replaced by core's
/// words for what the CLI asked. In phase 1 the server protects the root only,
/// so its refusal is the root's (spec §5.3); the words are core's, never the
/// server's free text.
pub(crate) fn server_refusal(e: ApiError, m: Mutation) -> String {
    if e.is_code(PROTECTED_FOLDER_ERROR) {
        let words = check_app_mutation(BackupPosition::Root, m)
            .err()
            .map(|refused| refused.message.to_string())
            .unwrap_or(e.message);
        refusal(&words, m)
    } else {
        e.message
    }
}

/// The depth below the flagged root of the row `id`, read through its parents
/// (`GET /files/{id}`, one per level). `None`: the row is not in the tree.
pub(crate) async fn depth_of(api: &ApiClient, id: &str) -> Result<Option<usize>, String> {
    BackupDepths::default().depth_of(api, id).await
}

/// The depths below the flagged root that one command already knows, so a `bb push` reads each folder at most
/// once and the folders it listed or made not at all. A folder's depth follows from its parent's: a row listed
/// in a folder of known depth, or a folder made in one, needs no read (the vault root is outside the tree).
#[derive(Debug, Default)]
pub(crate) struct BackupDepths {
    /// By id, in the server's form (`None`: outside the tree).
    known: HashMap<String, Option<usize>>,
}

impl BackupDepths {
    /// `row`, as the listing of `parent` showed it (`None`: the vault root).
    pub(crate) fn listed(&mut self, parent: Option<&str>, row: &Value) {
        if let (Some(id), Some(parent_depth)) = (row.get("id").and_then(Value::as_str), self.known(parent)) {
            self.known.insert(canonical(id), child_depth(parent_depth, row));
        }
    }

    /// The folder `id` this command made in `parent` (`None`: the vault root).
    pub(crate) fn created(&mut self, id: &str, parent: Option<&str>) {
        if let Some(parent_depth) = self.known(parent) {
            self.known.insert(canonical(id), parent_depth.map(|d| d + 1));
        }
    }

    fn known(&self, id: Option<&str>) -> Option<Option<usize>> {
        match id {
            None => Some(None),
            Some(id) => self.known.get(&canonical(id)).copied(),
        }
    }

    /// [`depth_of`], reading only the folders whose depth is not known yet, and keeping every depth it learns.
    pub(crate) async fn depth_of(&mut self, api: &ApiClient, id: &str) -> Result<Option<usize>, String> {
        // The rows read, each the parent of the one before: their depths follow from where the walk ends.
        let mut below: Vec<String> = Vec::new();
        let mut current = canonical(id);
        let mut depth = loop {
            if let Some(depth) = self.known.get(&current) {
                break *depth;
            }
            if below.len() == MAX_ANCESTORS {
                return Err("cannot tell where this item is: its folders are nested too deeply".to_string());
            }
            let row = match api.get_file(current.as_str()).await {
                Ok(row) => row,
                // Above a folder shared with this account sits the sharer's own
                // folder, which this account cannot read: the shared tree is not
                // this account's Backups tree (its root has no parent).
                Err(e) if !below.is_empty() && matches!(e.status, 403 | 404) => break None,
                Err(e) => return Err(e.message),
            };
            if is_backups_root(&row) {
                break Some(0);
            }
            match row.get("parent_id").and_then(Value::as_str) {
                Some(parent) => below.push(std::mem::replace(&mut current, canonical(parent))),
                None => break None,
            }
        };
        self.known.insert(current, depth);
        while let Some(child) = below.pop() {
            depth = depth.map(|d| d + 1);
            self.known.insert(child, depth);
        }
        Ok(depth)
    }

    /// Refuse a new item in folder `parent` (`None`: the vault root) when it is in the Backups tree, or when
    /// where `parent` is cannot be told (fail closed).
    pub(crate) async fn check_create_in(&mut self, api: &ApiClient, parent: Option<&str>) -> Result<(), String> {
        let Some(parent) = parent else {
            return Ok(());
        };
        let depth = self.depth_of(api, parent).await.map_err(|e| {
            format!("could not check whether the destination folder is inside Backups, so the push stops: {e}")
        })?;
        // A parent is a folder, so at depth 2 it is a category folder, never a
        // manifest: the name is not needed.
        check(BackupPosition::at_depth(depth, ""), Mutation::Create)
    }
}

/// One spelling per id (a user's `--parent` may differ in case from the server's rows).
fn canonical(id: &str) -> String {
    uuid::Uuid::parse_str(id).map_or_else(|_| id.to_string(), |u| u.to_string())
}

/// The most folders [`count_items`] lists before it reports a lower bound.
const COUNT_FOLDER_CAP: usize = 10_000;

/// How many items (files and folders) are under the folder `id`, for the
/// "Delete <device>'s backup?" question. `exact` is false when the count
/// stopped at [`COUNT_FOLDER_CAP`] folders.
pub(crate) async fn count_items(api: &ApiClient, id: &str) -> Result<(u64, bool), String> {
    let mut stack = vec![id.to_string()];
    let (mut items, mut folders) = (0u64, 0usize);
    while let Some(folder) = stack.pop() {
        folders += 1;
        if folders > COUNT_FOLDER_CAP {
            return Ok((items, false));
        }
        let listing = api.list_files(Some(&folder)).await?;
        let rows = listing
            .get("files")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for row in rows {
            items += 1;
            if row.get("is_folder").and_then(Value::as_bool).unwrap_or(false)
                && let Some(child) = row.get("id").and_then(Value::as_str)
            {
                stack.push(child.to_string());
            }
        }
    }
    Ok((items, true))
}

/// The question `bb rm` asks before it deletes a device's backup or one of its
/// folders (spec §8 and §4.2 rule 11). `folder` is `None` for a device folder.
pub(crate) fn delete_backup_question(device: &str, folder: Option<&str>, items: u64, exact: bool) -> String {
    let noun = if items == 1 { "item" } else { "items" };
    let count = if exact {
        format!("{items} {noun}")
    } else {
        format!("More than {items} {noun}")
    };
    match folder {
        None => format!(
            "Delete {device}'s backup? {count}. {device} will stop backing up these folders until you choose \
             Back up again on it."
        ),
        Some(folder) => format!(
            "Delete {device}'s backup of {folder}? {count}. {device} will stop backing up this folder until you \
             choose Back up again on it."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beebeeb_core::backup::{MSG_DELETE, MSG_MOVE, MSG_READ_ONLY};
    use serde_json::json;

    #[test]
    fn only_the_wire_role_is_the_root() {
        assert!(is_backups_root(&json!({ "system_role": "backups_root" })));
        assert!(!is_backups_root(
            &json!({ "system_role": null, "name_encrypted": "Backups" })
        ));
        assert!(!is_backups_root(&json!({ "system_role": "Backups" })));
        assert!(!is_backups_root(&json!({})));
    }

    #[test]
    fn depth_counts_from_the_flagged_root_down() {
        let root = json!({ "system_role": "backups_root" });
        let plain = json!({ "system_role": null });
        assert_eq!(child_depth(None, &root), Some(0));
        assert_eq!(child_depth(None, &plain), None);
        assert_eq!(child_depth(Some(0), &plain), Some(1));
        assert_eq!(child_depth(Some(2), &plain), Some(3));
    }

    #[test]
    fn a_refused_write_carries_the_hint_and_a_refused_delete_does_not() {
        assert_eq!(
            check(BackupPosition::Root, Mutation::Trash),
            Err(MSG_DELETE.to_string())
        );
        assert_eq!(check(BackupPosition::Root, Mutation::Move), Err(MSG_MOVE.to_string()));
        assert_eq!(
            check(BackupPosition::Inside, Mutation::Create),
            Err(format!("{MSG_READ_ONLY}\n  {BACKUP_HINT}"))
        );
        assert_eq!(check(BackupPosition::DeviceFolder, Mutation::Trash), Ok(()));
        assert_eq!(check(BackupPosition::Outside, Mutation::Create), Ok(()));
    }

    #[test]
    fn a_server_protected_folder_answer_becomes_core_words() {
        let e = ApiError {
            code: Some("protected_folder".into()),
            message: "something the server said".into(),
            status: 409,
        };
        assert_eq!(server_refusal(e, Mutation::Trash), MSG_DELETE);
        let other = ApiError {
            code: Some("not_found".into()),
            message: "not found".into(),
            status: 404,
        };
        assert_eq!(server_refusal(other, Mutation::Trash), "not found");
    }

    #[test]
    fn the_device_question_names_the_count_and_what_happens_next() {
        assert_eq!(
            delete_backup_question("GLegion", None, 3, true),
            "Delete GLegion's backup? 3 items. GLegion will stop backing up these folders until you choose Back \
             up again on it."
        );
        assert!(
            delete_backup_question("GLegion", Some("Pictures"), 10, false)
                .starts_with("Delete GLegion's backup of Pictures? More than 10 items.")
        );
    }
}
