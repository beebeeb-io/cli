//! Files that may hold a secret (`config.json`, `credentials.json`; LD-0029, decision F4).
//!
//! No other user may read such a file at any moment, under any umask. So a file is never written in place: a
//! temporary file in the same directory is created with `create_new` and mode 0600 (unix), or with sharing denied
//! and an owner-only DACL (Windows), BEFORE any byte goes into it. It is then written, fsynced, renamed over the
//! target, and the directory is fsynced. A reader sees the old file or the new one, never a torn or a wider one.
//!
//! The previous code did `std::fs::write(path)` and only then `chmod 0600`: between the two, the master key sat in
//! a file created with the umask's mode (0644 under the usual 022), and on Windows nothing was restricted at all.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` so that only the current user can ever read them (see the module docs).
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(test)]
    if let Some(mut observer) = OBSERVER.with(|o| o.borrow_mut().take()) {
        let result = write_observed(path, bytes, &mut *observer);
        OBSERVER.with(|o| *o.borrow_mut() = Some(observer));
        return result;
    }
    write_observed(path, bytes, &mut |_| {})
}

#[cfg(test)]
thread_local! {
    /// A test's look at every [`write`] made on its own thread (see [`observe`]).
    static OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(&Path)>>> = const { std::cell::RefCell::new(None) };
}

/// Tests: call `observer` at the seam of every [`write`] this thread makes, until [`stop_observing`].
#[cfg(test)]
pub(crate) fn observe(observer: Box<dyn FnMut(&Path)>) {
    OBSERVER.with(|o| *o.borrow_mut() = Some(observer));
}

#[cfg(test)]
pub(crate) fn stop_observing() {
    OBSERVER.with(|o| *o.borrow_mut() = None);
}

/// Tests: the process umask, set for the life of the guard. `umask` is process-wide; 022 is what every test runs
/// under on a normal machine anyway, so tests running beside it see nothing new.
/// Two such tests never overlap (one restoring its mask mid-way through the other's write would void it).
#[cfg(all(test, unix))]
pub(crate) struct TestUmask(libc::mode_t, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(all(test, unix))]
pub(crate) fn test_umask(mask: libc::mode_t) -> TestUmask {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: umask has no preconditions.
    TestUmask(unsafe { libc::umask(mask) }, turn)
}

#[cfg(all(test, unix))]
impl Drop for TestUmask {
    fn drop(&mut self) {
        // SAFETY: as above; restores the mask the test found.
        unsafe { libc::umask(self.0) };
    }
}

/// [`write`] with a seam: `written` is called with the path of the file that holds the bytes, once they are on
/// disk and before that file is published under `path`. Tests look at the file's mode at exactly that moment.
pub(crate) fn write_observed(path: &Path, bytes: &[u8], written: &mut dyn FnMut(&Path)) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} is not a file path", path.display()))?;
    let directory = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    create_private_dir(&directory).map_err(|e| format!("failed to create {}: {e}", directory.display()))?;
    let temporary = directory.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file =
            create_private(&temporary).map_err(|e| format!("failed to create {}: {e}", temporary.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("failed to write {}: {e}", temporary.display()))?;
        drop(file);
        written(&temporary);
        std::fs::rename(&temporary, path).map_err(|e| format!("failed to replace {}: {e}", path.display()))?;
        sync_directory(&directory).map_err(|e| format!("failed to sync {}: {e}", directory.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// The directory that holds the file. A directory this creates is 0700 on unix (an existing one is left as it
/// is: it may be shared, e.g. `~/.config`, and the files in it are 0600 either way).
fn create_private_dir(directory: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)
}

/// A new file, readable and writable by its owner only, from the moment it exists.
#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    // The umask can only take bits away from 0600; set it exactly, on the descriptor, before anything is written.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// A new file nobody else can open while it is being written (sharing denied), with an owner-only, protected DACL
/// set before any byte is written. The DACL stays when the handle is closed and the file renamed.
#[cfg(windows)]
fn create_private(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC};
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .access_mode(FILE_GENERIC_WRITE | WRITE_DAC | READ_CONTROL)
        .open(path)?;
    windows_owner_only::restrict(&file)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn create_private(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Make the rename durable. Windows cannot open a directory for a sync; NTFS journals the rename itself.
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

/// Read a file written by [`write`]. `Ok(None)` when there is none.
pub fn read(path: &Path) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(zeroize::Zeroizing::new(bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("failed to read {}: {e}", path.display())),
    }
}

/// Remove a file written by [`write`]. `Ok(false)` when there was none.
pub fn remove(path: &Path) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("failed to remove {}: {e}", path.display())),
    }
}

#[cfg(windows)]
mod windows_owner_only {
    //! An owner-only DACL: one ACE that gives the current process's user full access, marked PROTECTED so that
    //! nothing is inherited from the directory (no SYSTEM, Administrators or Users entries).
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION, GetLengthSid,
        GetTokenInformation, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcessToken and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn restrict(file: &std::fs::File) -> std::io::Result<()> {
        // SAFETY: every pointer passed below points into a live, suitably aligned buffer of the stated length.
        unsafe {
            let mut raw: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let token = Token(raw);
            let mut needed = 0u32;
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // u64 storage keeps TOKEN_USER (pointer-aligned) correctly aligned.
            let mut user = vec![0u64; (needed as usize).div_ceil(8)];
            if GetTokenInformation(token.0, TokenUser, user.as_mut_ptr().cast(), needed, &mut needed) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let acl_len = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
                - std::mem::size_of::<u32>()
                + GetLengthSid(sid) as usize;
            let mut acl = vec![0u64; acl_len.div_ceil(8)];
            let acl = acl.as_mut_ptr().cast::<ACL>();
            if InitializeAcl(acl, acl_len as u32, ACL_REVISION) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS, sid) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let status = SetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                acl,
                std::ptr::null(),
            );
            if status != ERROR_SUCCESS {
                return Err(std::io::Error::from_raw_os_error(status as i32));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// F7 (a): under umask 022 the file is never wider than 0600, and that is observed at the moment the bytes
    /// are on disk but not yet published. The old `fs::write`-then-chmod showed 0644 there.
    #[cfg(unix)]
    #[test]
    fn a_secret_file_is_never_readable_by_others_under_umask_022() {
        let _mask = test_umask(0o022);
        let dir = tempdir();
        let path = dir.join("credentials.json");
        let secret = b"{\"master_key\":\"c2VjcmV0\"}";
        let mut seen = Vec::new();
        write_observed(&path, secret, &mut |p| {
            seen.push((mode(p), std::fs::read(p).unwrap()));
        })
        .unwrap();
        assert_eq!(seen.len(), 1, "the seam must have run once, with the bytes on disk");
        let (mode_while_written, bytes) = &seen[0];
        assert_eq!(bytes.as_slice(), secret, "the seam must see the written bytes");
        assert_eq!(
            mode_while_written & 0o077,
            0,
            "group/other bits set while the secret was on disk: {mode_while_written:o}"
        );
        assert_eq!(mode(&path), 0o600, "the published file must be 0600");
        assert_eq!(std::fs::read(&path).unwrap(), secret);
        // Replacing an existing, wider file: the result is 0600 too (a new inode, not the old one chmodded).
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, b"{}").unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_no_temporaries(&dir);
    }

    #[test]
    fn a_failed_write_leaves_the_old_file_and_no_temporary() {
        let dir = tempdir();
        let path = dir.join("config.json");
        write(&path, b"old").unwrap();
        // The target is a directory: the rename fails after the temporary was written.
        let blocked = dir.join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("x"), b"").unwrap();
        assert!(write(&blocked, b"new").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        assert_no_temporaries(&dir);
    }

    fn assert_no_temporaries(dir: &Path) {
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-private-file-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
