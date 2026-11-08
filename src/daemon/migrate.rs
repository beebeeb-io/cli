//! Rewriting service definitions installed by older releases: add the
//! `BEEBEEB_SYNC_SERVICE` marker and the sign-out-safe restart policy once,
//! without touching a user's own environment lines or a foreign plist/unit.
//!
//! Owns: the plist/unit content migrations, the directory walkers
//! (`migrate_dir`, `migrate_launchagents_in`, `migrate_systemd_units_in`) and
//! `migrate_sync_services`, plus the small XML/text helpers they need.
//! Must not: migrate a file whose label/exec path is not ours.
//! Tests: `restart_policy_tests` at the bottom of this file.

use super::*;

pub(super) fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let a = s.find(start)? + start.len();
    let b = s[a..].find(end)? + a;
    Some(&s[a..b])
}

pub(super) fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The string value following `<key>{key}</key>` in a plist.
pub(super) fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    between(after, "<string>", "</string>").map(xml_unescape)
}

/// Rewrite a pre-1872 sync plist (blind `KeepAlive=true`, no throttle) to the
/// current template, keeping its label, arguments and log paths. `None` when
/// the content is not one of ours or already current.
pub fn migrate_plist_content(old: &str) -> Option<String> {
    let label = plist_string(old, "Label")?;
    if !label.starts_with("io.beebeeb.sync.")
        || (old.contains("<key>SuccessfulExit</key>") && old.contains("BEEBEEB_SYNC_SERVICE"))
    {
        return None;
    }
    let args_xml = between(old, "<array>\n", "\n    </array>")?;
    let out = plist_string(old, "StandardOutPath")?;
    let err = plist_string(old, "StandardErrorPath")?;
    Some(render_plist_with_env(
        &label,
        args_xml,
        &out,
        &err,
        &merged_env_xml(old),
    ))
}

/// Inner XML of the `EnvironmentVariables` dict for a migrated plist: every
/// entry the old plist had (user keys such as `HOME`, `BB_NO_UPDATE`, proxy
/// vars), with only `BEEBEEB_SYNC_SERVICE` added or set to `1`.
pub(super) fn merged_env_xml(old: &str) -> String {
    const MARKER_KEY: &str = "<key>BEEBEEB_SYNC_SERVICE</key>";
    let Some(k) = old.find("<key>EnvironmentVariables</key>") else {
        return MARKER_ENV_XML.to_string();
    };
    let after = &old[k..];
    let Some(inner) = between(after, "<dict>", "</dict>") else {
        return MARKER_ENV_XML.to_string();
    };
    let inner = inner.trim_matches(|c| c == '\n' || c == '\r').trim_end();
    if let Some(m) = inner.find(MARKER_KEY) {
        // Marker present with another value: overwrite just that value, and
        // only the value node that immediately follows the key.
        let tail = &inner[m + MARKER_KEY.len()..];
        let ws = tail.len() - tail.trim_start().len();
        let node = &tail[ws..];
        let value_len = if node.starts_with("<string>") {
            node.find("</string>").map(|e| e + "</string>".len())
        } else if node.starts_with("<string/>") {
            Some("<string/>".len())
        } else if node.starts_with("<string />") {
            Some("<string />".len())
        } else {
            None
        };
        if let Some(len) = value_len {
            let head = &inner[..m + MARKER_KEY.len() + ws];
            return format!("{head}<string>1</string>{}", &node[len..]);
        }
    }
    if inner.trim().is_empty() {
        MARKER_ENV_XML.to_string()
    } else {
        format!("{inner}\n{MARKER_ENV_XML}")
    }
}

/// Rewrite a pre-1872 sync systemd unit (`Restart=on-failure` with no
/// `RestartPreventExitStatus=77`, or no `BEEBEEB_SYNC_SERVICE` marker) in
/// place, keeping everything else.
pub fn migrate_unit_content(old: &str) -> Option<String> {
    let is_ours = old.contains("Description=Beebeeb Sync") && old.contains("ExecStart=");
    if !is_ours || (old.contains("RestartPreventExitStatus=77") && old.contains("Environment=BEEBEEB_SYNC_SERVICE=1")) {
        return None;
    }
    let mut out = String::new();
    for line in old.lines() {
        if line.starts_with("Restart=")
            || line.starts_with("RestartSec=")
            || line.starts_with("RestartPreventExitStatus=")
            || line.starts_with("Environment=BEEBEEB_SYNC_SERVICE=")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if line.starts_with("ExecStart=") {
            out.push_str(
                "Environment=BEEBEEB_SYNC_SERVICE=1\nRestart=on-failure\nRestartSec=30\nRestartPreventExitStatus=77\n",
            );
        }
    }
    Some(out)
}

/// Rewrite old-template files of ours in `dir` (`prefix*suffix`); returns the
/// rewritten paths. Touches nothing else. Idempotent.
pub(super) fn migrate_dir(dir: &Path, prefix: &str, suffix: &str, f: fn(&str) -> Option<String>) -> Vec<PathBuf> {
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
        let _ = service_manager("systemctl", &["--user", "daemon-reload"]);
        eprintln!("Updated the background sync service to stop restarting after sign-out.");
    }
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
    fn round2_units_and_plists_without_marker_get_it() {
        let cur_unit = render_systemd_unit("x", "/b", "/l", "/r");
        let r2_unit = cur_unit.replace("Environment=BEEBEEB_SYNC_SERVICE=1\n", "");
        assert!(!r2_unit.contains("BEEBEEB_SYNC_SERVICE"));
        let new = migrate_unit_content(&r2_unit).expect("must migrate");
        assert_eq!(new.matches("Environment=BEEBEEB_SYNC_SERVICE=1").count(), 1, "{new}");
        assert_eq!(new.matches("Restart=on-failure").count(), 1, "{new}");
        assert!(migrate_unit_content(&new).is_none());
        let cur_plist = render_plist("io.beebeeb.sync.x", "        <string>a</string>", "/o", "/e");
        assert!(cur_plist.contains("<key>BEEBEEB_SYNC_SERVICE</key>"), "{cur_plist}");
        let r2_plist = cur_plist.replace(
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>BEEBEEB_SYNC_SERVICE</key>\n        <string>1</string>\n    </dict>\n",
            "",
        );
        assert!(!r2_plist.contains("BEEBEEB_SYNC_SERVICE"));
        let np = migrate_plist_content(&r2_plist).expect("must migrate");
        assert!(np.contains("<key>BEEBEEB_SYNC_SERVICE</key>"), "{np}");
        assert!(migrate_plist_content(&np).is_none());
    }

    #[test]
    fn plist_migration_keeps_user_env_and_trailing_newline() {
        let old = OLD_PLIST.replace(
            "    <key>RunAtLoad</key>",
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>BB_NO_UPDATE</key>\n        <string>1</string>\n        <key>HOME</key>\n        <string>/Users/a &amp; b</string>\n    </dict>\n    <key>RunAtLoad</key>",
        );
        let new = migrate_plist_content(&old).expect("must migrate");
        assert!(
            new.contains("<key>BB_NO_UPDATE</key>\n        <string>1</string>"),
            "{new}"
        );
        assert!(
            new.contains("<key>HOME</key>\n        <string>/Users/a &amp; b</string>"),
            "{new}"
        );
        assert_eq!(new.matches("<key>BEEBEEB_SYNC_SERVICE</key>").count(), 1, "{new}");
        assert_eq!(new.matches("<key>EnvironmentVariables</key>").count(), 1, "{new}");
        assert!(new.ends_with("</plist>\n"), "{new:?}");
        assert!(migrate_plist_content(&new).is_none());
    }

    #[test]
    fn plist_migration_overwrites_a_wrong_marker_value_only() {
        let old = OLD_PLIST.replace(
            "    <key>RunAtLoad</key>",
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>BEEBEEB_SYNC_SERVICE</key>\n        <string>0</string>\n        <key>FOO</key>\n        <string>bar</string>\n    </dict>\n    <key>RunAtLoad</key>",
        );
        let new = migrate_plist_content(&old).expect("must migrate");
        assert!(
            new.contains("<key>BEEBEEB_SYNC_SERVICE</key>\n        <string>1</string>"),
            "{new}"
        );
        assert!(!new.contains("<string>0</string>"), "{new}");
        assert!(new.contains("<key>FOO</key>\n        <string>bar</string>"), "{new}");
        assert_eq!(new.matches("<key>BEEBEEB_SYNC_SERVICE</key>").count(), 1, "{new}");
    }

    #[test]
    fn plist_migration_empty_marker_value_does_not_eat_the_next_key() {
        for empty in ["<string/>", "<string></string>", "<string />"] {
            let old = OLD_PLIST.replace(
                "    <key>RunAtLoad</key>",
                &format!("    <key>EnvironmentVariables</key>\n    <dict>\n        <key>BEEBEEB_SYNC_SERVICE</key>\n        {empty}\n        <key>FOO</key>\n        <string>bar</string>\n    </dict>\n    <key>RunAtLoad</key>"),
            );
            let new = migrate_plist_content(&old).expect("must migrate");
            assert!(
                new.contains("<key>BEEBEEB_SYNC_SERVICE</key>\n        <string>1</string>"),
                "{empty}: {new}"
            );
            assert!(
                new.contains("<key>FOO</key>\n        <string>bar</string>"),
                "{empty}: {new}"
            );
            assert_eq!(new.matches("<key>BEEBEEB_SYNC_SERVICE</key>").count(), 1, "{new}");
        }
    }

    #[test]
    fn run_bounded_kills_and_reaps_a_slow_child() {
        let t = std::time::Instant::now();
        let mut slow = std::process::Command::new("sleep");
        slow.arg("30");
        assert!(!run_bounded(&mut slow, std::time::Duration::from_millis(300)));
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        let mut fast = std::process::Command::new("true");
        assert!(run_bounded(&mut fast, std::time::Duration::from_secs(5)));
    }

    #[test]
    fn systemd_migration_keeps_user_environment_lines_and_adds_marker_once() {
        let old = "[Unit]\nDescription=Beebeeb Sync - docs\n\n[Service]\nType=simple\nEnvironment=BB_NO_UPDATE=1\nExecStart=/bin/bb sync /a /b\nEnvironment=HTTPS_PROXY=http://p:3128\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n";
        let new = migrate_unit_content(old).expect("must migrate");
        assert!(new.contains("Environment=BB_NO_UPDATE=1\n"), "{new}");
        assert!(new.contains("Environment=HTTPS_PROXY=http://p:3128\n"), "{new}");
        assert_eq!(new.matches("Environment=BEEBEEB_SYNC_SERVICE=1").count(), 1, "{new}");
        assert!(new.ends_with('\n'), "{new:?}");
        assert!(migrate_unit_content(&new).is_none());
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
        assert!(running_as_service(None, Some("1")));
        assert!(!running_as_service(None, Some("abc123")));
        assert!(!running_as_service(Some("com.other"), None));
        assert!(!running_as_service(None, Some("")));
        assert!(!running_as_service(None, None));
    }
}
