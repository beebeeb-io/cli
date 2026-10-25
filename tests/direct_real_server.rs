//! `bb push` + `bb pull` against a REAL local server (task 2173), to prove the
//! API path, the direct path and a forced fallback all return the same bytes.
//!
//! Ignored by default: it needs a running `beebeeb-api` (repos/server main at
//! #329 or later, built locally), a Redis, a Postgres database of its own and a
//! MinIO bucket as the default storage pool, started by the caller. It never
//! starts anything itself and never reads the developer's config: every run has
//! a scratch `HOME`. It refuses to run without its environment (never a vacuous
//! pass), and it never points at production: the URL must be loopback http.
//!
//! ```text
//! BB_REAL_SERVER_URL=http://127.0.0.1:3991   # the server under test
//! BB_REAL_SESSION_TOKEN=<token>              # a session of a fresh test account
//! BB_REAL_EXPECT_TRANSPORT=direct|api        # what `bb pull` must report
//! BB_REAL_REPORT=<file>                      # optional: sha256 lines are appended here
//! cargo test --test direct_real_server -- --ignored --nocapture
//! ```
//!
//! The server side (`BB_DIRECT_DOWNLOAD=1`, the MinIO pool, `BB_DIRECT_TRANSFER_POOLS`)
//! is the caller's; this test only checks what the client got and which
//! transport the CLI says it used.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SIZE: usize = 24 * 1024 * 1024 + 12_345;

fn env(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set (see the module docs); this test never passes vacuously"))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("direct-real-server-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn bb(home: &Path, cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let bin = std::env::var("BB_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_bb").to_string());
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("run bb")
}

fn describe(out: &Output) -> String {
    format!(
        "rc={:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "needs a running server and MinIO: set BB_REAL_SERVER_URL, BB_REAL_SESSION_TOKEN, BB_REAL_EXPECT_TRANSPORT"]
fn real_server_api_direct_and_forced_fallback_return_identical_bytes() {
    let url = env("BB_REAL_SERVER_URL");
    assert!(
        url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:"),
        "refusing to run against {url}: only a loopback http server"
    );
    let token = env("BB_REAL_SESSION_TOKEN");
    let expect = env("BB_REAL_EXPECT_TRANSPORT");
    assert!(
        expect == "direct" || expect == "api",
        "BB_REAL_EXPECT_TRANSPORT is direct or api"
    );
    // The released 0.14.3 binary has no `--transport`; a matrix run says so.
    let has_transport_flag = std::env::var("BB_REAL_NO_TRANSPORT_FLAG").is_err();

    let dir = scratch();
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let config = json!({
        "api_url": url,
        "session_token": token,
        "email": "direct-real@beebeeb.io",
        // 32 zero bytes: the server never sees the master key, any consistent key round-trips.
        "master_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    });
    for d in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("config.json"), serde_json::to_string_pretty(&config).unwrap()).unwrap();
    }

    // Deterministic pseudo-random input (incompressible, reproducible).
    let mut input = Vec::with_capacity(SIZE);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    while input.len() < SIZE {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        input.extend_from_slice(&x.to_le_bytes());
    }
    input.truncate(SIZE);
    std::fs::File::create(cwd.join("in.bin"))
        .unwrap()
        .write_all(&input)
        .unwrap();
    let want = sha256(&input);

    let pushed = bb(&home, &cwd, &["push", "in.bin", "--json"], &[]);
    assert!(pushed.status.success(), "push failed\n{}", describe(&pushed));
    let pushed_json: Value = serde_json::from_slice(&pushed.stdout).expect("push --json");
    let id = pushed_json["files"][0]["id"].as_str().expect("file id").to_string();

    let mut report = vec![format!("input sha256 {want} ({SIZE} bytes) file {id}")];
    /// (label, extra flags, extra environment, transport the run must report)
    type Cell<'a> = (&'a str, Vec<&'a str>, Vec<(&'a str, &'a str)>, &'a str);
    let mut cells: Vec<Cell> = vec![("auto", vec![], vec![], expect.as_str())];
    if has_transport_flag {
        cells.push(("forced-api-flag", vec!["--transport", "api"], vec![], "api"));
        cells.push(("forced-api-env", vec![], vec![("BB_TRANSPORT", "api")], "api"));
    }
    for (label, flags, envs, expected_transport) in cells {
        let out_name = format!("out-{label}.bin");
        let mut args = vec!["pull", id.as_str(), "--json", "-o", out_name.as_str()];
        args.extend(flags);
        let pulled = bb(&home, &cwd, &args, &envs);
        assert!(pulled.status.success(), "pull ({label}) failed\n{}", describe(&pulled));
        let bytes = std::fs::read(cwd.join(&out_name)).unwrap();
        let got = sha256(&bytes);
        report.push(format!("{label}: output sha256 {got} ({} bytes)", bytes.len()));
        assert_eq!(
            got,
            want,
            "pull ({label}) returned different bytes\n{}",
            describe(&pulled)
        );
        if has_transport_flag {
            let json: Value = serde_json::from_slice(&pulled.stdout).expect("pull --json");
            assert_eq!(
                json["transport"],
                expected_transport,
                "pull ({label}) used another transport\n{}",
                describe(&pulled)
            );
        }
        let all = describe(&pulled);
        assert!(
            !all.contains("X-Amz-Signature"),
            "pull ({label}) printed a signature\n{all}"
        );
    }

    if let Ok(path) = std::env::var("BB_REAL_REPORT") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for line in &report {
            writeln!(f, "{line}").unwrap();
        }
    }
    for line in &report {
        println!("{line}");
    }
}
