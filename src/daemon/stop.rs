//! Stopping one daemon, or every daemon this account has running.
//!
//! Owns: `stop_daemon`, `stop_all_daemons`.
//! Must not: install or remove a service definition — a stopped daemon keeps
//! its LaunchAgent/systemd unit (see `daemon::launchd`, `daemon::systemd`).
//! Tests: covered end-to-end from `tests/`; the kill signal itself is in
//! `daemon::paths`.

use super::*;

/// Stop a daemon by slug: kill the process, remove PID file, and
/// uninstall platform auto-start (LaunchAgent / systemd unit).
pub fn stop_daemon(slug: &str) -> Result<bool, DaemonError> {
    let killed = kill_daemon(slug)?;
    let la_removed = uninstall_launchagent(slug)?;
    let sd_removed = uninstall_systemd_unit(slug)?;
    Ok(killed || la_removed || sd_removed)
}

/// Stop all daemons: iterate all PID files and stop each one.
pub fn stop_all_daemons() -> Result<u32, DaemonError> {
    let slugs = list_daemon_slugs()?;
    let mut count = 0u32;
    for slug in &slugs {
        if stop_daemon(slug)? {
            count += 1;
        }
    }
    Ok(count)
}
