//! Where the daemon keeps its PID file and log, and how a second invocation
//! decides whether one is already alive.
//!
//! Owns: `daemon_dir`/`log_dir`/`slugify`, the PID file lifecycle, process
//! liveness, and the `kill_daemon` signal path.
//! Must not: spawn the child (see `daemon::spawn`) or install a service.
//! Tests: liveness and the kill path are not unit-tested here; the daemon
//! end-to-end behaviour is covered in `tests/`.

use super::*;

/// Directory for PID files: `~/.local/share/beebeeb/daemons/`
pub fn daemon_dir() -> Result<PathBuf, DaemonError> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or("cannot determine home directory")?;
    let dir = base.join("beebeeb").join("daemons");
    fs::create_dir_all(&dir).map_err(|e| DaemonError::Other(format!("create daemon dir: {e}")))?;
    Ok(dir)
}

/// Directory for daemon log files: `~/.local/share/beebeeb/logs/`
pub fn log_dir() -> Result<PathBuf, DaemonError> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or("cannot determine home directory")?;
    let dir = base.join("beebeeb").join("logs");
    fs::create_dir_all(&dir).map_err(|e| DaemonError::Other(format!("create log dir: {e}")))?;
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
pub fn pid_path(slug: &str) -> Result<PathBuf, DaemonError> {
    Ok(daemon_dir()?.join(format!("{slug}.pid")))
}

/// Write a PID to the PID file for the given slug.
pub fn write_pid(slug: &str, pid: u32) -> Result<(), DaemonError> {
    let path = pid_path(slug)?;
    fs::write(&path, pid.to_string()).map_err(|e| DaemonError::Other(format!("write PID file: {e}")))
}

/// Read the PID from a PID file, if it exists.
pub fn read_pid(slug: &str) -> Result<Option<u32>, DaemonError> {
    let path = pid_path(slug)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).map_err(|e| DaemonError::Other(format!("read PID file: {e}")))?;
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
pub fn remove_pid(slug: &str) -> Result<(), DaemonError> {
    let path = pid_path(slug)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|e| DaemonError::Other(format!("remove PID file: {e}")))?;
    }
    Ok(())
}

/// Check whether a process with the given PID is still running.
pub(super) fn process_exists(pid: u32) -> bool {
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
pub fn is_daemon_running(slug: &str) -> Result<Option<u32>, DaemonError> {
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
pub fn list_daemon_slugs() -> Result<Vec<String>, DaemonError> {
    let dir = daemon_dir()?;
    let mut slugs = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| DaemonError::Other(format!("read daemon dir: {e}")))?;
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
pub fn kill_daemon(slug: &str) -> Result<bool, DaemonError> {
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
