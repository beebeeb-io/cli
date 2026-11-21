//! The CLI as `beebeeb_backup::BackupHost`: one host per backed-up folder per
//! run. The pass queues uploads here; [`CliHost::drain`] uploads them after
//! the pass, each read in place and checked against its fingerprint before
//! the first chunk and again before it completes (spec §4.3), and writes each
//! record when its upload completes.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use beebeeb_backup::{BackupHost, DeviceTree, Fingerprint, HostError, UploadRequest};
use beebeeb_core::backup::{BackupRecord, FolderPresence, RecordRef, RecordState, UploadReason};
use beebeeb_core::kdf::MasterKey;
use beebeeb_types::backup::Category;
use uuid::Uuid;

use super::remote::{self, Tree, Uploaded};
use super::state::Records;
use crate::api::ApiClient;
use crate::upload::{ChunkProgress, FileProgress};

fn host_err(e: impl std::fmt::Display) -> HostError {
    HostError(e.to_string())
}

/// What the uploads after a pass came to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Drained {
    pub uploaded: usize,
    pub failed: usize,
    /// Changed again after the pass looked at it: backed up next run.
    pub changed_meanwhile: usize,
    /// Uploaded as a new file because two or more server rows had its exact name in its folder.
    pub ambiguous: usize,
}

pub(crate) struct CliHost<'a> {
    api: &'a ApiClient,
    master_key: Arc<MasterKey>,
    category: Category,
    device_folder_live: bool,
    category_folder_live: Option<bool>,
    /// The category's tree, listed once before the pass (`None` when the
    /// folder is gone).
    tree: Option<Tree>,
    records: Mutex<Records>,
    queue: Mutex<Vec<UploadRequest>>,
    turned_off: Mutex<Option<FolderPresence>>,
    /// Uploads that found two or more same-name rows and added no version (see [`SameName`]).
    ambiguous: AtomicUsize,
}

/// The server copies, listed before the pass, of a file that has no record: same folder, exact name.
enum SameName {
    None,
    One(Uuid),
    /// Which row is this backup's copy is not known: nothing is guessed.
    Several,
}

impl<'a> CliHost<'a> {
    pub(crate) fn new(
        api: &'a ApiClient,
        master_key: Arc<MasterKey>,
        category: Category,
        presence: (bool, Option<bool>),
        tree: Option<Tree>,
        records: Records,
    ) -> Self {
        CliHost {
            api,
            master_key,
            category,
            device_folder_live: presence.0,
            category_folder_live: presence.1,
            tree,
            records: Mutex::new(records),
            queue: Mutex::new(Vec::new()),
            turned_off: Mutex::new(None),
            ambiguous: AtomicUsize::new(0),
        }
    }

    /// The live server rows named exactly like `req`, in the exact folder `req` goes into, listed before the
    /// pass. Only for a file with no record: with one, the pass already knows its remote file. A row in
    /// another folder, or with a name that differs in any way, never counts.
    fn same_name_copies(&self, req: &UploadRequest) -> SameName {
        let Some(tree) = self.tree.as_ref().filter(|_| req.record_key.is_none()) else {
            return SameName::None;
        };
        let mut found = tree
            .files
            .iter()
            .filter(|f| f.key == req.key && tree.parents.get(&f.remote_id) == Some(&req.parent_id));
        match (found.next(), found.next()) {
            (None, _) => SameName::None,
            (Some(only), None) => SameName::One(only.remote_id),
            (Some(_), Some(_)) => SameName::Several,
        }
    }

    pub(crate) fn turned_off(&self) -> Option<FolderPresence> {
        *self.turned_off.lock().unwrap()
    }

    pub(crate) fn queued(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub(crate) fn into_records(self) -> Records {
        self.records.into_inner().unwrap()
    }

    /// Upload everything the pass queued, one file at a time.
    pub(crate) async fn drain(&self) -> Drained {
        let queue = std::mem::take(&mut *self.queue.lock().unwrap());
        let mut drained = Drained::default();
        for req in queue {
            match self.upload_one(&req).await {
                Ok(true) => drained.uploaded += 1,
                Ok(false) => drained.changed_meanwhile += 1,
                Err(_) => drained.failed += 1,
            }
        }
        drained.ambiguous = self.ambiguous.load(Ordering::Relaxed);
        drained
    }

    /// `Ok(false)`: the file changed after the pass fingerprinted it, so it
    /// waits for the next run (nothing landed).
    async fn upload_one(&self, req: &UploadRequest) -> Result<bool, String> {
        // An unknown file goes on: the upload fails on its own if it really cannot be read.
        if matches!(compare(&req.source_path, &req.fingerprint), Compared::Changed) {
            return Ok(false);
        }
        let guard = SourceGuard {
            fingerprint: req.fingerprint,
        };
        // A file with no record (a reinstall) whose copy the rebuild did not trust: the copy is still on the
        // server, so the upload adds a version to it instead of making a second row.
        let replace = match req.reason {
            UploadReason::Changed => req.remote_id,
            UploadReason::New => match self.same_name_copies(req) {
                SameName::One(id) => Some(id),
                SameName::Several => {
                    self.ambiguous.fetch_add(1, Ordering::Relaxed);
                    None
                }
                SameName::None => None,
            },
        };
        let mut done = remote::upload(
            self.api,
            &self.master_key,
            &req.source_path,
            &req.name,
            req.parent_id,
            replace,
            &guard,
        )
        .await;
        // A new version of a file the server no longer has comes back as a new
        // file (beebeeb-backup's `UploadRequest` contract).
        if matches!(done, Ok(Uploaded::Gone)) {
            done = remote::upload(
                self.api,
                &self.master_key,
                &req.source_path,
                &req.name,
                req.parent_id,
                None,
                &guard,
            )
            .await;
        }
        let id = match done {
            Ok(Uploaded::Done { id }) => id,
            Ok(Uploaded::Gone) => return Err("the file to add a version to is gone".to_string()),
            Err(e) if e == CHANGED_WHILE_READ => return Ok(false),
            Err(e) => return Err(e),
        };
        let mut records = self.records.lock().unwrap();
        if let Some(old) = req.record_key.as_deref().filter(|k| *k != req.key)
            && records.get(old).is_some_and(|r| r.remote_id != id)
        {
            records.delete(old)?;
        }
        records.put(BackupRecord {
            key: req.key.clone(),
            size: req.fingerprint.size,
            modified_ms: req.fingerprint.modified_ms,
            remote_id: id,
            uploaded_at_ms: super::now_ms(),
            state: RecordState::BackedUp,
        })?;
        Ok(true)
    }
}

/// What a guard refusal says; the drain counts it as "changed meanwhile".
const CHANGED_WHILE_READ: &str = "the file changed while it was read; it is backed up on the next run";

/// How the file at a path compares with the fingerprint the pass took.
enum Compared {
    Same,
    /// Another file now, or none: it waits for the next run.
    Changed,
    /// The file could not be looked at (a dropped mount, a folder that lost its search permission): nothing is known
    /// about it, so it is not "changed" and its upload is a failure, which the person sees.
    Unknown(String),
}

fn compare(path: &Path, expected: &Fingerprint) -> Compared {
    match beebeeb_backup::fingerprint(path) {
        Ok(now) if now == *expected => Compared::Same,
        Ok(_) => Compared::Changed,
        // Gone, or no longer a regular file (core's own refusal, `InvalidInput`): not the file the pass saw.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Compared::Changed
        }
        Err(e) => Compared::Unknown(e.to_string()),
    }
}

/// The upload driver's hook: before a version completes, the file must still
/// be the one the pass fingerprinted, or the session is abandoned (spec §4.3).
struct SourceGuard {
    fingerprint: Fingerprint,
}

struct Quiet;

impl FileProgress for Quiet {
    fn chunk_confirmed(&self, _ciphertext_bytes: u64) {}
    fn finish(self: Box<Self>, _success: bool) {}
}

impl ChunkProgress for SourceGuard {
    fn begin_file(&self, _file_name: &str, _expected_ciphertext: u64) -> Box<dyn FileProgress> {
        Box::new(Quiet)
    }

    fn before_complete(&self, path: &Path) -> Result<(), String> {
        match compare(path, &self.fingerprint) {
            Compared::Same => Ok(()),
            Compared::Changed => Err(CHANGED_WHILE_READ.to_string()),
            // The driver stops here as it does for a change, but these words are not CHANGED_WHILE_READ: the drain
            // counts the file as failed.
            Compared::Unknown(why) => Err(format!("cannot check that the file is unchanged: {why}")),
        }
    }
}

impl BackupHost for CliHost<'_> {
    async fn device_tree(&self, _c: &Category) -> Result<DeviceTree, HostError> {
        let (folders, files, complete) = match &self.tree {
            Some(t) => (t.folders.clone(), t.files.clone(), t.complete),
            None => (Default::default(), Vec::new(), true),
        };
        Ok(DeviceTree {
            device_folder_live: self.device_folder_live,
            category_folder_live: self.category_folder_live,
            folders,
            files,
            complete,
        })
    }

    async fn ensure_folder(&self, parent: Uuid, name: &str) -> Result<Uuid, HostError> {
        remote::ensure_folder(self.api, &self.master_key, parent, name)
            .await
            .map_err(host_err)
    }

    async fn move_file(&self, id: Uuid, new_parent: Uuid, new_name: &str) -> Result<(), HostError> {
        let row = self.api.get_file(id.to_string()).await.map_err(host_err)?;
        let resolved = crate::owned_keys::OwnedKeyResolver::new()
            .resolve(self.api, &self.master_key, &row)
            .await
            .map_err(host_err)?;
        let target = resolved
            .write_target
            .ok_or_else(|| HostError(crate::owned_keys::RENAME_UNKNOWN_KEY.to_string()))?;
        let sealed = beebeeb_core::encrypt::encrypt_name_for_target(&target, new_name, None)
            .map_err(|e| HostError(format!("failed to encrypt the new name: {e}")))?;
        self.api
            .move_file(&id.to_string(), Some(&sealed), Some(Some(new_parent)))
            .await
            .map(|_| ())
            .map_err(host_err)
    }

    async fn enqueue_upload(&self, req: UploadRequest) -> Result<(), HostError> {
        let mut queue = self.queue.lock().unwrap();
        if !queue.iter().any(|q| q.key == req.key) {
            queue.push(req);
        }
        Ok(())
    }

    async fn records(&self, _c: &Category) -> Result<Vec<BackupRecord>, HostError> {
        Ok(self.records.lock().unwrap().all())
    }

    async fn held_keys(&self, _c: &Category) -> Result<Vec<String>, HostError> {
        // Uploads run after the pass, one run at a time: nothing is in flight
        // while a pass plans, and a failed upload is simply planned again.
        Ok(Vec::new())
    }

    async fn put_record(&self, _c: &Category, expected: &RecordRef, record: BackupRecord) -> Result<bool, HostError> {
        let mut records = self.records.lock().unwrap();
        let stored = records
            .get(&expected.key)
            .filter(|r| r.remote_id == expected.remote_id)
            .cloned();
        let next = match stored {
            // Only the key and the state change; the stored size, mtime and
            // upload time stay (an upload may have written newer ones).
            Some(stored) => BackupRecord {
                key: record.key.clone(),
                state: RecordState::BackedUp,
                ..stored
            },
            None if records.find_remote(expected.remote_id).is_none() => record,
            None => return Ok(false),
        };
        let at_old_key_is_moved = records
            .get(&expected.key)
            .is_some_and(|r| r.remote_id == expected.remote_id);
        if expected.key != next.key && at_old_key_is_moved {
            records.delete(&expected.key).map_err(HostError)?;
        }
        records.put(next).map_err(HostError)?;
        Ok(true)
    }

    async fn mark_removed_elsewhere(&self, _c: &Category, refs: &[RecordRef]) -> Result<(), HostError> {
        let mut records = self.records.lock().unwrap();
        for r in refs {
            if let Some(stored) = records
                .get(&r.key)
                .filter(|s| s.remote_id == r.remote_id && s.state == RecordState::BackedUp)
                .cloned()
            {
                records
                    .put(BackupRecord {
                        state: RecordState::RemovedElsewhere,
                        ..stored
                    })
                    .map_err(HostError)?;
            }
        }
        Ok(())
    }

    async fn turn_off(&self, c: &Category, why: FolderPresence) -> Result<(), HostError> {
        debug_assert_eq!(c, &self.category);
        // Nothing of this folder is uploaded any more (Amendment 12: never
        // re-upload a folder that was deleted).
        self.queue.lock().unwrap().clear();
        *self.turned_off.lock().unwrap() = Some(why);
        Ok(())
    }
}

#[cfg(test)]
mod move_tests {
    use super::*;
    use crate::test_server::{Mock, shared, spawn};
    use beebeeb_core::chunk_stream::ExpectedChunkFormat;
    use serde_json::json;

    /// A backup's move of a v2-protected file re-seals the name WITH the version commitment, so the file stays
    /// readable (core refuses v2 content whose name carries none).
    #[tokio::test]
    async fn a_backup_move_keeps_the_version_commitment() {
        const ID: &str = "11111111-1111-4111-8111-111111111111";
        let master = Arc::new(MasterKey::from_bytes([7u8; 32]));
        let key = beebeeb_core::kdf::derive_file_key(&master, ID.as_bytes());
        let commitment =
            beebeeb_core::chunk_stream::VersionCommitment::new([9u8; beebeeb_core::chunk_stream::VERSION_ID_LEN], 3)
                .unwrap();
        let sealed =
            beebeeb_core::encrypt::encrypt_name_for_version(&key, "old.txt", None, ExpectedChunkFormat::V2, commitment)
                .unwrap();
        let row = json!({"id": ID, "parent_id": null, "name_encrypted": sealed, "is_folder": false,
            "size_bytes": 3, "chunk_count": 1, "updated_at": "2026-10-31T10:00:00Z",
            "owner_wrapped_file_key": null, "acted_via_share_id": null, "uploaded_by": null});
        let mut mock = Mock::default();
        mock.files.insert(ID.to_string(), row);
        let mock = shared(mock);
        let api = ApiClient::new_for_test(spawn(mock.clone()).await);
        let records = Records::load(std::env::temp_dir().join(format!("bb-move-{}.jsonl", Uuid::new_v4()))).unwrap();
        let host = CliHost::new(&api, master, Category::Documents, (true, Some(true)), None, records);

        host.move_file(ID.parse().unwrap(), Uuid::new_v4(), "new.txt")
            .await
            .unwrap();

        let m = mock.lock().unwrap();
        let name = m.move_bodies[0].1["name_encrypted"].as_str().unwrap();
        let meta = beebeeb_core::encrypt::decrypt_file_metadata_with_key(&key, name).unwrap();
        assert_eq!(meta.name, "new.txt");
        assert_eq!(meta.chunk_format, ExpectedChunkFormat::V2);
        assert_eq!(
            meta.version,
            Some(commitment),
            "the commitment is carried over, not dropped"
        );
    }
}
