//! The local store of `bb backup`: which folders this machine backs up, the
//! device folder it backs up into, and per folder the records of what was
//! backed up (core's `BackupRecord`), in the CLI's state dir.
//!
//! - `backup.json`: the settings, written whole (small);
//! - `backup-records-<n>.jsonl`: one line per record change, appended as each
//!   upload completes, so a run that dies keeps every record it wrote (a lost
//!   record would upload the file again as a second copy). Compacted on save.
//!
//! The store belongs to ONE account (its user id): a different signed-in
//! account never reads or writes it (R10).
//!
//! Owns: the files' shapes and their atomic writes.
//! Must not: talk to the server, or decide what a pass does (core's).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use beebeeb_core::backup::{BackupRecord, FolderPresence, RecordState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What the settings file holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Settings {
    /// The account these settings belong to (`GET /auth/me` id).
    pub account_id: String,
    /// This machine's folder under the Backups root, once found or made.
    #[serde(default)]
    pub device_folder_id: Option<Uuid>,
    /// When `.device.json` was last written (ms since the epoch), and the
    /// folder list it was written with, so it is rewritten only on a change.
    #[serde(default)]
    pub manifest_written_ms: Option<i64>,
    #[serde(default)]
    pub manifest_folders: Vec<String>,
    #[serde(default)]
    pub sources: Vec<Source>,
}

/// One backed-up folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Source {
    /// The folder, canonical (absolute, no links).
    pub path: PathBuf,
    /// A `beebeeb_types::backup::Category` key (`"pictures"`, `"custom:<name>"`).
    pub category: String,
    /// The number of its records file.
    pub records_file: u32,
    #[serde(default)]
    pub category_folder_id: Option<Uuid>,
    /// Set when the backup turned off: its folder was deleted elsewhere (spec
    /// §4.2 rule 11, Amendment 12). It stays off until `bb backup add` again.
    #[serde(default)]
    pub off: Option<Off>,
    #[serde(default)]
    pub last_pass: Option<LastPass>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OffReason {
    CategoryDeleted,
    DeviceDeleted,
}

impl OffReason {
    pub(crate) fn from_presence(p: FolderPresence) -> Option<OffReason> {
        match p {
            FolderPresence::Present => None,
            FolderPresence::CategoryDeleted => Some(OffReason::CategoryDeleted),
            FolderPresence::DeviceDeleted => Some(OffReason::DeviceDeleted),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Off {
    pub reason: OffReason,
    pub at_ms: i64,
}

/// The counts of the last pass (what `bb backup status` prints).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LastPass {
    pub at_ms: i64,
    pub uploaded: usize,
    pub failed: usize,
    pub moved: usize,
    pub unchanged: usize,
    pub deferred: usize,
    pub skipped: usize,
    pub gone_locally: usize,
}

/// A record as stored: core's `BackupRecord`, with its state as a word.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredRecord {
    key: String,
    size: u64,
    modified_ms: i64,
    remote_id: Uuid,
    uploaded_at_ms: i64,
    state: RecordState,
}

impl From<&BackupRecord> for StoredRecord {
    fn from(r: &BackupRecord) -> Self {
        StoredRecord {
            key: r.key.clone(),
            size: r.size,
            modified_ms: r.modified_ms,
            remote_id: r.remote_id,
            uploaded_at_ms: r.uploaded_at_ms,
            state: r.state,
        }
    }
}

impl From<StoredRecord> for BackupRecord {
    fn from(r: StoredRecord) -> Self {
        BackupRecord {
            key: r.key,
            size: r.size,
            modified_ms: r.modified_ms,
            remote_id: r.remote_id,
            uploaded_at_ms: r.uploaded_at_ms,
            state: r.state,
        }
    }
}

/// One line of a records file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Line {
    Put(StoredRecord),
    Delete { key: String },
}

/// The state dir: `<config dir>/beebeeb`, as every other CLI state file.
pub(crate) fn state_dir() -> PathBuf {
    crate::resume::state_dir()
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("backup.json")
}

fn records_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("backup-records-{n}.jsonl"))
}

/// Whether the journal at `path` ends inside a line (a run died mid-write).
fn ends_mid_line(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// Write `bytes` to `path` through a private temp file and a rename, so a
/// crash leaves the old file or the new one, never half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("state path has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create the state folder: {e}"))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|e| format!("cannot write the backup state: {e}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("cannot write the backup state: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write the backup state: {e}"))
}

/// The settings and every source's records.
pub(crate) struct Store {
    dir: PathBuf,
    pub settings: Settings,
}

impl Store {
    /// Open the store in `dir` for `account_id`. A store of another account is
    /// never used: `Ok(None)` tells the caller to say so; `reset` replaces it.
    pub(crate) fn open(dir: &Path, account_id: &str) -> Result<Opened, String> {
        let path = settings_path(dir);
        let settings = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Settings>(&bytes)
                .map_err(|e| format!("the backup settings in {} cannot be read: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings {
                account_id: account_id.to_string(),
                ..Settings::default()
            },
            Err(e) => return Err(format!("cannot read the backup settings: {e}")),
        };
        if settings.account_id != account_id {
            return Ok(Opened::OtherAccount);
        }
        Ok(Opened::Mine(Store {
            dir: dir.to_path_buf(),
            settings,
        }))
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.settings).map_err(|e| e.to_string())?;
        write_atomic(&settings_path(&self.dir), &bytes)
    }

    /// A records-file number no source uses.
    pub(crate) fn next_records_file(&self) -> u32 {
        self.settings
            .sources
            .iter()
            .map(|s| s.records_file + 1)
            .max()
            .unwrap_or(1)
    }

    /// [`Self::next_records_file`] with nothing left at that number: records an earlier store left there
    /// (another account's, after only `backup.json` was deleted, or one a failed removal kept) are never loaded
    /// as a new source's own, where core would take every unchanged file as removed elsewhere and upload none.
    pub(crate) fn fresh_records_file(&self) -> Result<u32, String> {
        let n = self.next_records_file();
        match std::fs::remove_file(records_path(&self.dir, n)) {
            Ok(()) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(n),
            Err(e) => Err(format!("cannot clear old backup records: {e}")),
        }
    }

    /// Drop a source's records file.
    pub(crate) fn forget_records(&self, n: u32) {
        let _ = std::fs::remove_file(records_path(&self.dir, n));
    }

    pub(crate) fn records(&self, n: u32) -> Result<Records, String> {
        Records::load(records_path(&self.dir, n))
    }
}

pub(crate) enum Opened {
    Mine(Store),
    OtherAccount,
}

/// One source's records, by key, with the journal they are appended to.
pub(crate) struct Records {
    path: PathBuf,
    by_key: BTreeMap<String, BackupRecord>,
}

impl Records {
    fn load(path: PathBuf) -> Result<Records, String> {
        let mut by_key = BTreeMap::new();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    // A torn last line (a run that died mid-write) is skipped;
                    // every complete line before it counts.
                    match serde_json::from_str::<Line>(line) {
                        Ok(Line::Put(r)) => {
                            by_key.insert(r.key.clone(), BackupRecord::from(r));
                        }
                        Ok(Line::Delete { key }) => {
                            by_key.remove(&key);
                        }
                        Err(_) => {}
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot read the backup records: {e}")),
        }
        Ok(Records { path, by_key })
    }

    pub(crate) fn all(&self) -> Vec<BackupRecord> {
        self.by_key.values().cloned().collect()
    }

    pub(crate) fn len(&self) -> usize {
        self.by_key.len()
    }

    /// The records whose copy is in the backup: one removed elsewhere is not.
    pub(crate) fn backed_up(&self) -> usize {
        self.by_key
            .values()
            .filter(|r| r.state == RecordState::BackedUp)
            .count()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&BackupRecord> {
        self.by_key.get(key)
    }

    pub(crate) fn find_remote(&self, remote_id: Uuid) -> Option<&BackupRecord> {
        self.by_key.values().find(|r| r.remote_id == remote_id)
    }

    fn append(&self, lines: &[Line]) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create the state folder: {e}"))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.path)
            .map_err(|e| format!("cannot write the backup records: {e}"))?;
        // After a torn last line, start a new one: glued onto the fragment, this record would be lost with it.
        let mut text = if ends_mid_line(&self.path) {
            String::from("\n")
        } else {
            String::new()
        };
        for line in lines {
            text.push_str(&serde_json::to_string(line).map_err(|e| e.to_string())?);
            text.push('\n');
        }
        file.write_all(text.as_bytes())
            .map_err(|e| format!("cannot write the backup records: {e}"))
    }

    /// Store `record` at its key, and drop every other record of the same
    /// remote file, so one remote file never has two keys.
    pub(crate) fn put(&mut self, record: BackupRecord) -> Result<(), String> {
        let others: Vec<String> = self
            .by_key
            .values()
            .filter(|r| r.remote_id == record.remote_id && r.key != record.key)
            .map(|r| r.key.clone())
            .collect();
        let mut lines: Vec<Line> = others.iter().map(|k| Line::Delete { key: k.clone() }).collect();
        lines.push(Line::Put(StoredRecord::from(&record)));
        self.append(&lines)?;
        for k in others {
            self.by_key.remove(&k);
        }
        self.by_key.insert(record.key.clone(), record);
        Ok(())
    }

    pub(crate) fn delete(&mut self, key: &str) -> Result<(), String> {
        if self.by_key.contains_key(key) {
            self.append(&[Line::Delete { key: key.to_string() }])?;
            self.by_key.remove(key);
        }
        Ok(())
    }

    /// Rewrite the journal as one line per record.
    pub(crate) fn compact(&self) -> Result<(), String> {
        let mut text = String::new();
        for r in self.by_key.values() {
            text.push_str(&serde_json::to_string(&Line::Put(StoredRecord::from(r))).map_err(|e| e.to_string())?);
            text.push('\n');
        }
        write_atomic(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: &str, remote: u128) -> BackupRecord {
        BackupRecord {
            key: key.to_string(),
            size: 3,
            modified_ms: 1_000,
            remote_id: Uuid::from_u128(remote),
            uploaded_at_ms: 2_000,
            state: RecordState::BackedUp,
        }
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cli-backup-state-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_survive_a_reload_and_a_remote_file_keeps_one_key() {
        let dir = scratch();
        let mut records = Records::load(records_path(&dir, 1)).unwrap();
        records.put(record("a.txt", 1)).unwrap();
        records.put(record("b.txt", 2)).unwrap();
        // The same remote file under a new key: the old key goes.
        records.put(record("moved/a.txt", 1)).unwrap();
        records.delete("b.txt").unwrap();
        let again = Records::load(records_path(&dir, 1)).unwrap();
        assert_eq!(again.all(), vec![record("moved/a.txt", 1)]);
        again.compact().unwrap();
        let compacted = std::fs::read_to_string(records_path(&dir, 1)).unwrap();
        assert_eq!(compacted.lines().count(), 1, "{compacted}");
    }

    #[test]
    fn a_torn_last_line_loses_only_itself() {
        let dir = scratch();
        let mut records = Records::load(records_path(&dir, 1)).unwrap();
        records.put(record("a.txt", 1)).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(records_path(&dir, 1))
            .unwrap();
        f.write_all(b"{\"op\":\"put\",\"key\":\"b.t").unwrap();
        assert_eq!(Records::load(records_path(&dir, 1)).unwrap().len(), 1);
    }

    #[test]
    fn a_record_appended_after_a_torn_line_is_kept() {
        let dir = scratch();
        let mut records = Records::load(records_path(&dir, 1)).unwrap();
        records.put(record("a.txt", 1)).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(records_path(&dir, 1))
            .unwrap();
        f.write_all(b"{\"op\":\"put\",\"key\":\"b.t").unwrap();
        let mut records = Records::load(records_path(&dir, 1)).unwrap();
        records.put(record("c.txt", 3)).unwrap();
        let again = Records::load(records_path(&dir, 1)).unwrap();
        assert_eq!(again.all(), vec![record("a.txt", 1), record("c.txt", 3)]);
    }

    #[test]
    fn a_new_source_never_loads_records_left_at_its_number() {
        let dir = scratch();
        // Account A's records; then only backup.json was deleted, and account B adds a folder.
        let mut left = Records::load(records_path(&dir, 1)).unwrap();
        left.put(record("a.txt", 1)).unwrap();
        let Opened::Mine(store) = Store::open(&dir, "user-b").unwrap() else {
            panic!("a new store is mine");
        };
        let n = store.fresh_records_file().unwrap();
        assert_eq!(n, 1);
        assert_eq!(store.records(n).unwrap().len(), 0);
    }

    #[test]
    fn a_record_removed_elsewhere_is_not_counted_as_backed_up() {
        let dir = scratch();
        let mut records = Records::load(records_path(&dir, 1)).unwrap();
        records.put(record("a.txt", 1)).unwrap();
        records
            .put(BackupRecord {
                state: RecordState::RemovedElsewhere,
                ..record("b.txt", 2)
            })
            .unwrap();
        assert_eq!((records.len(), records.backed_up()), (2, 1));
    }

    #[test]
    fn another_accounts_store_is_never_opened() {
        let dir = scratch();
        let Opened::Mine(store) = Store::open(&dir, "user-a").unwrap() else {
            panic!("a new store is mine");
        };
        store.save().unwrap();
        assert!(matches!(Store::open(&dir, "user-b").unwrap(), Opened::OtherAccount));
        assert!(matches!(Store::open(&dir, "user-a").unwrap(), Opened::Mine(_)));
    }
}
