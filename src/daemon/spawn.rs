//! Starting the detached sync child, and the one place that talks to
//! `launchctl`/`systemctl`.
//!
//! Owns: `service_manager` (every manager call goes through it), `daemon_args`
//! and `daemon_command` (the child's argv/env, including the forced transport),
//! `spawn_daemon`.
//! Must not: install or remove a service definition (see `daemon::launchd`
//! and `daemon::systemd`).
//! Tests: `transport_tests` at the bottom of this file (argv, env, and the
//! real-home guard cases).

use super::*;

/// Run `program args…` (`launchctl` or `systemctl`) and wait for it, after checking HOME. The only
/// way this crate starts a service manager (apart from a job removing itself in
/// [`disarm_launchagent_if_managed`]).
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
pub(super) fn service_manager(program: &str, args: &[&str]) -> Result<std::process::ExitStatus, DaemonError> {
    ensure_real_home()?;
    std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|e| DaemonError::Other(format!("{program}: {e}")))
}

/// The arguments of the detached child: the sync it runs, preceded by the transport this
/// invocation forced. `BB_TRANSPORT` reaches the child through the environment; the flag does not,
/// so it is passed on here.
pub(super) fn daemon_args(local_dir: &Path, remote_path: &str, mode: DaemonMode) -> Vec<String> {
    let mut args = Vec::new();
    if mode.is_api_forced() {
        args.extend(["--transport".to_string(), "api".to_string()]);
    }
    args.extend([
        "sync".to_string(),
        local_dir.to_string_lossy().into_owned(),
        remote_path.to_string(),
    ]);
    args
}

/// Spawn `bb sync <local> <remote>` as a detached daemon process.
///
/// Stdout/stderr are redirected to log files. Returns the child PID.
pub fn spawn_daemon(local_dir: &Path, remote_path: &str, slug: &str) -> Result<u32, DaemonError> {
    // Check if already running
    if let Some(pid) = is_daemon_running(slug)? {
        return Err(DaemonError::Other(format!(
            "daemon already running for this session (PID {pid}). \
             Use bb sync --stop-session to stop it first."
        )));
    }

    let exe = std::env::current_exe().map_err(|e| DaemonError::Other(format!("current_exe: {e}")))?;
    let logs = log_dir()?;

    let stdout_path = logs.join(format!("{slug}.log"));
    let stderr_path = logs.join(format!("{slug}.err"));

    let stdout_file =
        fs::File::create(&stdout_path).map_err(|e| DaemonError::Other(format!("create log file: {e}")))?;
    let stderr_file =
        fs::File::create(&stderr_path).map_err(|e| DaemonError::Other(format!("create error log file: {e}")))?;

    let child = daemon_command(
        &exe,
        local_dir,
        remote_path,
        DaemonMode::from(crate::transport::api_forced()),
    )
    .stdout(stdout_file)
    .stderr(stderr_file)
    // Detach from the parent process group
    .stdin(std::process::Stdio::null())
    .spawn()
    .map_err(|e| DaemonError::Other(format!("spawn daemon: {e}")))?;

    let pid = child.id();
    write_pid(slug, pid)?;

    Ok(pid)
}
#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn the_daemon_child_inherits_a_forced_api_transport() {
        let dir = Path::new("/tmp/photos");
        assert_eq!(
            daemon_args(dir, "/photos", DaemonMode::Default),
            ["sync", "/tmp/photos", "/photos"]
        );
        assert_eq!(
            daemon_args(dir, "/photos", DaemonMode::ApiForced),
            ["--transport", "api", "sync", "/tmp/photos", "/photos"]
        );
    }

    #[test]
    fn the_daemon_child_is_told_its_transport_in_words_not_by_inheritance() {
        let exe = Path::new("/usr/local/bin/bb");
        let dir = Path::new("/tmp/photos");
        for (mode, want) in [(DaemonMode::ApiForced, "api"), (DaemonMode::Default, "auto")] {
            let cmd = daemon_command(exe, dir, "/photos", mode);
            let env: Vec<_> = cmd
                .get_envs()
                .filter(|(k, _)| *k == "BB_TRANSPORT")
                .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
                .collect();
            assert_eq!(env, vec![Some(want.to_string())], "mode={mode:?}");
            let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert_eq!(args.last().map(String::as_str), Some("/photos"));
            assert_eq!(args.contains(&"--transport".to_string()), mode.is_api_forced());
        }
    }

    #[test]
    fn a_service_definition_carries_the_transport_it_was_installed_with() {
        let plist = env_xml_with_transport("api");
        assert!(plist.contains("BEEBEEB_SYNC_SERVICE"), "the marker stays: {plist}");
        assert!(
            plist.contains("<key>BB_TRANSPORT</key>\n        <string>api</string>"),
            "{plist}"
        );
        assert!(env_xml_with_transport("auto").contains("<string>auto</string>"));
        let unit = render_systemd_unit_with("s", "/bin/bb", "/tmp/a", "/r", Some("api"));
        assert!(unit.contains("Environment=BEEBEEB_SYNC_SERVICE=1\nEnvironment=BB_TRANSPORT=api\nRestart=on-failure"));
        let plain = render_systemd_unit("s", "/bin/bb", "/tmp/a", "/r");
        assert!(!plain.contains("BB_TRANSPORT") && plain.contains("Restart=on-failure"));
    }

    // ── the service managers are only spoken to from the account's own home ──

    #[test]
    fn the_service_managers_are_only_spoken_to_from_the_accounts_home() {
        let account = Path::new("/Users/dev");
        let scratch = Path::new("/tmp/scratch/home");
        assert!(check_home(Some(account), Some(account), None).is_ok());
        let refused = check_home(Some(scratch), Some(account), None).unwrap_err();
        assert!(
            refused.contains("refusing") && refused.contains("/tmp/scratch/home") && refused.contains("/Users/dev"),
            "{refused}"
        );
        // The refusal says how to proceed when the HOME is intended.
        assert!(refused.contains("BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME=1"), "{refused}");
        // When either side is unknown there is nothing to compare: a real user is not locked out.
        assert!(check_home(None, Some(account), None).is_ok());
        assert!(check_home(None, None, None).is_ok());
        // An unknown ACCOUNT home fails closed: not knowing is not consent. The override still works.
        let unknown = check_home(Some(account), None, None).unwrap_err();
        assert!(
            unknown.contains("cannot be determined") && unknown.contains("=1"),
            "{unknown}"
        );
        assert!(check_home(Some(account), None, Some("1")).is_ok());
    }

    #[test]
    fn a_config_dir_that_is_not_the_services_is_refused() {
        // XDG_CONFIG_HOME pointing elsewhere with the real HOME: the HOME check passes, this must not.
        let account = Path::new("/home/dev");
        assert!(check_config_dir(Some(Path::new("/home/dev/.config")), Some(account), None).is_ok());
        let refused = check_config_dir(Some(Path::new("/tmp/scratch/xdg")), Some(account), None).unwrap_err();
        assert!(
            refused.contains("XDG_CONFIG_HOME")
                && refused.contains("/tmp/scratch/xdg")
                && refused.contains("/home/dev/.config"),
            "{refused}"
        );
        assert!(refused.contains("BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME=1"), "{refused}");
        assert!(check_config_dir(Some(Path::new("/tmp/scratch/xdg")), Some(account), Some("1")).is_ok());
        assert!(check_config_dir(Some(Path::new("/tmp/scratch/xdg")), Some(account), Some("0")).is_err());
        // Nothing to compare when either side is unknown (the HOME check has its own rule for that).
        assert!(check_config_dir(None, Some(account), None).is_ok());
        assert!(check_config_dir(Some(Path::new("/x")), None, None).is_ok());
    }

    #[test]
    fn the_override_lets_an_intended_custom_home_through_and_only_the_value_one() {
        let account = Path::new("/Users/dev");
        let custom = Path::new("/srv/custom-home");
        assert!(check_home(Some(custom), Some(account), Some("1")).is_ok());
        for not_consent in [None, Some(""), Some("0"), Some("true"), Some("yes"), Some("11")] {
            assert!(
                check_home(Some(custom), Some(account), not_consent).is_err(),
                "{not_consent:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_home_is_the_same_home() {
        let dir = std::env::temp_dir().join(format!("bb-home-guard-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(check_home(Some(&link), Some(&real), None).is_ok());
        assert!(
            check_home(Some(&dir), Some(&real), None).is_err(),
            "a parent directory is not the home"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_accounts_home_comes_from_the_passwd_database() {
        let home = account_home().expect("this account has a passwd entry");
        assert!(home.is_absolute(), "{home:?}");
    }
}
