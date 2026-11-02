//! L12/A19: adopting every foreign item a move takes out of a shared root.
//!
//! One helper for every surface that moves an item (`bb mv`, bulk `bb mv`,
//! WebDAV MOVE, the FUSE rename): before the move, a source that sits in a
//! shared folder has its foreign subtree adopted and read back. A source that
//! does not sit in a shared folder is left alone (no request, no adoption), and
//! an ancestry this client cannot read refuses the move (fail closed).
use crate::api::ApiClient;
use beebeeb_core::kdf::MasterKey;
use std::collections::HashSet;

use super::OwnedKeyResolver;

/// §5: at most this many items are adopted for one move, listed in batches of
/// [`ADOPT_BATCH`]. Above the cap the move is refused with a reason.
pub const ADOPT_LIMIT: usize = 10_000;
pub const ADOPT_BATCH: usize = 1_000;

impl OwnedKeyResolver {
    /// A19: adopt what `item_id` takes out of a shared root before the move.
    ///
    /// `parent_id` is the source's current parent (the row it is listed under).
    /// `Ok(())` means the move may proceed: either nothing foreign is involved,
    /// or every foreign item was adopted and confirmed. Any doubt — an ancestor
    /// the client cannot read, an adoption not confirmed, a subtree above
    /// [`ADOPT_LIMIT`] — is an `Err`, and the caller must not move.
    pub async fn adopt_before_move(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        item_id: &str,
        parent_id: Option<&str>,
        is_folder: bool,
    ) -> Result<(), String> {
        let Some(parent_id) = parent_id else {
            return Ok(());
        };
        // Fail CLOSED: an ancestry that cannot be read is not "not shared".
        let shared = self
            .shared_root_of(api, master, parent_id)
            .await
            .map_err(|e| format!("move refused: {e}"))?;
        if shared.is_none() {
            return Ok(());
        }
        self.adopt_foreign_subtree(api, master, item_id, is_folder)
            .await
            .map_err(|e| format!("move refused: {e}"))
    }

    /// L12/A19: adopt `root_id` and every foreign item below it, confirming each
    /// by reading it back. Listings are read in batches of [`ADOPT_BATCH`] rows.
    pub async fn adopt_foreign_subtree(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        root_id: &str,
        is_folder: bool,
    ) -> Result<(), String> {
        let mut queue = vec![(root_id.to_string(), is_folder)];
        let mut seen = HashSet::new();
        // The source itself counts; every child is counted as it is enqueued.
        let mut count = 1usize;
        while let Some((id, folder)) = queue.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let row = self.ancestor_row(api, &id).await?;
            let class = beebeeb_core::item_id::item_class(&id).map_err(|_| "invalid item id")?;
            if class == beebeeb_core::item_id::ItemClass::Foreign {
                let resolved = self.resolve(api, master, &row).await?;
                if !self.adopt(api, master, &resolved).await? {
                    return Err("adoption not confirmed".into());
                }
            }
            if !folder {
                continue;
            }
            let mut cursor: Option<String> = None;
            loop {
                let (rows, next) = api.list_files_batch(&id, cursor.as_deref(), ADOPT_BATCH).await?;
                self.seed(&rows);
                for row in &rows {
                    count += 1;
                    if count > ADOPT_LIMIT {
                        return Err(format!("more than {ADOPT_LIMIT} shared items to adopt"));
                    }
                    if let Some(child) = row["id"].as_str() {
                        queue.push((child.to_string(), row["is_folder"] == true));
                    }
                }
                match next {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
        }
        Ok(())
    }
}
