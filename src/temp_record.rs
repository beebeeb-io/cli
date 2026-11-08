//! A per-user record of the download temp files this CLI is writing (task 2420).
//!
//! A file name cannot tell our temp from a user's own file (`foo.tmp`,
//! `x.<16 hex>.tmp` and `y.bbtmp` are all names other tools use), so the
//! download writers (`bb pull`, `--zip`, direct download) record the absolute
//! path of each temp they create, and `bb sync` acts on exactly those paths:
//! it leaves them out of the upload, and removes one only once its writer is
//! gone. Each entry also stores the temp's file identity (device + inode on
//! unix, volume serial + file index on Windows); the sweep deletes a path only
//! when the file there still has that identity, so a name that was reused, by a
//! new pull or by the user, is never deleted. It never follows a link.
//! The directory is owner-only (0700, entries 0600) on unix: entries name
//! decrypted downloads.
//!
//! One small JSON file per temp lives in `<state dir>/beebeeb/download-temps/`
//! (a file per temp means no shared file to rewrite, so two `bb` processes never
//! race). Liveness is an OS advisory lock: the writer holds its entry file
//! locked for the temp's whole life (`std::fs::File::try_lock`, as `upload_lock.rs`
//! does: `flock` on unix, `LockFileEx` on Windows). The OS drops the lock when the process dies,
//! killed or not, so "locked" means "the writer is alive" and age plays no part.
//!
//! A record that is missing, unreadable or corrupt means no exclusions and no
//! deletions. A temp created in the instant before its entry is written is not
//! recorded (the writer still removes it on every normal exit).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A file's identity on its volume: (device, inode) on unix, (volume serial,
/// file index) on Windows.
type Identity = (u64, u64);

#[derive(Serialize, Deserialize)]
struct Entry {
    v: u32,
    path: PathBuf,
    /// Identity of the temp when it was recorded. An entry without one is never
    /// used to delete anything.
    #[serde(default)]
    id: Option<Identity>,
}

/// An entry with no readable content is left alone this long, so a writer that
/// has not finished writing its entry is not mistaken for a dead one.
const UNREADABLE_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
thread_local! {
    pub(crate) static DIR_OVERRIDE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Where the entries live.
pub(crate) fn record_dir() -> PathBuf {
    #[cfg(test)]
    let dir = DIR_OVERRIDE
        .with(|d| d.borrow().clone())
        .unwrap_or_else(|| std::env::temp_dir().join(format!("bb-temp-record-test-{}", std::process::id())));
    #[cfg(not(test))]
    let dir = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beebeeb")
        .join("download-temps");
    dir
}

/// `path` made absolute, with its folder resolved so a link in the way does not
/// change the answer. The file name itself is never resolved.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    match (abs.parent(), abs.file_name()) {
        (Some(parent), Some(name)) => parent.canonicalize().map(|p| p.join(name)).unwrap_or(abs),
        _ => abs,
    }
}

/// Identity of an open file.
#[cfg(unix)]
fn identity_of_file(file: &File) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|m| (m.dev(), m.ino()))
}

/// Identity of the entry at `path`, a link not followed. `None` when it is not
/// a regular file or cannot be read.
#[cfg(unix)]
fn identity_of_path(path: &Path) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).ok()?;
    m.file_type().is_file().then(|| (m.dev(), m.ino()))
}

#[cfg(windows)]
fn identity_of_file(file: &File) -> Option<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    // SAFETY: a valid handle owned by `file`; the struct is plain data.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) == 0 {
            return None;
        }
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        Some((u64::from(info.dwVolumeSerialNumber), index))
    }
}

#[cfg(windows)]
fn identity_of_path(path: &Path) -> Option<Identity> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS: open a link or
    // folder itself rather than its target; share mode 7 never blocks others.
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(7)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)
        .ok()?;
    let m = file.metadata().ok()?;
    if !m.file_type().is_file() {
        return None;
    }
    identity_of_file(&file)
}

#[cfg(not(any(unix, windows)))]
fn identity_of_file(_file: &File) -> Option<Identity> {
    None
}

#[cfg(not(any(unix, windows)))]
fn identity_of_path(_path: &Path) -> Option<Identity> {
    None
}

/// Create the entry file owner-only on unix.
fn create_new_entry(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

fn create_record_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// How the sweeper found an entry.
enum Open {
    /// Its writer is gone and we hold the lock now.
    Dead(File),
    /// Its writer holds the lock (the file is open for reading, content only).
    Alive(Option<PathBuf>),
    /// Cannot be opened or locked for another reason: left alone, reported.
    Unusable(std::io::Error),
}

fn open_for_sweep(path: &Path) -> Open {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    // Windows `LockFileEx` needs write access; unix `flock` does not.
    #[cfg(windows)]
    opts.write(true);
    let mut file = match opts.open(path) {
        Ok(f) => f,
        Err(e) => return Open::Unusable(e),
    };
    #[allow(clippy::incompatible_msrv)] // stable since the pinned 1.89+ toolchain; same note as `upload_lock.rs`
    match file.try_lock() {
        Ok(()) => Open::Dead(file),
        Err(TryLockError::WouldBlock) => Open::Alive(parse(&mut file).map(|e| e.path)),
        Err(TryLockError::Error(e)) => Open::Unusable(e),
    }
}

/// The record of one live temp. Dropping it (after the publish or the cleanup)
/// releases the lock and removes the entry.
pub(crate) struct TempRecord {
    entry: PathBuf,
    lock: Option<File>,
}

impl TempRecord {
    /// Record `temp`, the file `handle` is open on, in the default directory.
    /// `None` when it cannot be recorded (the download goes on; sync then
    /// treats the temp as any file).
    pub(crate) fn register(temp: &Path, handle: &File) -> Option<Self> {
        Self::register_in(&record_dir(), temp, handle)
    }

    pub(crate) fn register_in(dir: &Path, temp: &Path, handle: &File) -> Option<Self> {
        // Serialize first: nothing is created when it cannot be written.
        let body = serde_json::to_vec(&Entry {
            v: 1,
            path: absolute(temp),
            id: identity_of_file(handle),
        })
        .ok()?;
        create_record_dir(dir).ok()?;
        let entry = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
        let mut lock = create_new_entry(&entry).ok()?;
        #[allow(clippy::incompatible_msrv)] // same note as `upload_lock.rs`
        let locked = lock.try_lock().is_ok();
        if !locked || lock.write_all(&body).and_then(|_| lock.flush()).is_err() {
            drop(lock);
            let _ = std::fs::remove_file(&entry);
            return None;
        }
        Some(Self {
            entry,
            lock: Some(lock),
        })
    }

    /// Test only: the writer dies, the lock goes, the entry and the temp stay.
    #[cfg(test)]
    pub(crate) fn simulate_kill(mut self) {
        self.lock.take();
        self.entry = PathBuf::new();
    }
}

impl Drop for TempRecord {
    fn drop(&mut self) {
        if self.entry.as_os_str().is_empty() {
            return;
        }
        self.lock.take(); // Windows deletes only a closed file
        let _ = std::fs::remove_file(&self.entry);
    }
}

fn entry_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect()
}

fn parse(file: &mut File) -> Option<Entry> {
    let mut body = Vec::new();
    file.read_to_end(&mut body).ok()?;
    let entry: Entry = serde_json::from_slice(&body).ok()?;
    (entry.v == 1 && entry.path.is_absolute()).then_some(entry)
}

/// Every recorded temp path, live or orphaned: all are ours, so none uploads.
pub(crate) fn recorded_paths() -> HashSet<PathBuf> {
    recorded_paths_in(&record_dir())
}

pub(crate) fn recorded_paths_in(dir: &Path) -> HashSet<PathBuf> {
    entry_files(dir)
        .into_iter()
        .filter_map(|p| parse(&mut File::open(p).ok()?).map(|e| e.path))
        .collect()
}

/// Delete the recorded temp when, and only when, the file at its path is the
/// one that was recorded. Returns (deleted, drop the entry).
fn remove_recorded(temp: &Path, recorded: Option<Identity>) -> (bool, bool) {
    match std::fs::symlink_metadata(temp) {
        Ok(m) if m.file_type().is_file() => {}
        // A link or a folder is not ours and is never touched: the entry goes.
        Ok(_) => return (false, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Gone for good only when the folder is still there; a folder that
            // cannot be reached (offline volume, moved root) keeps the entry.
            let folder_ok = temp.parent().is_some_and(|p| p.is_dir());
            return (false, folder_ok);
        }
        Err(_) => return (false, false),
    }
    // A name that reaches a different entry through its 8.3 short name (task
    // 2408) is not ours either: skip it, keep the entry.
    if let (Some(dir), Some(name)) = (temp.parent(), temp.file_name()) {
        if !matches!(crate::safe_path::short_name_alias(dir, Path::new(name)), Ok(None)) {
            return (false, false);
        }
    }
    // No stored identity, or a different file at the path: not the recorded temp.
    if recorded.is_none() || identity_of_path(temp) != recorded {
        return (false, true);
    }
    match std::fs::remove_file(temp) {
        Ok(()) => (true, true),
        Err(_) => (false, false),
    }
}

/// Remove the recorded temps whose writer is gone, and their entries. Reads the
/// record only, never the tree. Returns how many temps it removed.
pub(crate) fn sweep() -> usize {
    sweep_in(&record_dir())
}

pub(crate) fn sweep_in(dir: &Path) -> usize {
    let mut removed = 0;
    let mut live: HashSet<PathBuf> = HashSet::new();
    let mut dead: Vec<(PathBuf, File)> = Vec::new();
    for entry in entry_files(dir) {
        match open_for_sweep(&entry) {
            Open::Alive(path) => live.extend(path),
            Open::Dead(file) => dead.push((entry, file)),
            Open::Unusable(e) => {
                // Reported once per sweep, the entry kept; never the full path.
                eprintln!(
                    "note: a download-temp record could not be read ({:?}); leaving it",
                    e.kind()
                );
            }
        }
    }
    for (entry, mut file) in dead {
        match parse(&mut file) {
            Some(rec) => {
                // A live writer names this path (a reused name): leave the entry's
                // file alone, the entry is stale.
                let (deleted, drop_entry) = if live.contains(&rec.path) {
                    (false, true)
                } else {
                    remove_recorded(&rec.path, rec.id)
                };
                removed += usize::from(deleted);
                drop(file);
                if drop_entry {
                    let _ = std::fs::remove_file(&entry);
                }
            }
            None => {
                // Unreadable: it names nothing, so nothing is deleted. Old ones go.
                let old = std::fs::metadata(&entry)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age >= UNREADABLE_GRACE);
                drop(file);
                if old {
                    let _ = std::fs::remove_file(&entry);
                }
            }
        }
    }
    removed
}

/// Create the temp beside `output` through core's `TempSibling`, then record the
/// path it chose. Core's `TempSibling` keeps its path private, so the name is
/// found with ONE listing of the folder after the create: the names of
/// `output`'s temp family whose file identity equals the handle we were given
/// (exact, so a neighbour's temp or a user's file is never taken for ours).
/// A `TempSibling::path()` in core would remove the listing (follow-up).
pub(crate) fn create_recorded(
    output: &Path,
) -> std::io::Result<(crate::safe_path::TempSibling, File, Option<TempRecord>)> {
    let (temp, file) = crate::safe_path::TempSibling::create(output)?;
    let record = recorded_name(output, &file).and_then(|p| TempRecord::register(&p, &file));
    Ok((temp, file, record))
}

fn recorded_name(output: &Path, file: &File) -> Option<PathBuf> {
    let (parent, name) = (output.parent()?, output.file_name()?);
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let mine = identity_of_file(file)?;
    let prefix = format!("{}.", name.to_string_lossy());
    let mut hits = std::fs::read_dir(parent)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(&prefix) && n.ends_with(".tmp")
        })
        .map(|e| e.path())
        .filter(|p| identity_of_path(p) == Some(mine));
    let first = hits.next()?;
    // Two names for one file (a hard link): ambiguous, record nothing.
    hits.next().is_none().then_some(first)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bb-2420c-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A temp file and a dead (killed-writer) entry for it.
    fn dead_entry(rec: &Path, temp: &Path) {
        let f = File::create(temp).unwrap();
        TempRecord::register_in(rec, temp, &f).unwrap().simulate_kill();
    }

    #[test]
    fn the_recorded_file_itself_is_deleted() {
        let (rec, root) = (scratch("r1"), scratch("d1"));
        let temp = root.join("a.pdf.tmp");
        dead_entry(&rec, &temp);
        assert_eq!(sweep_in(&rec), 1);
        assert!(!temp.exists());
        assert!(entry_files(&rec).is_empty());
    }

    #[test]
    fn a_reused_path_with_a_different_identity_is_kept() {
        let (rec, root) = (scratch("r2"), scratch("d2"));
        let temp = root.join("a.pdf.tmp");
        dead_entry(&rec, &temp);
        // The user removes the leftover and creates their own file at that name.
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, b"users own").unwrap();
        assert_eq!(sweep_in(&rec), 0);
        assert_eq!(std::fs::read(&temp).unwrap(), b"users own");
        assert!(entry_files(&rec).is_empty(), "the stale entry is dropped");
    }

    #[test]
    fn a_path_a_live_entry_also_names_is_kept() {
        let (rec, root) = (scratch("r3"), scratch("d3"));
        let temp = root.join("a.pdf.tmp");
        dead_entry(&rec, &temp);
        // A running pull now owns the same name (its own file at that path).
        std::fs::remove_file(&temp).unwrap();
        let live_file = File::create(&temp).unwrap();
        let _live = TempRecord::register_in(&rec, &temp, &live_file).unwrap();
        assert_eq!(sweep_in(&rec), 0);
        assert!(temp.exists(), "the live writer's temp was swept");
        assert_eq!(entry_files(&rec).len(), 1, "only the live entry remains");
    }

    #[test]
    fn the_record_is_owner_only() {
        let (rec, root) = (scratch("r4"), scratch("d4"));
        let rec = rec.join("nested");
        let temp = root.join("a.tmp");
        let f = File::create(&temp).unwrap();
        let _r = TempRecord::register_in(&rec, &temp, &f).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&rec), 0o700);
        assert_eq!(mode(&entry_files(&rec)[0]), 0o600);
    }

    #[test]
    fn an_entry_whose_folder_is_offline_is_kept() {
        let (rec, root) = (scratch("r5"), scratch("d5"));
        let folder = root.join("usb");
        std::fs::create_dir(&folder).unwrap();
        let temp = folder.join("a.tmp");
        dead_entry(&rec, &temp);
        std::fs::remove_dir_all(&folder).unwrap(); // volume gone
        assert_eq!(sweep_in(&rec), 0);
        assert_eq!(entry_files(&rec).len(), 1);
    }

    #[test]
    fn create_recorded_records_exactly_the_created_temp() {
        let root = scratch("d6");
        let out = root.join("a.pdf");
        // Another writer's temp for the same output is already there.
        std::fs::write(root.join("a.pdf.tmp"), b"theirs").unwrap();
        let (_t, file, rec) = create_recorded(&out).unwrap();
        let rec = rec.expect("recorded");
        let want = identity_of_file(&file);
        let entry = parse(&mut File::open(&rec.entry).unwrap()).unwrap();
        assert_eq!(identity_of_path(&entry.path), want);
        assert_ne!(entry.path, absolute(&root.join("a.pdf.tmp")));
    }
}
