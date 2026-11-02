//! L14: the index check compares against ONE root's custody, never another's.

use super::*;

impl OwnedKeyResolver {
    /// L14: the proven custody key of `root_id` and the L7 wrap THAT root's own
    /// inventory holds for `id`. A wrap held by another root is never returned:
    /// it proves nothing about membership in the root being repaired.
    pub fn child_wrap_in_root(&self, root_id: &str, id: &str) -> Option<(Arc<FileKey>, Vec<u8>)> {
        let root = self.roots.get(root_id)?;
        if root.legacy_invite {
            return None;
        }
        root.children.get(id).map(|wrap| (root.key.clone(), wrap.clone()))
    }
    /// True when some other root's inventory holds a wrap for `id`. Such a wrap
    /// cannot stand in for `root_id`'s custody (L14).
    pub fn wrap_held_elsewhere(&self, id: &str, root_id: &str) -> bool {
        self.roots
            .iter()
            .any(|(other, root)| other != root_id && !root.legacy_invite && root.children.contains_key(id))
    }
}
