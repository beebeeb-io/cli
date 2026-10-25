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
        assert!(!out.status.success(), "{all}");
    }
}

/// Every `launchctl` / `systemctl` the crate starts goes through `service_manager`. The one exception
/// is a job removing ITSELF (`disarm_launchagent_if_managed`, which only runs when launchd started it).
#[test]
fn no_source_file_starts_a_service_manager_except_through_the_guard() {
    fn walk(dir: &Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                for (n, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                    let starts_a_process = line.contains("Command::new(");
                    if starts_a_process && (line.contains("launchctl") || line.contains("systemctl")) {
                        hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut hits);
    assert_eq!(hits.len(), 1, "unguarded service-manager calls: {hits:#?}");
    assert!(
        hits[0].contains("src/daemon.rs") && hits[0].contains("launchctl"),
        "{hits:?}"
    );
    // And the guard itself is the only door: `service_manager` starts the program it is given.
    let daemon = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon.rs")).unwrap();
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
