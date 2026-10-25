//! The guard that keeps a test run off production (safety finding, 2026-10-25).
//!
//! The CLI's API URL comes from `--api`, the config file, or the built-in default `https://api.beebeeb.io`.
//! `BB_API_BASE` is NOT read by the CLI, so a test that runs the binary with a scratch HOME and names no
//! server points it at production. A harness therefore sets `BB_REFUSE_NON_LOOPBACK_API=1` on every run of
//! the binary, and `config::load_config` exits 2 for a URL that is not on this machine before anything is sent.
//!
//! These tests also point every proxy variable at a closed local port, so that even a regression of the guard
//! cannot put a request on the network: the worst it can do is fail to connect to 127.0.0.1:9.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch_home(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("api-guard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".config")).unwrap();
    dir
}

/// `bb <args>` in a scratch HOME with NO config file, behind a proxy that goes nowhere.
fn bb(home: &Path, args: &[&str], guard: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env_remove("BB_TRANSPORT")
        .stdin(Stdio::null());
    for var in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "https_proxy", "http_proxy", "all_proxy"] {
        cmd.env(var, "http://127.0.0.1:9");
    }
    cmd.env_remove("NO_PROXY").env_remove("no_proxy");
    match guard {
        Some(value) => {
            cmd.env("BB_REFUSE_NON_LOOPBACK_API", value);
        }
        None => {
            cmd.env_remove("BB_REFUSE_NON_LOOPBACK_API");
        }
    }
    cmd.output().expect("run bb")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_run_that_names_no_server_stops_before_calling_the_default() {
    let home = scratch_home("default");
    let out = bb(&home, &["ls"], Some("1"));
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("Refusing to contact api.beebeeb.io"), "{err}");
    assert!(err.contains("BB_REFUSE_NON_LOOPBACK_API"), "{err}");
    assert!(err.contains("--api"), "it says how to name a local server: {err}");
}

#[test]
fn a_run_that_names_another_public_server_is_refused_too() {
    let home = scratch_home("named");
    let out = bb(&home, &["--api", "https://api.example.org", "ls"], Some("1"));
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("Refusing to contact api.example.org"), "{}", stderr(&out));
}

#[test]
fn a_run_that_names_a_server_on_this_machine_is_not_refused() {
    let home = scratch_home("local");
    // Nothing listens on port 9: the run fails to connect, which is NOT the guard.
    let out = bb(&home, &["--api", "http://127.0.0.1:9", "ls"], Some("1"));
    assert!(!stderr(&out).contains("Refusing to contact"), "{}", stderr(&out));
}

#[test]
fn without_the_variable_nothing_changes_for_a_user() {
    let home = scratch_home("off");
    // A user who names a public server is not refused; the proxy sends the request nowhere.
    let out = bb(&home, &["--api", "https://api.example.org", "ls"], None);
    assert!(!stderr(&out).contains("Refusing to contact"), "{}", stderr(&out));
    // "0" and an empty value mean off as well.
    for value in ["0", "", "  "] {
        let out = bb(&home, &["--api", "https://api.example.org", "ls"], Some(value));
        assert!(!stderr(&out).contains("Refusing to contact"), "{value:?}: {}", stderr(&out));
    }
}

/// Every test file that runs the binary says it is on the guard: a new test that forgets would point the
/// binary at production the day its scratch config is missing.
#[test]
fn every_test_that_runs_the_binary_sets_the_guard() {
    // This file's own helper sets the variable conditionally; the real-server test is #[ignore]d and takes its
    // server from the environment on purpose.
    const EXEMPT: &[&str] = &["api_guard.rs", "direct_real_server.rs"];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut scanned = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") || EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if !text.contains("CARGO_BIN_EXE_bb") {
            continue;
        }
        scanned += 1;
        let spawns = text.matches("Command::new(env!(\"CARGO_BIN_EXE_bb\"))").count();
        let guarded = text.matches("BB_REFUSE_NON_LOOPBACK_API").count();
        if guarded < spawns {
            offenders.push(format!("{name}: {spawns} spawn(s), {guarded} guard line(s)"));
        }
    }
    assert!(scanned >= 10, "the scan found only {scanned} files that run the binary");
    assert!(offenders.is_empty(), "set BB_REFUSE_NON_LOOPBACK_API=1 on every run of the binary: {offenders:#?}");
}
