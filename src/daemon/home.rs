//! The real-home guard for the service managers: a LaunchAgent or systemd unit
//! installed from a redirected HOME would land in the real user's domain, so
//! every install/remove/migrate is refused unless HOME is the account's home.
//!
//! Owns: `account_home`, `CUSTOM_HOME_OVERRIDE`, `same_dir`, `check_home`,
//! `check_config_dir`, `ensure_real_home`.
//! Must not: call `launchctl`/`systemctl` itself (see `daemon::spawn`).
//! Tests: the guard's cases live in `daemon::spawn`'s `transport_tests`.

use super::*;

// ── the background-service managers (launchd, systemd) ─────────────────────
//
// launchd and `systemd --user` run a job as the real user with the account's real home directory.
// They know nothing of a HOME that a test, a wrapper or a container redirected. A service installed,
// removed or migrated from such a process would be loaded into the REAL user's service domain while
// its files sit under the fake home, and the job it loads reads the REAL config: a live session.
// (A test once did exactly that: `bb sync --daemon` under a scratch HOME loaded a job that ran against
// production with the developer's session.) So the managers are only ever spoken to when HOME is the
// account's home. EVERY `launchctl` / `systemctl` call goes through [`service_manager`]; a source scan
// in the tests fails if one does not.

/// The account's home directory from the passwd database, not from `$HOME`.
#[cfg(unix)]
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(super) fn account_home() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    // 8 KiB, and more when the entry does not fit (ERANGE: a long group list or gecos field).
    for size in [8 * 1024usize, 64 * 1024, 1024 * 1024] {
        let mut buf = vec![0u8; size];
        // SAFETY: `getpwuid_r` writes into the buffers we own and sets `out` to null or to `pwd`; the
        // directory string it points into lives in `buf`, which outlives the copy below.
        unsafe {
            let mut pwd: libc::passwd = std::mem::zeroed();
            let mut out: *mut libc::passwd = std::ptr::null_mut();
            let rc = libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut out);
            if rc == libc::ERANGE {
                continue;
            }
            if rc != 0 || out.is_null() || pwd.pw_dir.is_null() {
                return None;
            }
            // Bytes, not UTF-8: a home directory need not be valid UTF-8.
            return Some(PathBuf::from(OsStr::from_bytes(CStr::from_ptr(pwd.pw_dir).to_bytes())));
        }
    }
    None
}

#[cfg(not(unix))]
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(super) fn account_home() -> Option<PathBuf> {
    None
}

/// Set to `1` to let a service be installed (or removed) from a HOME that is not the account's home.
/// For a real user with an unusual but intended setup (`sudo -E`, a relocated home directory). Tests
/// never set it.
pub const CUSTOM_HOME_OVERRIDE: &str = "BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME";

/// Is `a` the same directory as `b` (equal, or the same after resolving symlinks)?
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// `Err` when `home` is not the account's home, unless `allow` (the override variable's value) is
/// `1`. When the account's home cannot be determined at all (no passwd entry) it FAILS CLOSED: not
/// knowing is not a reason to load a service into someone's launchd; the override says "I am sure".
/// Only an unknown `home` (nothing to compare) passes.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(super) fn check_home(home: Option<&Path>, account: Option<&Path>, allow: Option<&str>) -> Result<(), String> {
    if allow == Some("1") {
        return Ok(());
    }
    let Some(home) = home else {
        return Ok(());
    };
    let Some(account) = account else {
        return Err(format!(
            "refusing to touch the background-service manager: HOME is {} but this account's home \
             directory cannot be determined (no passwd entry). If you are sure, run the command again \
             with {CUSTOM_HOME_OVERRIDE}=1",
            home.display()
        ));
    };
    if same_dir(home, account) {
        Ok(())
    } else {
        Err(format!(
            "refusing to touch the background-service manager: HOME is {} but this account's home is {}. \
             launchd and systemd run services with the account's home, so the service would use that \
             home's config and session, not this one. If this HOME is intended (`sudo -E`, a relocated \
             home directory), run the command again with {CUSTOM_HOME_OVERRIDE}=1",
            home.display(),
            account.display()
        ))
    }
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(super) fn ensure_real_home() -> Result<(), String> {
    let allow = std::env::var(CUSTOM_HOME_OVERRIDE).ok();
    let account = account_home();
    check_home(dirs::home_dir().as_deref(), account.as_deref(), allow.as_deref())?;
    #[cfg(target_os = "linux")]
    check_config_dir(dirs::config_dir().as_deref(), account.as_deref(), allow.as_deref())?;
    Ok(())
}
