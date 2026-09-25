//! `bb mount` availability must match what the released binaries can actually do.
//!
//! The release binaries (cargo-dist, `dist-workspace.toml`) are built WITHOUT the
//! `fuse` Cargo feature, so they cannot mount. Before this test existed, v0.10.0:
//!   - told users to "download the FUSE build from github.com/beebeeb-io/cli/releases"
//!     — no such asset has ever been published;
//!   - listed `mount` / `unmount` in `bb --help`;
//!   - advertised `bb mount` in the README command table with no caveat.
//!
//! These tests pin the honest behaviour: a build without FUSE points at `bb webdav`
//! (which ships in every release binary), never at a nonexistent download, and the
//! README claim follows the dist feature list.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A private scratch directory under the target dir (no tempfile dev-dep needed).
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("mount-availability-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Run the built `bb` fully isolated: scratch HOME (the real config may hold a live
/// session), no update check, no colour, stdin closed, and an EMPTY `PATH` so no
/// version of the stub can shell out to brew / apt / sudo from a test.
fn run_bb(home: &Path, args: &[&str]) -> Output {
    let empty_path = home.join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("create empty PATH dir");
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("PATH", &empty_path)
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn bb")
}

fn strip_ansi(s: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-9;]*m")
        .unwrap()
        .replace_all(s, "")
        .into_owned()
}

fn combined(out: &Output) -> String {
    strip_ansi(&format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// Does the cargo-dist release build enable the `fuse` feature?
fn dist_builds_with_fuse() -> bool {
    let dist = std::fs::read_to_string(repo_root().join("dist-workspace.toml")).expect("read dist-workspace.toml");
    dist.lines().map(str::trim).filter(|l| !l.starts_with('#')).any(|l| {
        (l.starts_with("features") || l.starts_with("all-features")) && l.contains("fuse") || l == "all-features = true"
    })
}

#[cfg(not(feature = "fuse"))]
#[test]
fn mount_stub_points_at_webdav_not_a_nonexistent_fuse_build() {
    let home = scratch("stub");
    let mountpoint = home.join("vault");
    std::fs::create_dir_all(&mountpoint).unwrap();

    let out = run_bb(
        &home,
        &["--api", "http://127.0.0.1:3319", "mount", mountpoint.to_str().unwrap()],
    );
    let text = combined(&out);

    assert!(
        !text.contains("download the FUSE build"),
        "stub points at a FUSE release asset that does not exist:\n{text}"
    );
    assert!(
        text.contains("bb webdav"),
        "stub must point at `bb webdav`, which ships in every release binary:\n{text}"
    );
    assert!(
        !out.status.success(),
        "`bb mount` cannot mount in this build, so it must exit non-zero (got {:?}):\n{text}",
        out.status
    );
}

#[cfg(not(feature = "fuse"))]
#[test]
fn help_does_not_list_mount_in_a_build_without_fuse() {
    let home = scratch("help");
    let out = run_bb(&home, &["--help"]);
    let text = combined(&out);
    assert!(out.status.success(), "bb --help failed:\n{text}");
    // The derived overflow line ("+ a, b, c") lists every non-hidden subcommand.
    let overflow = text.lines().map(str::trim).find(|l| l.starts_with("+ ")).unwrap_or("");
    let listed: Vec<&str> = overflow.trim_start_matches("+ ").split(", ").map(str::trim).collect();
    assert!(
        !listed.contains(&"mount") && !listed.contains(&"unmount"),
        "bb --help advertises mount/unmount in a binary that cannot mount: {overflow}"
    );
    // And it must still list real commands, or this check proved nothing.
    assert!(
        listed.len() >= 10,
        "overflow line looks empty/unparsed: {overflow:?}\n{text}"
    );
}

#[test]
fn readme_mount_claim_matches_the_dist_feature_list() {
    let readme = std::fs::read_to_string(repo_root().join("README.md")).expect("read README.md");
    let mount_rows: Vec<&str> = readme.lines().filter(|l| l.starts_with("| `bb mount")).collect();
    assert_eq!(
        mount_rows.len(),
        1,
        "expected exactly one `bb mount` row in the README command table"
    );
    let row = mount_rows[0];

    if dist_builds_with_fuse() {
        return; // release binaries can mount; an unqualified claim is true.
    }
    assert!(
        row.contains("--features fuse") && row.contains("bb webdav"),
        "dist-workspace.toml builds release binaries WITHOUT the `fuse` feature, but the README \
         advertises `bb mount` without saying it is source-build-only and pointing at `bb webdav`:\n{row}"
    );
}
