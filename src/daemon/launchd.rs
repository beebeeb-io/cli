//! The macOS LaunchAgent back-end: installing, disarming and removing
//! `io.beebeeb.sync.<slug>` plists, and rendering their XML.
//!
//! Owns: the plist template and its escaping/env merge, `managed_launchd_label`,
//! `disarm_launchagent_if_managed`, `run_bounded`, `running_as_service`, and the
//! macOS/no-op `install_launchagent`/`uninstall_launchagent` pair.
//! Must not: be reached without passing `daemon::home`'s guard, or call
//! `launchctl` directly instead of `service_manager`.
//! Tests: `daemon::migrate`'s `restart_policy_tests` covers the template's
//! restart policy and label filter.

use super::*;

/// Escape user-controllable text for XML plist inclusion.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Install a macOS LaunchAgent plist for the given sync session.
///
/// The plist label is `io.beebeeb.sync.<slug>` to allow multiple
/// concurrent sync sessions.
#[cfg(target_os = "macos")]
pub fn install_launchagent(local_dir: &Path, remote_path: &str, slug: &str) -> Result<(), DaemonError> {
    // Before anything is written: a plist under a redirected HOME must not exist at all.
    ensure_real_home()?;
    let plist_dir = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join("Library/LaunchAgents");
    fs::create_dir_all(&plist_dir).map_err(|e| DaemonError::Other(format!("create LaunchAgents dir: {e}")))?;

    let label = format!("io.beebeeb.sync.{slug}");
    let plist_path = plist_dir.join(format!("{label}.plist"));

    let exe = std::env::current_exe().map_err(|e| DaemonError::Other(format!("current_exe: {e}")))?;
    let logs = log_dir()?;

    let args = [
        exe.to_string_lossy().to_string(),
        "sync".to_string(),
        local_dir.to_string_lossy().to_string(),
        remote_path.to_string(),
    ];

    let args_xml: String = args
        .iter()
        .map(|a| format!("        <string>{}</string>", xml_escape(a)))
        .collect::<Vec<_>>()
        .join("\n");

    let stdout_log = logs.join(format!("{slug}.log"));
    let stderr_log = logs.join(format!("{slug}.err"));

    let plist = render_plist_with_env(
        &label,
        &args_xml,
        &stdout_log.to_string_lossy(),
        &stderr_log.to_string_lossy(),
        &env_xml_with_transport(crate::transport::selected_name()),
    );

    fs::write(&plist_path, &plist).map_err(|e| DaemonError::Other(format!("write plist: {e}")))?;

    let status = service_manager("launchctl", &["load", &plist_path.to_string_lossy()])?;

    if !status.success() {
        return Err(DaemonError::Other("launchctl load failed".to_string()));
    }

    Ok(())
}

/// The `EnvironmentVariables` entry every managed sync plist carries.
pub(super) const MARKER_ENV_XML: &str = "        <key>BEEBEEB_SYNC_SERVICE</key>\n        <string>1</string>";

/// The marker env plus the transport the installing invocation selected, so the service moves files
/// the way the user asked and not the way a stray BB_TRANSPORT in launchd's environment says.
#[cfg(any(target_os = "macos", test))]
pub(super) fn env_xml_with_transport(transport: &str) -> String {
    format!("{MARKER_ENV_XML}\n        <key>BB_TRANSPORT</key>\n        <string>{transport}</string>")
}

pub fn render_plist(label: &str, args_xml: &str, stdout_log: &str, stderr_log: &str) -> String {
    render_plist_with_env(label, args_xml, stdout_log, stderr_log, MARKER_ENV_XML)
}

/// The launchd job label when this process was started by one of our sync
/// LaunchAgents (`io.beebeeb.sync.*`), from `XPC_SERVICE_NAME`.
pub fn managed_launchd_label(xpc_service_name: Option<&str>) -> Option<String> {
    xpc_service_name
        .filter(|l| l.starts_with("io.beebeeb.sync."))
        .map(String::from)
}

/// launchd cannot key KeepAlive on a specific exit code, so when a sync run
/// ends because its session is dead we take ourselves out of launchd's loaded
/// set (`launchctl remove`; the plist stays, so the next login retries once).
///
/// Call this only AFTER the session-ended message has been written and
/// flushed: `launchctl remove` makes launchd SIGTERM this very process, which
/// (task 1872 r3 launchd run) swallowed the message when it ran first. We
/// ignore SIGTERM/SIGHUP for the duration, run `launchctl remove` and wait for
/// it (bounded), and only then let the caller exit 77. Removal therefore
/// lands before we exit, so KeepAlive never sees the failed exit and there is
/// no restart (and no orphaned helper process to outlive the job).
pub fn disarm_launchagent_if_managed() {
    #[cfg(target_os = "macos")]
    if let Some(label) = managed_launchd_label(std::env::var("XPC_SERVICE_NAME").ok().as_deref()) {
        // SAFETY: SIG_IGN installs no handler code; signal() is async-signal-safe.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        let mut cmd = std::process::Command::new("launchctl");
        cmd.args(["remove", &label]);
        run_bounded(&mut cmd, std::time::Duration::from_secs(5));
    }
}

/// Spawn `cmd` and wait up to `limit`. On timeout the child is killed and
/// reaped (a dropped `Child` neither kills nor waits). Returns true if it
/// exited on its own within the bound.
#[cfg(any(target_os = "macos", test))]
pub(super) fn run_bounded(cmd: &mut std::process::Command, limit: std::time::Duration) -> bool {
    let Ok(mut child) = cmd.spawn() else { return false };
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// True when this process runs as one of our managed sync services
/// (launchd label `io.beebeeb.sync.*`, or the `BEEBEEB_SYNC_SERVICE=1` marker
/// our generated systemd unit and launchd plist set). `INVOCATION_ID` is NOT a
/// signal: systemd sets it for every unit and children inherit it, so ordinary
/// interactive terminals would be misdetected.
pub fn running_as_service(xpc_service_name: Option<&str>, service_marker: Option<&str>) -> bool {
    managed_launchd_label(xpc_service_name).is_some() || service_marker == Some("1")
}

/// Unload and remove a macOS LaunchAgent plist for the given slug.
#[cfg(target_os = "macos")]
pub fn uninstall_launchagent(slug: &str) -> Result<bool, DaemonError> {
    let label = format!("io.beebeeb.sync.{slug}");
    let agents = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join("Library/LaunchAgents");
    let plist_path = agents.join(format!("{label}.plist"));
    // Also the legacy single-plist name.
    let legacy_path = agents.join("io.beebeeb.sync.plist");
    let target = if plist_path.exists() {
        plist_path
    } else if legacy_path.exists() {
        legacy_path
    } else {
        // Nothing installed here, nothing to unload: not a reason to refuse (and a caller that
        // loops over sessions must not stop at the first slug that has no service).
        return Ok(false);
    };
    ensure_real_home()?;
    let _ = service_manager("launchctl", &["unload", &target.to_string_lossy()]);
    fs::remove_file(&target).map_err(|e| DaemonError::Other(format!("remove plist: {e}")))?;
    Ok(true)
}

/// No-op on non-macOS for LaunchAgent install.
#[cfg(not(target_os = "macos"))]
pub fn install_launchagent(_local_dir: &Path, _remote_path: &str, _slug: &str) -> Result<(), DaemonError> {
    Ok(())
}

/// No-op on non-macOS for LaunchAgent uninstall.
#[cfg(not(target_os = "macos"))]
pub fn uninstall_launchagent(_slug: &str) -> Result<bool, DaemonError> {
    Ok(false)
}
