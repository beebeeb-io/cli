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

    let args = vec![
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

    let plist = format!(
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
    <true/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>"#,
        xml_escape(&stdout_log.to_string_lossy()),
        xml_escape(&stderr_log.to_string_lossy()),
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

    let unit_content = format!(
        r#"[Unit]
Description=Beebeeb Sync - {slug}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exe} sync {local} {remote}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
"#,
        slug = slug,
        exe = exe.to_string_lossy(),
        local = local_dir.to_string_lossy(),
        remote = remote_path,
    );

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
