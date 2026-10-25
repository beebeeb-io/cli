//! No test, and no `bb` run under a redirected HOME, may ever load, unload or edit a launchd job or a
//! systemd unit (task 2173 incident).
//!
//! launchd and `systemd --user` run a job as the REAL user with the account's REAL home. A test that
//! ran `bb sync --daemon` under a scratch HOME loaded a launchd job that ran against production with
//! the developer's live session. Since then every `launchctl` / `systemctl` call goes through
//! `daemon::service_manager`, which refuses unless HOME is the account's home.
//!
//! The end-to-end test below puts RECORDING STUBS in front of the real tools on PATH, so even if the
//! guard were broken nothing real would be touched: the stubs are the only `launchctl` / `systemctl` it
//! can find.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("service-guard-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `#!/bin/sh` stubs for `launchctl` and `systemctl` that append their command line to `log`.
fn stub_tools(dir: &Path, log: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("stub-bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["launchctl", "systemctl"] {
        let path = bin.join(tool);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"{tool} $@\" >> '{}'\nexit 0\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// A scratch HOME with a config that points at a closed port: the run under test must not need the API.
fn scratch_home(dir: &Path) -> PathBuf {
    let home = dir.join("home");
    let config = r#"{"api_url":"http://127.0.0.1:9","session_token":"scratch","email":"guard@beebeeb.io","master_key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#;
    for d in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), config).unwrap();
    }
    home
}

/// An API that answers every request `200 {}`: the run under test only needs `bb sync` to get past
/// its connectivity check to the code that would touch the service manager.
async fn any_api() -> String {
    let app = axum::Router::new().fallback(|| async { axum::Json(serde_json::json!({})) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_scratch_home_never_reaches_launchctl_or_systemctl() {
    let api = any_api().await;
    let dir = scratch("stub");
    let log = dir.join("calls.log");
    let bin = stub_tools(&dir, &log);
    let home = scratch_home(&dir);

    // The instrument first: the stub records, and it is what PATH finds first.
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let probe = Command::new("sh")
        .args(["-c", "launchctl probe; systemctl probe"])
        .env("PATH", &path)
        .status()
        .unwrap();
    assert!(probe.success());
    let recorded = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        recorded.lines().count(),
        2,
        "the stubs must record, or this test proves nothing: {recorded:?}"
    );
    std::fs::write(&log, "").unwrap();

    // An older install's plist and a unit that `bb sync --stop` would unload and delete.
    let agents = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    let plist = agents.join("io.beebeeb.sync.plist");
    std::fs::write(&plist, "<plist/>").unwrap();

    let (run_dir, run_home, run_path) = (dir.clone(), home.clone(), path.clone());
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            // `--stop` with a folder is the legacy "remove the old LaunchAgent" path: it unloads and deletes
            // `io.beebeeb.sync.plist`, which is exactly what must never happen from a scratch HOME.
            .args(["--api", &api, "sync", "--stop", run_dir.to_str().unwrap()])
            .current_dir(&run_dir)
            .env("HOME", &run_home)
            .env("XDG_CONFIG_HOME", run_home.join(".config"))
            .env("PATH", &run_path)
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let all = format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.is_empty(),
        "a run under a scratch HOME talked to a service manager: {calls:?}\n{all}"
    );
    assert!(plist.exists(), "the plist under the scratch HOME was removed\n{all}");
    if cfg!(target_os = "macos") {
        assert!(
            all.contains("refusing to touch the background-service manager"),
            "{all}"
        );
        assert!(
            all.contains("BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME=1"),
            "the refusal says how to proceed: {all}"
        );
        assert!(!out.status.success(), "{all}");
    }
}

/// An API that lists one sync session named `zzguard` and answers everything else `200 {}`.
async fn api_with_a_session() -> String {
    let app = axum::Router::new()
        .route(
            "/api/v1/clients/sessions",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({ "sessions": [{ "id": "s1", "name": "zzguard" }] }))
            }),
        )
        .fallback(|| async { axum::Json(serde_json::json!({})) });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// `bb sync --stop-session zzguard` stops a session's local service: it unloads the LaunchAgent (macOS)
/// or stops, disables and deletes the systemd unit (Linux). Under a scratch HOME it must do none of
/// it, on either platform: the stubs record nothing and the service files are untouched. (The Linux
/// half is what CI runs; the macOS half is `a_scratch_home_never_reaches_launchctl_or_systemctl`.)
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_session_never_unloads_or_deletes_a_service_from_a_scratch_home() {
    let api = api_with_a_session().await;
    let dir = scratch("session");
    let log = dir.join("calls.log");
    let bin = stub_tools(&dir, &log);
    std::fs::write(&log, "").unwrap();
    let home = scratch_home(&dir);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    let plist = home.join("Library/LaunchAgents/io.beebeeb.sync.zzguard.plist");
    let unit = home.join(".config/systemd/user/beebeeb-sync-zzguard.service");
    for f in [&plist, &unit] {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, "service definition").unwrap();
    }
    let (run_dir, run_home, run_path) = (dir.clone(), home.clone(), path);
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(["--api", &api, "sync", "--stop-session", "zzguard"])
            .current_dir(&run_dir)
            .env("HOME", &run_home)
            .env("XDG_CONFIG_HOME", run_home.join(".config"))
            .env("XDG_DATA_HOME", run_home.join(".local/share"))
            .env("XDG_STATE_HOME", run_home.join(".local/state"))
            .env("PATH", &run_path)
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let all = format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "stopping the session itself works: {all}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.is_empty(),
        "a service manager was started under a scratch HOME: {calls:?}\n{all}"
    );
    let (plist_kept, unit_kept) = (plist.exists(), unit.exists());
    if cfg!(target_os = "macos") {
        assert!(plist_kept, "the plist was deleted\n{all}");
    } else {
        assert!(unit_kept, "the systemd unit was deleted\n{all}");
    }
}

/// With nothing installed there is nothing to refuse: removing a service that does not exist is
/// `Ok(false)`, not an error, so a loop over sessions does not stop at the first slug without a service.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_service_that_is_not_installed_is_not_a_refusal() {
    let api = any_api().await;
    let dir = scratch("none");
    let log = dir.join("calls.log");
    let bin = stub_tools(&dir, &log);
    std::fs::write(&log, "").unwrap();
    let home = scratch_home(&dir);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let (run_dir, run_home) = (dir.clone(), home.clone());
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(["--api", &api, "sync", "--stop", run_dir.to_str().unwrap()])
            .current_dir(&run_dir)
            .env("HOME", &run_home)
            .env("XDG_CONFIG_HOME", run_home.join(".config"))
            .env("XDG_DATA_HOME", run_home.join(".local/share"))
            .env("PATH", &path)
            .env("BB_NO_UPDATE", "1")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let all = format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{all}");
    assert!(!all.contains("refusing"), "{all}");
    assert!(std::fs::read_to_string(&log).unwrap().is_empty(), "{all}");
}

/// Every `launchctl` / `systemctl` the code names is behind the guard. A line that names either tool is
/// accepted only if it (a) calls `service_manager(`, the one door that checks HOME before it starts
/// anything, (b) is message text (an error, a print), (c) lives in `tests/service_guard.rs`, whose
/// stubs are the point, or (d) is inside `disarm_launchagent_if_managed`: a job removing ITSELF, which
/// only runs when launchd started the process. That exception is tied to the function by name and may
/// name the tool exactly once. Scans `src/`, `tests/` and `build.rs`, so a multi-line `Command::new(`,
/// a constant or a `sh -c` string is caught too.
#[test]
fn no_source_file_names_a_service_manager_except_through_the_guard() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    walk(&root.join("tests"), &mut files);
    if root.join("build.rs").exists() {
        files.push(root.join("build.rs"));
    }
    let mut violations = Vec::new();
    let mut disarm_lines = 0;
    for path in &files {
        let name = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // The range of `disarm_launchagent_if_managed`: from its `fn` line to the first line that is just `}`.
        let disarm = lines
            .iter()
            .position(|l| l.contains("fn disarm_launchagent_if_managed"))
            .map(|start| (start, start + lines[start..].iter().position(|l| *l == "}").unwrap()));
        for (n, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            let names_a_tool = code.contains("launchctl") || code.contains("systemctl");
            if !names_a_tool || code.starts_with("//") || code.starts_with('*') || name == "tests/service_guard.rs" {
                continue;
            }
            let is_message = ["Err(", "format!(", "println!(", "eprintln!(", "panic!(", "assert"]
                .iter()
                .any(|t| code.contains(t));
            let in_disarm = name == "src/daemon.rs" && disarm.is_some_and(|(a, b)| (a..=b).contains(&n));
            if in_disarm {
                disarm_lines += 1;
            }
            if !(code.contains("service_manager(") || is_message || in_disarm) {
                violations.push(format!("{name}:{}: {}", n + 1, code));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "unguarded service-manager names: {violations:#?}"
    );
    assert_eq!(disarm_lines, 1, "the one exception must name the tool exactly once");

    // And the door itself: `service_manager` checks HOME before it starts the program it is given.
    let daemon = std::fs::read_to_string(root.join("src/daemon.rs")).unwrap();
    let door = daemon
        .split("fn service_manager")
        .nth(1)
        .expect("service_manager exists");
    let door = &door[..door.find("\n}\n").expect("end of service_manager")];
    assert!(
        door.find("ensure_real_home()?").unwrap() < door.find("Command::new(program)").unwrap(),
        "the HOME check must come before the program starts:\n{door}"
    );
}
