//! Daemon process management for `bb sync --daemon`.
//!
//! Handles PID file lifecycle, process spawning, and platform-specific
//! auto-start integration (LaunchAgent on macOS, systemd user units on Linux).

use std::fs;
use std::path::{Path, PathBuf};

/// Directory for PID files: `~/.local/share/beebeeb/daemons/`
pub fn daemon_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or("cannot determine home directory")?;
    let dir = base.join("beebeeb").join("daemons");
    fs::create_dir_all(&dir).map_err(|e| format!("create daemon dir: {e}"))?;
    Ok(dir)
}

/// Directory for daemon log files: `~/.local/share/beebeeb/logs/`
pub fn log_dir() -> Result<PathBuf, String> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or("cannot determine home directory")?;
    let dir = base.join("beebeeb").join("logs");
    fs::create_dir_all(&dir).map_err(|e| format!("create log dir: {e}"))?;
    Ok(dir)
}

/// Convert a session name into a filesystem-safe slug.
///
/// Lowercases, replaces whitespace and special chars with hyphens,
/// collapses runs, and trims leading/trailing hyphens.
pub fn slugify(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    // Collapse consecutive hyphens
    let mut result = String::with_capacity(slug.len());
    let mut prev_hyphen = true; // Trim leading hyphens
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    // Trim trailing hyphen
    while result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        "daemon".to_string()
    } else {
        result
    }
}

/// Path to the PID file for a given session slug.
pub fn pid_path(slug: &str) -> Result<PathBuf, String> {
    Ok(daemon_dir()?.join(format!("{slug}.pid")))
}

/// Write a PID to the PID file for the given slug.
pub fn write_pid(slug: &str, pid: u32) -> Result<(), String> {
    let path = pid_path(slug)?;
    fs::write(&path, pid.to_string()).map_err(|e| format!("write PID file: {e}"))
}

/// Read the PID from a PID file, if it exists.
pub fn read_pid(slug: &str) -> Result<Option<u32>, String> {
    let path = pid_path(slug)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(|e| format!("read PID file: {e}"))?;
    match content.trim().parse::<u32>() {
        Ok(pid) => Ok(Some(pid)),
        Err(_) => {
            // Corrupt PID file — remove it
            let _ = fs::remove_file(&path);
            Ok(None)
        }
    }
}

/// Remove the PID file for the given slug.
pub fn remove_pid(slug: &str) -> Result<(), String> {
    let path = pid_path(slug)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("remove PID file: {e}"))?;
    }
    Ok(())
}

/// Check whether a process with the given PID is still running.
fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Check if a daemon for the given session slug is currently running.
/// Returns `Some(pid)` if running, `None` otherwise.
/// Cleans up stale PID files automatically.
pub fn is_daemon_running(slug: &str) -> Result<Option<u32>, String> {
    match read_pid(slug)? {
        Some(pid) => {
            if process_exists(pid) {
                Ok(Some(pid))
            } else {
                // Stale PID file — process no longer exists
                remove_pid(slug)?;
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

/// List all daemon slugs that have PID files (running or stale).
pub fn list_daemon_slugs() -> Result<Vec<String>, String> {
    let dir = daemon_dir()?;
    let mut slugs = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("read daemon dir: {e}"))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if let Some(slug) = name_str.strip_suffix(".pid") {
            slugs.push(slug.to_string());
        }
    }
    Ok(slugs)
}

/// Kill a daemon process by slug. Sends SIGTERM, removes PID file.
pub fn kill_daemon(slug: &str) -> Result<bool, String> {
    match read_pid(slug)? {
        Some(pid) => {
            #[cfg(unix)]
            if process_exists(pid) {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                if process_exists(pid) {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .status();
            }
            remove_pid(slug)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Spawn `bb sync <local> <remote>` as a detached daemon process.
///
/// Stdout/stderr are redirected to log files. Returns the child PID.
pub fn spawn_daemon(local_dir: &Path, remote_path: &str, slug: &str) -> Result<u32, String> {
    // Check if already running
    if let Some(pid) = is_daemon_running(slug)? {
        return Err(format!(
            "daemon already running for this session (PID {pid}). \
             Use bb sync --stop-session to stop it first."
        ));
    }

    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let logs = log_dir()?;

    let stdout_path = logs.join(format!("{slug}.log"));
    let stderr_path = logs.join(format!("{slug}.err"));

    let stdout_file = fs::File::create(&stdout_path).map_err(|e| format!("create log file: {e}"))?;
    let stderr_file = fs::File::create(&stderr_path).map_err(|e| format!("create error log file: {e}"))?;

    let child = std::process::Command::new(&exe)
        .args(["sync", &local_dir.to_string_lossy(), remote_path])
        .stdout(stdout_file)
        .stderr(stderr_file)
        // Detach from the parent process group
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn daemon: {e}"))?;

    let pid = child.id();
    write_pid(slug, pid)?;

    Ok(pid)
}

// ── Platform-specific auto-start integration ──────────────────────────────

/// Escape user-controllable text for XML plist inclusion.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(s: &str) -> String {
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
pub fn install_launchagent(local_dir: &Path, remote_path: &str, slug: &str) -> Result<(), String> {
    let plist_dir = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join("Library/LaunchAgents");
    fs::create_dir_all(&plist_dir).map_err(|e| format!("create LaunchAgents dir: {e}"))?;

    let label = format!("io.beebeeb.sync.{slug}");
    let plist_path = plist_dir.join(format!("{label}.plist"));

    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
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

    let plist = render_plist(
        &label,
        &args_xml,
        &stdout_log.to_string_lossy(),
        &stderr_log.to_string_lossy(),
    );

    fs::write(&plist_path, &plist).map_err(|e| format!("write plist: {e}"))?;

    let status = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .status()
        .map_err(|e| format!("launchctl load: {e}"))?;

    if !status.success() {
        return Err("launchctl load failed".to_string());
    }

    Ok(())
}

/// Render the sync LaunchAgent plist.
pub fn render_plist(label: &str, args_xml: &str, stdout_log: &str, stderr_log: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>30</integer>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>"#,
        xml_escape(stdout_log),
        xml_escape(stderr_log),
    )
}

/// Render the sync systemd user unit.
pub fn render_systemd_unit(slug: &str, exe: &str, local: &str, remote: &str) -> String {
    format!(
        r#"[Unit]
Description=Beebeeb Sync - {slug}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exe} sync {local} {remote}
Restart=on-failure
RestartSec=30
RestartPreventExitStatus=77

[Install]
WantedBy=default.target
"#,
        slug = slug,
        exe = exe,
        local = local,
        remote = remote,
    )
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
pub fn disarm_launchagent_if_managed() {
    #[cfg(target_os = "macos")]
    if let Some(label) = managed_launchd_label(std::env::var("XPC_SERVICE_NAME").ok().as_deref()) {
        let _ = std::process::Command::new("launchctl")
            .args(["remove", &label])
            .status();
    }
}

/// True when this process runs as one of our managed sync services
/// (launchd label `io.beebeeb.sync.*`, or a systemd unit via INVOCATION_ID).
pub fn running_as_service(xpc_service_name: Option<&str>, invocation_id: Option<&str>) -> bool {
    managed_launchd_label(xpc_service_name).is_some() || invocation_id.is_some_and(|v| !v.is_empty())
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let a = s.find(start)? + start.len();
    let b = s[a..].find(end)? + a;
    Some(&s[a..b])
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The string value following `<key>{key}</key>` in a plist.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    between(after, "<string>", "</string>").map(xml_unescape)
}

/// Rewrite a pre-1872 sync plist (blind `KeepAlive=true`, no throttle) to the
/// current template, keeping its label, arguments and log paths. `None` when
/// the content is not one of ours or already current.
pub fn migrate_plist_content(old: &str) -> Option<String> {
    let label = plist_string(old, "Label")?;
    if !label.starts_with("io.beebeeb.sync.") || old.contains("<key>SuccessfulExit</key>") {
        return None;
    }
    let args_xml = between(old, "<array>\n", "\n    </array>")?;
    let out = plist_string(old, "StandardOutPath")?;
    let err = plist_string(old, "StandardErrorPath")?;
    Some(render_plist(&label, args_xml, &out, &err))
}

/// Rewrite a pre-1872 sync systemd unit (`Restart=on-failure` with no
/// `RestartPreventExitStatus=77`) in place, keeping everything else.
pub fn migrate_unit_content(old: &str) -> Option<String> {
    let is_ours = old.contains("Description=Beebeeb Sync") && old.contains("ExecStart=");
    if !is_ours || old.contains("RestartPreventExitStatus=77") {
        return None;
    }
    let mut out = String::new();
    for line in old.lines() {
        if line.starts_with("Restart=")
            || line.starts_with("RestartSec=")
            || line.starts_with("RestartPreventExitStatus=")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if line.starts_with("ExecStart=") {
            out.push_str("Restart=on-failure\nRestartSec=30\nRestartPreventExitStatus=77\n");
        }
    }
    Some(out)
}

/// Rewrite old-template files of ours in `dir` (`prefix*suffix`); returns the
/// rewritten paths. Touches nothing else. Idempotent.
fn migrate_dir(dir: &Path, prefix: &str, suffix: &str, f: fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut done = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else { return done };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with(prefix) && name.ends_with(suffix)) {
            continue;
        }
        let path = e.path();
        if let Ok(old) = fs::read_to_string(&path) {
            if let Some(new) = f(&old) {
                if fs::write(&path, new).is_ok() {
                    done.push(path);
                }
            }
        }
    }
    done
}

pub fn migrate_launchagents_in(dir: &Path) -> Vec<PathBuf> {
    migrate_dir(dir, "io.beebeeb.sync.", ".plist", migrate_plist_content)
}

pub fn migrate_systemd_units_in(dir: &Path) -> Vec<PathBuf> {
    migrate_dir(dir, "beebeeb-sync-", ".service", migrate_unit_content)
}

/// Upgrade service definitions installed by an older `bb` to the current
/// restart policy, so an existing install stops its restart loop without the
/// user re-running `bb sync --daemon`. Best effort; called at `bb sync` and
/// `bb login` start. systemd: `daemon-reload` keeps the running unit alive and
/// applies the new policy at its next exit (the dead-session exit 77 then
/// stays stopped). launchd: the file is rewritten for the next load; a loaded
/// old job is taken out of launchd by [`disarm_launchagent_if_managed`] on the
/// same exit 77 (we never unload our own running job here).
pub fn migrate_sync_services() {
    let Some(home) = dirs::home_dir() else { return };
    if !migrate_launchagents_in(&home.join("Library/LaunchAgents")).is_empty() {
        eprintln!("Updated the background sync service to stop restarting after sign-out.");
    }
    if !migrate_systemd_units_in(&home.join(".config/systemd/user")).is_empty() {
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        eprintln!("Updated the background sync service to stop restarting after sign-out.");
    }
}

/// Unload and remove a macOS LaunchAgent plist for the given slug.
#[cfg(target_os = "macos")]
pub fn uninstall_launchagent(slug: &str) -> Result<bool, String> {
    let label = format!("io.beebeeb.sync.{slug}");
    let plist_path = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));

    if !plist_path.exists() {
        // Also try the legacy single-plist name
        let legacy_path = dirs::home_dir()
            .ok_or("cannot find home directory")?
            .join("Library/LaunchAgents/io.beebeeb.sync.plist");
        if legacy_path.exists() {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &legacy_path.to_string_lossy()])
                .status();
            let _ = fs::remove_file(&legacy_path);
            return Ok(true);
        }
        return Ok(false);
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();

    fs::remove_file(&plist_path).map_err(|e| format!("remove plist: {e}"))?;
    Ok(true)
}

/// No-op on non-macOS for LaunchAgent install.
#[cfg(not(target_os = "macos"))]
pub fn install_launchagent(_local_dir: &Path, _remote_path: &str, _slug: &str) -> Result<(), String> {
    Ok(())
}

/// No-op on non-macOS for LaunchAgent uninstall.
#[cfg(not(target_os = "macos"))]
pub fn uninstall_launchagent(_slug: &str) -> Result<bool, String> {
    Ok(false)
}

/// Install a systemd user service unit for the given sync session.
#[cfg(target_os = "linux")]
pub fn install_systemd_unit(local_dir: &Path, remote_path: &str, slug: &str) -> Result<(), String> {
    let unit_dir = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).map_err(|e| format!("create systemd user dir: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let unit_name = format!("beebeeb-sync-{slug}.service");
    let unit_path = unit_dir.join(&unit_name);

    let unit_content = render_systemd_unit(&slug, &exe.to_string_lossy(), &local_dir.to_string_lossy(), remote_path);

    fs::write(&unit_path, &unit_content).map_err(|e| format!("write systemd unit: {e}"))?;

    // Reload and enable
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", &unit_name])
        .status()
        .map_err(|e| format!("systemctl enable: {e}"))?;

    if !status.success() {
        return Err(format!("systemctl enable --now {unit_name} failed"));
    }

    Ok(())
}

/// Stop and remove a systemd user service unit for the given slug.
#[cfg(target_os = "linux")]
pub fn uninstall_systemd_unit(slug: &str) -> Result<bool, String> {
    let unit_name = format!("beebeeb-sync-{slug}.service");
    let unit_path = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join(".config/systemd/user")
        .join(&unit_name);

    if !unit_path.exists() {
        return Ok(false);
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", &unit_name])
        .status();

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", &unit_name])
        .status();

    fs::remove_file(&unit_path).map_err(|e| format!("remove systemd unit: {e}"))?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    Ok(true)
}

/// No-op on non-Linux for systemd install.
#[cfg(not(target_os = "linux"))]
pub fn install_systemd_unit(_local_dir: &Path, _remote_path: &str, _slug: &str) -> Result<(), String> {
    Ok(())
}

/// No-op on non-Linux for systemd uninstall.
#[cfg(not(target_os = "linux"))]
pub fn uninstall_systemd_unit(_slug: &str) -> Result<bool, String> {
    Ok(false)
}

/// Stop a daemon by slug: kill the process, remove PID file, and
/// uninstall platform auto-start (LaunchAgent / systemd unit).
pub fn stop_daemon(slug: &str) -> Result<bool, String> {
    let killed = kill_daemon(slug)?;
    let la_removed = uninstall_launchagent(slug)?;
    let sd_removed = uninstall_systemd_unit(slug)?;
    Ok(killed || la_removed || sd_removed)
}

/// Stop all daemons: iterate all PID files and stop each one.
pub fn stop_all_daemons() -> Result<u32, String> {
    let slugs = list_daemon_slugs()?;
    let mut count = 0u32;
    for slug in &slugs {
        if stop_daemon(slug)? {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod restart_policy_tests {
    use super::*;

    #[test]
    fn plist_does_not_blindly_keep_alive_and_throttles() {
        let p = render_plist("io.beebeeb.sync.x", "<string>a</string>", "/o", "/e");
        assert!(!p.contains("<key>KeepAlive</key>\n    <true/>"), "{p}");
        assert!(p.contains("<key>SuccessfulExit</key>"), "{p}");
        assert!(p.contains("<key>ThrottleInterval</key>"), "{p}");
    }

    #[test]
    fn systemd_unit_does_not_restart_on_session_ended() {
        let u = render_systemd_unit("x", "/bin/bb", "/l", "/r");
        assert!(u.contains("RestartPreventExitStatus=77"), "{u}");
        assert!(u.contains("RestartSec=30"), "{u}");
    }

    #[test]
    fn only_our_labels_are_managed() {
        assert_eq!(
            managed_launchd_label(Some("io.beebeeb.sync.a")).as_deref(),
            Some("io.beebeeb.sync.a")
        );
        assert!(managed_launchd_label(Some("com.other")).is_none());
        assert!(managed_launchd_label(None).is_none());
    }

    const OLD_PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n    <key>Label</key>\n    <string>io.beebeeb.sync.docs</string>\n    <key>ProgramArguments</key>\n    <array>\n        <string>/usr/local/bin/bb</string>\n        <string>sync</string>\n        <string>/Users/a &amp; b/Docs</string>\n        <string>/docs</string>\n    </array>\n    <key>RunAtLoad</key>\n    <true/>\n    <key>KeepAlive</key>\n    <true/>\n    <key>StandardOutPath</key>\n    <string>/l/docs.log</string>\n    <key>StandardErrorPath</key>\n    <string>/l/docs.err</string>\n</dict>\n</plist>";
    const OLD_UNIT: &str = "[Unit]\nDescription=Beebeeb Sync - docs\n\n[Service]\nType=simple\nExecStart=/bin/bb sync /home/a/Docs /docs\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n";

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bb-1872-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn old_plist_is_rewritten_preserving_args_idempotently_and_foreign_untouched() {
        let d = scratch("plist");
        let ours = d.join("io.beebeeb.sync.docs.plist");
        let foreign = d.join("com.other.plist");
        let foreign_body = "<key>Label</key><string>com.other</string><key>KeepAlive</key><true/>";
        fs::write(&ours, OLD_PLIST).unwrap();
        fs::write(&foreign, foreign_body).unwrap();
        // a file with our name but someone else's label is also left alone
        let imposter = d.join("io.beebeeb.sync.x.plist");
        fs::write(&imposter, foreign_body).unwrap();

        assert_eq!(migrate_launchagents_in(&d), vec![ours.clone()]);
        let new = fs::read_to_string(&ours).unwrap();
        assert!(new.contains("<key>SuccessfulExit</key>"), "{new}");
        assert!(new.contains("<key>ThrottleInterval</key>"), "{new}");
        assert!(!new.contains("&amp;amp;"), "double escaped: {new}");
        assert!(new.contains("<string>/Users/a &amp; b/Docs</string>"), "{new}");
        assert!(
            new.contains("<string>/docs</string>") && new.contains("/l/docs.err"),
            "{new}"
        );
        assert_eq!(fs::read_to_string(&foreign).unwrap(), foreign_body);
        assert_eq!(fs::read_to_string(&imposter).unwrap(), foreign_body);
        assert!(migrate_launchagents_in(&d).is_empty(), "second run must be a no-op");
        assert_eq!(fs::read_to_string(&ours).unwrap(), new);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn old_systemd_unit_is_rewritten_idempotently_and_foreign_untouched() {
        let d = scratch("unit");
        let ours = d.join("beebeeb-sync-docs.service");
        let foreign = d.join("other.service");
        let foreign_body = "[Service]\nExecStart=/bin/x\nRestart=always\n";
        fs::write(&ours, OLD_UNIT).unwrap();
        fs::write(&foreign, foreign_body).unwrap();

        assert_eq!(migrate_systemd_units_in(&d), vec![ours.clone()]);
        let new = fs::read_to_string(&ours).unwrap();
        assert!(new.contains("RestartPreventExitStatus=77"), "{new}");
        assert!(
            new.contains("RestartSec=30") && !new.contains("RestartSec=5\n"),
            "{new}"
        );
        assert!(new.contains("ExecStart=/bin/bb sync /home/a/Docs /docs"), "{new}");
        assert_eq!(new.matches("Restart=on-failure").count(), 1, "{new}");
        assert_eq!(fs::read_to_string(&foreign).unwrap(), foreign_body);
        assert!(migrate_systemd_units_in(&d).is_empty());
        assert_eq!(fs::read_to_string(&ours).unwrap(), new);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn current_templates_are_not_migrated() {
        let p = render_plist("io.beebeeb.sync.x", "        <string>a</string>", "/o", "/e");
        assert!(migrate_plist_content(&p).is_none());
        assert!(migrate_unit_content(&render_systemd_unit("x", "/b", "/l", "/r")).is_none());
    }

    #[test]
    fn service_detection() {
        assert!(running_as_service(Some("io.beebeeb.sync.a"), None));
        assert!(running_as_service(None, Some("abc123")));
        assert!(!running_as_service(Some("com.other"), None));
        assert!(!running_as_service(None, Some("")));
        assert!(!running_as_service(None, None));
    }
}
