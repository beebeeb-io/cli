//! One `bb push` per local file at a time, across processes (task 1950 review).
//!
//! A re-run of an interrupted push resumes the upload its resume record names.
//! A second push of the same unchanged file started while the FIRST is still
//! uploading matches that same record, and the resume store's mutex only
//! serialises threads of one process: both processes would then drive one
//! upload session (one fails at completion, or core's restart rule abandons the
//! live upload under the other).
//!
//! So `bb push` holds an OS advisory lock on a per-file lock file next to the
//! resume state for the whole upload, from before the conflict check to the
//! end. A second push finds it held and refuses (it does not wait: behind a
//! large upload that would look like a hang, and the first push may well
//! finish the file). The lock is `std::fs::File::try_lock`: `flock` on Unix,
//! `LockFileEx` on Windows. The kernel releases it when the holder exits, also
//! on SIGKILL, so a crashed push never blocks the re-run that resumes it.
//!
//! Lock files live at `<config_dir>/beebeeb/upload-locks/<sha256 of the
//! canonical path>.lock` and hold the holder's pid (shown in the refusal). They
//! are never deleted: unlinking a lock file another process has open would let
//! two holders lock two different files for one path.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Held for the duration of one upload; released on drop, or by the OS when
/// the process dies.
#[derive(Debug)]
pub struct UploadLock {
    _file: File,
}

#[derive(Debug)]
pub enum LockError {
    /// Another process holds the lock; its pid, if the lock file says.
    Held {
        pid: Option<u32>,
    },
    Io(String),
}

fn lock_path(path: &Path) -> PathBuf {
    let digest = Sha256::digest(crate::resume::key(path).as_bytes());
    let name: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    crate::resume::state_dir()
        .join("upload-locks")
        .join(format!("{name}.lock"))
}

/// Take the upload lock for `path` without waiting.
pub fn try_acquire(path: &Path) -> Result<UploadLock, LockError> {
    let lock = lock_path(path);
    if let Some(dir) = lock.parent() {
        std::fs::create_dir_all(dir).map_err(|e| LockError::Io(format!("create {}: {e}", dir.display())))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock)
        .map_err(|e| LockError::Io(format!("open {}: {e}", lock.display())))?;
    // `File::try_lock` is std since 1.89; `.clippy.toml` still says 1.85, but the
    // crate already needs 1.88 (let-chains) and every build pins 1.96.0.
    #[allow(clippy::incompatible_msrv)]
    let locked = file.try_lock();
    match locked {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            // Windows refuses reads of a locked file: no pid then.
            let pid = std::fs::read_to_string(&lock).ok().and_then(|s| s.trim().parse().ok());
            return Err(LockError::Held { pid });
        }
        Err(TryLockError::Error(e)) => return Err(LockError::Io(format!("lock {}: {e}", lock.display()))),
    }
    // Best effort: the pid only makes the refusal more helpful.
    let _ = file.set_len(0);
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();
    Ok(UploadLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A second handle (what a second process opens) cannot take the lock while
    /// the first holds it, sees the holder's pid, and gets it once the first is
    /// dropped. Two different files never block each other.
    #[test]
    fn the_lock_is_exclusive_per_file_and_released_on_drop() {
        let _env = crate::resume::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("bb-upload-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialised by TEST_ENV_LOCK (every user of this var takes it).
        unsafe { std::env::set_var("BB_PENDING_UPLOADS_PATH", dir.join("pending-uploads.json")) };
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();

        let first = try_acquire(&a).expect("first push takes the lock");
        match try_acquire(&a) {
            Err(LockError::Held { pid }) => {
                if cfg!(unix) {
                    assert_eq!(pid, Some(std::process::id()), "the refusal names the holder");
                }
            }
            other => panic!("a second push of the same file must be refused, got {other:?}"),
        }
        let other_file = try_acquire(&b).expect("a different file is not blocked");
        drop(first);
        let again = try_acquire(&a).expect("released when the holder lets go");

        drop((other_file, again));
        unsafe { std::env::remove_var("BB_PENDING_UPLOADS_PATH") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
