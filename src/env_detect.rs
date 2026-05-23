//! Detect headless / SSH environments so `bb login` can skip the browser.
//!
//! Triggers (any one ⇒ headless):
//!   - `BB_LOGIN_HEADLESS=1` (explicit override)
//!   - `--headless` CLI flag (passed in by caller; this module just consults env)
//!   - On Unix: SSH session (`SSH_CLIENT` or `SSH_TTY` set) AND no display
//!     (`DISPLAY` and `WAYLAND_DISPLAY` both unset)
//!   - On Linux specifically: no display at all (`DISPLAY` and `WAYLAND_DISPLAY` unset)
//!
//! On macOS without SSH: always assume a window server (the user can dismiss
//! a wrong-browser prompt; that's better than failing to open at all).
//!
//! On Windows: always false — `start <url>` works even on Server Core in most
//! configurations, and Scoop users almost certainly have a desktop session.

use std::env;

/// Read the live process environment and decide whether we should suppress
/// `open::that` and fall back to code-only display.
pub fn is_headless() -> bool {
    is_headless_with(EnvSnapshot::current())
}

/// A pure function over an environment snapshot — used by unit tests so we
/// don't have to mutate the real process env.
pub(crate) fn is_headless_with(env: EnvSnapshot) -> bool {
    if env.bb_login_headless.as_deref() == Some("1") {
        return true;
    }

    let has_ssh = env.ssh_client.is_some() || env.ssh_tty.is_some();
    let has_display = env.display.is_some() || env.wayland_display.is_some();

    if cfg!(target_os = "windows") {
        // Windows: assume a desktop is reachable. If it isn't, the user can
        // still pass `--headless` explicitly (handled by the caller).
        return false;
    }

    if has_ssh && !has_display {
        return true;
    }

    if cfg!(target_os = "linux") && !has_display {
        // A bare Linux TTY with no display server — same UX as SSH.
        return true;
    }

    // macOS console session, or any Unix with a display server.
    false
}

#[derive(Debug, Default, Clone)]
pub(crate) struct EnvSnapshot {
    pub bb_login_headless: Option<String>,
    pub ssh_client: Option<String>,
    pub ssh_tty: Option<String>,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
}

impl EnvSnapshot {
    pub(crate) fn current() -> Self {
        Self {
            bb_login_headless: env::var("BB_LOGIN_HEADLESS").ok(),
            ssh_client: env::var("SSH_CLIENT").ok(),
            ssh_tty: env::var("SSH_TTY").ok(),
            display: env::var("DISPLAY").ok(),
            wayland_display: env::var("WAYLAND_DISPLAY").ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> EnvSnapshot {
        EnvSnapshot::default()
    }

    #[test]
    fn empty_env_is_not_headless_on_macos() {
        // macOS console session: no SSH, no DISPLAY. We still assume a window
        // server is reachable (Aqua doesn't set DISPLAY).
        if cfg!(target_os = "macos") {
            assert!(!is_headless_with(snap()));
        }
    }

    #[test]
    fn empty_env_is_headless_on_linux() {
        // Linux with no display server is a TTY — no point opening a browser.
        if cfg!(target_os = "linux") {
            assert!(is_headless_with(snap()));
        }
    }

    #[test]
    fn empty_env_is_not_headless_on_windows() {
        if cfg!(target_os = "windows") {
            assert!(!is_headless_with(snap()));
        }
    }

    #[test]
    fn explicit_override_always_wins() {
        let mut s = snap();
        s.bb_login_headless = Some("1".into());
        s.display = Some(":0".into());
        assert!(is_headless_with(s));
    }

    #[test]
    fn explicit_override_zero_is_not_a_yes() {
        let mut s = snap();
        s.bb_login_headless = Some("0".into());
        // Other rules still apply, but the env var itself doesn't force true.
        // On Linux with no display, the OS rule kicks in instead.
        if cfg!(target_os = "macos") {
            assert!(!is_headless_with(s.clone()));
        }
        if cfg!(target_os = "linux") {
            assert!(is_headless_with(s));
        }
    }

    #[test]
    fn ssh_without_display_is_headless() {
        let mut s = snap();
        s.ssh_client = Some("10.0.0.1 12345 22".into());
        // No DISPLAY — should fall back.
        if !cfg!(target_os = "windows") {
            assert!(is_headless_with(s));
        }
    }

    #[test]
    fn ssh_with_x_forwarding_is_not_headless() {
        let mut s = snap();
        s.ssh_client = Some("10.0.0.1 12345 22".into());
        s.display = Some("localhost:10.0".into());
        // X forwarding is active — try the browser.
        assert!(!is_headless_with(s));
    }

    #[test]
    fn ssh_tty_alone_counts_as_ssh() {
        let mut s = snap();
        s.ssh_tty = Some("/dev/pts/0".into());
        if !cfg!(target_os = "windows") {
            assert!(is_headless_with(s));
        }
    }

    #[test]
    fn wayland_display_counts_as_a_display() {
        let mut s = snap();
        s.ssh_client = Some("10.0.0.1 12345 22".into());
        s.wayland_display = Some("wayland-0".into());
        assert!(!is_headless_with(s));
    }
}
