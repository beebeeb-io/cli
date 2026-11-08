//! A per-user record of the download temp files this CLI is writing (task 2420).
//!
//! A file name cannot tell our temp from a user's own file (`foo.tmp`,
//! `x.<16 hex>.tmp` and `y.bbtmp` are all names other tools use), so the
//! download writers (`bb pull`, `--zip`, direct download) record the absolute
//! path of each temp they create, and `bb sync` acts on exactly those paths:
//! it leaves them out of the upload, and removes one only once its writer is
//! gone. It never deletes a file that is not recorded and never follows a link.
//!
//! One small JSON file per temp lives in `<state dir>/beebeeb/download-temps/`
//! (a file per temp means no shared file to rewrite, so two `bb` processes never
//! race). Liveness is an OS advisory lock: the writer holds its entry file
//! locked for the temp's whole life (`flock` on unix; on Windows the entry is
//! opened without write sharing). The OS drops the lock when the process dies,
//! killed or not, so "locked" means "the writer is alive" and age plays no part.
//!
//! A record that is missing, unreadable or corrupt means no exclusions and no
//! deletions. A temp created in the instant before its entry is written is not
//! recorded (the writer still removes it on every normal exit).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct Entry {
    v: u32,
    path: PathBuf,
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

#[cfg(unix)]
fn lock_exclusive(file: &File) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: a valid descriptor owned by `file` for the call.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// Open the entry for its writer: the lock is held until the handle is dropped.
fn create_locked(path: &Path) -> Option<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.share_mode(1); // FILE_SHARE_READ: nobody else may open it for writing
    }
    let file = opts.open(path).ok()?;
    #[cfg(unix)]
    if !lock_exclusive(&file) {
        return None;
    }
    Some(file)
}

/// Open an entry for the sweeper. `None` while its writer holds it.
fn open_if_writer_gone(path: &Path) -> Option<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true);
    let file = opts.open(path).ok()?;
    #[cfg(unix)]
    if !lock_exclusive(&file) {
        return None;
    }
    Some(file)
}

/// The record of one live temp. Dropping it (after the publish or the cleanup)
/// releases the lock and removes the entry.
pub(crate) struct TempRecord {
    entry: PathBuf,
    lock: Option<File>,
}

impl TempRecord {
    /// Record `temp` in the default directory. `None` when it cannot be recorded
    /// (the download goes on; sync then treats the temp as any file).
    pub(crate) fn register(temp: &Path) -> Option<Self> {
        Self::register_in(&record_dir(), temp)
    }

    pub(crate) fn register_in(dir: &Path, temp: &Path) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        let entry = dir.join(format!("{}.json", uuid::Uuid::new_v4()));
        let mut lock = create_locked(&entry)?;
        let body = serde_json::to_vec(&Entry {
            v: 1,
            path: absolute(temp),
        })
        .ok()?;
        if lock.write_all(&body).and_then(|_| lock.flush()).is_err() {
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

fn parse(file: &mut File) -> Option<PathBuf> {
    let mut body = Vec::new();
    file.read_to_end(&mut body).ok()?;
    let entry: Entry = serde_json::from_slice(&body).ok()?;
    (entry.v == 1 && entry.path.is_absolute()).then_some(entry.path)
}

/// Every recorded temp path, live or orphaned: all are ours, so none uploads.
pub(crate) fn recorded_paths() -> HashSet<PathBuf> {
    recorded_paths_in(&record_dir())
}

pub(crate) fn recorded_paths_in(dir: &Path) -> HashSet<PathBuf> {
    entry_files(dir)
        .into_iter()
        .filter_map(|p| parse(&mut File::open(p).ok()?))
        .collect()
}

/// Remove the recorded temps whose writer is gone, and their entries. Reads the
/// record only, never the tree. Returns how many temps it removed.
pub(crate) fn sweep() -> usize {
    sweep_in(&record_dir())
}

pub(crate) fn sweep_in(dir: &Path) -> usize {
    let mut removed = 0;
    for entry in entry_files(dir) {
        let Some(mut file) = open_if_writer_gone(&entry) else {
            continue; // the writer holds it: alive
        };
        match parse(&mut file) {
            Some(temp) => {
                // A regular file only: a link, a folder or nothing is left alone.
                let regular = std::fs::symlink_metadata(&temp).is_ok_and(|m| m.file_type().is_file());
                if regular && std::fs::remove_file(&temp).is_ok() {
                    removed += 1;
                }
                drop(file);
                if !regular || !temp.exists() {
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

/// The names beside `output` that core's `TempSibling` could pick for it.
fn family(output: &Path) -> HashSet<std::ffi::OsString> {
    let (Some(parent), Some(name)) = (output.parent(), output.file_name()) else {
        return HashSet::new();
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let prefix = format!("{}.", name.to_string_lossy());
    std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| {
            let n = n.to_string_lossy();
            n.starts_with(&prefix) && n.ends_with(".tmp")
        })
        .collect()
}

/// Create the temp beside `output` through core's `TempSibling`, then record the
/// path it chose: the one name in `output`'s temp family that was not there
/// before. A name that was already taken is the user's and is never recorded.
pub(crate) fn create_recorded(
    output: &Path,
) -> std::io::Result<(crate::safe_path::TempSibling, File, Option<TempRecord>)> {
    let before = family(output);
    let (temp, file) = crate::safe_path::TempSibling::create(output)?;
    let mut fresh: Vec<_> = family(output).difference(&before).cloned().collect();
    #[cfg(unix)]
    if fresh.len() > 1 {
        use std::os::unix::fs::MetadataExt;
        if let Ok(mine) = file.metadata() {
            let dir = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fresh.retain(|n| {
                std::fs::symlink_metadata(dir.join(n)).is_ok_and(|m| m.dev() == mine.dev() && m.ino() == mine.ino())
            });
        }
    }
    let record = match fresh.as_slice() {
        [name] => {
            let dir = output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            TempRecord::register(&dir.join(name))
        }
        _ => None,
    };
    Ok((temp, file, record))
}
