//! Daemon process management for `bb sync --daemon`.
//!
//! Handles PID file lifecycle, process spawning, and platform-specific
//! auto-start integration (LaunchAgent on macOS, systemd user units on Linux).
//!
//! Owns: the facade over `paths` (PID files, liveness, kill), `home` (the
//! real-home guard the service managers must pass), `spawn` (the child
//! command line), `launchd`/`systemd` (the two service back-ends), `migrate`
//! (rewriting units installed by older releases) and `stop`.
//! Must not: hold any item of its own other than these re-exports.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
/// The config directory the process uses must be the one the installed service would use. On Linux
/// `dirs` reads it from `$XDG_CONFIG_HOME` (when absolute) before `$HOME/.config`, but a systemd user
/// service gets neither: it reads `<account home>/.config`. So an `XDG_CONFIG_HOME` that points
/// elsewhere with the real HOME would pass the HOME check and still install a unit that reads the
/// real config (and a live session). (macOS ignores XDG; its config lives under HOME.)
#[cfg(any(target_os = "linux", test))]
fn check_config_dir(config: Option<&Path>, account: Option<&Path>, allow: Option<&str>) -> Result<(), String> {
    if allow == Some("1") {
        return Ok(());
    }
    let (Some(config), Some(account)) = (config, account) else {
        return Ok(());
    };
    let expected = account.join(".config");
    if same_dir(config, &expected) {
        Ok(())
    } else {
        Err(format!(
            "refusing to touch the background-service manager: this run reads its config from {} (is \
             XDG_CONFIG_HOME set?) but the service would read {}. If this is intended, run the command \
             again with {CUSTOM_HOME_OVERRIDE}=1",
            config.display(),
            expected.display()
        ))
    }
}

/// Like [`render_plist`], with the full inner XML of the `EnvironmentVariables`
/// dict (migration passes the user's own entries plus the marker).
fn render_plist_with_env(label: &str, args_xml: &str, stdout_log: &str, stderr_log: &str, env_xml: &str) -> String {
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
    <key>EnvironmentVariables</key>
    <dict>
{env_xml}
    </dict>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#,
        out = xml_escape(stdout_log),
        err = xml_escape(stderr_log),
    )
}

/// The command that starts the detached child, not yet started: its arguments, and the transport
/// this invocation selected said explicitly in its environment, so `--transport auto` beats an
/// inherited `BB_TRANSPORT=api` just as `--transport api` beats an inherited `auto`.
fn daemon_command(exe: &Path, local_dir: &Path, remote_path: &str, api_forced: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(daemon_args(local_dir, remote_path, api_forced))
        .env("BB_TRANSPORT", if api_forced { "api" } else { "auto" });
    cmd
}

mod paths;
pub use paths::{daemon_dir, is_daemon_running, kill_daemon, list_daemon_slugs};
pub use paths::{log_dir, pid_path, read_pid, remove_pid, slugify, write_pid};

mod home;
pub use home::CUSTOM_HOME_OVERRIDE;
use home::*;

mod spawn;
pub use spawn::spawn_daemon;
use spawn::*;

// ── Platform-specific auto-start integration ──────────────────────────────

mod launchd;
use launchd::*;
pub use launchd::{disarm_launchagent_if_managed, install_launchagent, managed_launchd_label};
pub use launchd::{render_plist, running_as_service, uninstall_launchagent};

mod systemd;
pub use systemd::uninstall_systemd_unit;
pub use systemd::{install_systemd_unit, render_systemd_unit, render_systemd_unit_with};

mod migrate;
pub use migrate::{migrate_launchagents_in, migrate_plist_content, migrate_sync_services};
pub use migrate::{migrate_systemd_units_in, migrate_unit_content};

mod stop;
pub use stop::{stop_all_daemons, stop_daemon};
