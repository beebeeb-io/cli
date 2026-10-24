//! Automatic and explicit (`bb update`) release updates from GitHub.
//!
//! Behaviour:
//! - Cooldown of 1 hour between checks (timestamp persisted in `~/.config/beebeeb/last_update_check`).
//! - Network requests time out after 10 seconds so they never block startup noticeably.
//! - Automatic checks skip `BB_NO_UPDATE=1`, cargo development paths, and help/version output.
//! - Automatic failures leave the user's command running normally; explicit failures return an error.
//! - Automatic updates re-exec the original command; explicit updates print the result and finish.
//! - Homebrew and Scoop retain ownership of managed installations.

#[cfg(any(not(windows), test))]
use std::collections::HashMap;
#[cfg(not(windows))]
use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(any(not(windows), test))]
use sha2::{Digest, Sha256};

const CHECK_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/beebeeb-io/cli/releases/latest";

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

type UpdateResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum UpdateMode {
    Automatic,
    Manual,
}

#[derive(Debug, PartialEq, Eq)]
enum UpdateOutcome {
    AlreadyCurrent,
    Updated { version: String, executable: PathBuf },
}

fn should_check(mode: UpdateMode, disabled: bool, development: bool, elapsed: bool) -> bool {
    mode == UpdateMode::Manual || (!disabled && !development && elapsed)
}

/// Best-effort startup check. Only automatic updates restart the original command.
pub async fn check_and_update() {
    let state = state_file();
    if !should_check(
        UpdateMode::Automatic,
        std::env::var("BB_NO_UPDATE").unwrap_or_default() == "1",
        is_dev_build(),
        cooldown_elapsed(&state),
    ) {
        return;
    }
    let result = match std::env::current_exe() {
        Ok(exe) => try_update(&state, &exe, GITHUB_RELEASES_URL, std::path::Path::new("brew")).await,
        Err(error) => Err(error.into()),
    };
    match result {
        Ok(UpdateOutcome::Updated { executable, .. }) => re_exec(&executable),
        Ok(UpdateOutcome::AlreadyCurrent) => {}
        Err(_error) => {
            #[cfg(debug_assertions)]
            eprintln!("  [update debug] {_error}");
        }
    }
}

/// A user-requested update always checks now and surfaces errors. It finishes
/// normally after replacement instead of re-running `bb update` in a loop.
pub async fn run_manual() -> Result<(), String> {
    let state = state_file();
    if !should_check(
        UpdateMode::Manual,
        std::env::var("BB_NO_UPDATE").unwrap_or_default() == "1",
        is_dev_build(),
        cooldown_elapsed(&state),
    ) {
        return Ok(());
    }
    if cfg!(debug_assertions) {
        return Err(
            "development builds cannot self-update; install a release with curl -fsSL https://get.beebeeb.io | sh"
                .into(),
        );
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let outcome = try_update(&state, &exe, GITHUB_RELEASES_URL, std::path::Path::new("brew"))
        .await
        .map_err(|error| error.to_string())?;
    let (updated, version) = match outcome {
        UpdateOutcome::AlreadyCurrent => (false, CURRENT_VERSION.to_string()),
        UpdateOutcome::Updated { version, .. } => (true, version),
    };
    if crate::ui::is_json() {
        println!("{}", serde_json::json!({"updated": updated, "version": version}));
    } else if crate::ui::is_quiet() {
        println!("{version}");
    } else if updated {
        println!("Updated bb v{CURRENT_VERSION} -> v{version}");
    } else {
        println!("bb v{version} is already up to date.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core update logic. Endpoint/executable arguments are internal test seams;
// production entry points always use the fixed GitHub release URL above.
// ---------------------------------------------------------------------------

async fn try_update(
    state: &std::path::Path,
    current_exe: &std::path::Path,
    releases_url: &str,
    _brew: &std::path::Path,
) -> UpdateResult<UpdateOutcome> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!("bb/{CURRENT_VERSION}"))
        .build()?;
    let release: GitHubRelease = client
        .get(releases_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let remote_ver = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
    let local = semver::Version::parse(CURRENT_VERSION)?;
    let remote = semver::Version::parse(remote_ver)?;
    write_timestamp(state);
    if remote <= local {
        return Ok(UpdateOutcome::AlreadyCurrent);
    }

    #[cfg(windows)]
    {
        let executable = update_via_scoop(current_exe, &remote)?;
        Ok(UpdateOutcome::Updated {
            version: remote.to_string(),
            executable,
        })
    }

    #[cfg(not(windows))]
    {
        if let Some(executable) = homebrew_executable(current_exe, _brew)? {
            update_via_homebrew(_brew, &executable, &remote)?;
            return Ok(UpdateOutcome::Updated {
                version: remote.to_string(),
                executable,
            });
        }
        let target = current_target();
        let asset_name = format!("beebeeb-cli-{target}.tar.xz");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no release asset for {target}"))?;
        let manifest_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == "dist-manifest.json")
            .ok_or("dist-manifest.json missing from release — refusing to update without checksum verification")?;
        eprintln!("  Updating bb v{CURRENT_VERSION} -> v{remote}...");

        // The manifest is checksum metadata, not a cryptographic signature.
        let manifest: DistManifest = client
            .get(&manifest_asset.browser_download_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .map_err(|error| format!("dist-manifest.json is malformed: {error}"))?;
        let expected = manifest
            .artifacts
            .get(&asset_name)
            .and_then(|artifact| artifact.checksums.as_ref())
            .and_then(|checksums| checksums.sha256.as_deref())
            .ok_or_else(|| format!("dist-manifest.json has no sha256 for {asset_name} — refusing to update"))?;
        let archive = client
            .get(&asset.browser_download_url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        let actual = sha256_hex(&archive);
        if !ct_eq_ignore_case(&actual, expected) {
            return Err(format!(
                "SHA-256 mismatch for {asset_name}: expected {expected}, got {actual} — aborting update"
            )
            .into());
        }
        let binary = extract_binary_from_tarball(&archive, target)?;
        replace_binary(current_exe, &binary, &remote)?;
        Ok(UpdateOutcome::Updated {
            version: remote.to_string(),
            executable: current_exe.to_path_buf(),
        })
    }
}

#[cfg(not(windows))]
fn replace_binary(executable: &std::path::Path, binary: &[u8], version: &semver::Version) -> UpdateResult<()> {
    use std::io::Write as _;
    let staged = executable.with_extension(format!("update-tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&staged)?;
    let result = (|| {
        file.write_all(binary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        verify_installed_version(&staged, version)?;
        std::fs::rename(&staged, executable)?;
        Ok(())
    })();
    // A failed verification/replacement must not leave an executable behind.
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

fn verify_installed_version(executable: &std::path::Path, expected: &semver::Version) -> UpdateResult<()> {
    let output = std::process::Command::new(executable)
        .arg("--version")
        .env("BB_NO_UPDATE", "1")
        .output()?;
    if !output.status.success() {
        return Err("updated bb --version failed; refusing to report an update".into());
    }
    let output = String::from_utf8(output.stdout)?;
    let found = output
        .split_whitespace()
        .last()
        .and_then(|value| semver::Version::parse(value).ok());
    if found.as_ref() != Some(expected) {
        return Err(format!(
            "installed bb reports {}; expected v{expected}. Refresh your package manager and retry bb update.",
            output.trim()
        )
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Package-manager updates: preserve ownership and verify the linked executable.
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn homebrew_executable(executable: &std::path::Path, brew: &std::path::Path) -> UpdateResult<Option<PathBuf>> {
    let cellar = executable.components().any(|part| part.as_os_str() == "Cellar");
    let output = std::process::Command::new(brew).arg("--prefix").output();
    let prefix = match output {
        Ok(output) if output.status.success() => PathBuf::from(String::from_utf8(output.stdout)?.trim()),
        _ if cellar => {
            return Err(
                "Homebrew install detected but brew --prefix failed; repair Homebrew and retry bb update".into(),
            );
        }
        _ => return Ok(None),
    };
    if !prefix.is_absolute() || (cellar && !executable.starts_with(prefix.join("Cellar/bb"))) {
        return Err("Homebrew prefix does not match this bb installation; use its matching brew command".into());
    }
    let linked = prefix.join("bin/bb");
    if !cellar && executable != linked {
        return Ok(None);
    }
    let listed = std::process::Command::new(brew)
        .args(["list", "--formula", "beebeeb-io/tap/bb"])
        .output()?;
    if !listed.status.success() {
        if cellar {
            return Err("Homebrew does not list beebeeb-io/tap/bb; repair the installation before updating".into());
        }
        return Ok(None);
    }
    Ok(Some(linked))
}

#[cfg(not(windows))]
fn update_via_homebrew(
    brew: &std::path::Path,
    linked: &std::path::Path,
    version: &semver::Version,
) -> UpdateResult<()> {
    eprintln!("  Updating bb via Homebrew...");
    for args in [
        ["update", "--quiet"].as_slice(),
        ["upgrade", "beebeeb-io/tap/bb"].as_slice(),
        ["link", "--overwrite", "beebeeb-io/tap/bb"].as_slice(),
    ] {
        let status = std::process::Command::new(brew)
            .args(args)
            .stdout(std::process::Stdio::from(std::io::stderr()))
            .status()?;
        if !status.success() {
            return Err(format!("brew {} failed; retry after fixing Homebrew", args[0]).into());
        }
    }
    verify_installed_version(linked, version)
}

#[cfg(any(windows, test))]
fn scoop_executable(executable: &std::path::Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| {
            path.file_name().is_some_and(|name| name == "bb")
                && path
                    .parent()
                    .and_then(std::path::Path::file_name)
                    .is_some_and(|name| name == "apps")
        })
        .map(|root| root.join("current/bb.exe"))
}

#[cfg(windows)]
fn update_via_scoop(executable: &std::path::Path, version: &semver::Version) -> UpdateResult<PathBuf> {
    let linked = scoop_executable(executable).ok_or("unmanaged Windows installation: reinstall the latest release from https://github.com/beebeeb-io/cli/releases or install with Scoop; bb update uses scoop update bb for Scoop-managed installs")?;
    for args in [
        ["/C", "scoop", "update"].as_slice(),
        ["/C", "scoop", "update", "bb"].as_slice(),
    ] {
        let status = std::process::Command::new("cmd")
            .args(args)
            .stdout(std::process::Stdio::from(std::io::stderr()))
            .status()?;
        if !status.success() {
            return Err("scoop update failed; repair Scoop and retry bb update".into());
        }
    }
    verify_installed_version(&linked, version)?;
    Ok(linked)
}

/// Extract the `bb` binary from a `.tar.xz` archive.
#[cfg(not(windows))]
fn extract_binary_from_tarball(data: &[u8], target: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let xz_reader = xz2::read::XzDecoder::new(data);
    let mut archive = tar::Archive::new(xz_reader);

    // The binary lives at `beebeeb-cli-{target}/bb` inside the tarball.
    let expected_path = format!("beebeeb-cli-{target}/bb");

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.to_string_lossy() == expected_path {
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }

    Err(format!("binary not found at {expected_path} in tarball").into())
}

/// Replace the current process with the updated binary (Unix `exec`).
/// On non-Unix platforms, spawn a child and exit.
fn re_exec(exe: &std::path::Path) -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // exec() replaces the process — this does not return on success.
        let err = std::process::Command::new(exe).args(&args).exec();
        eprintln!("  exec failed: {err}");
        std::process::exit(1);
    }

    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(exe)
            .args(&args)
            .status()
            .unwrap_or_else(|e| {
                eprintln!("  failed to restart: {e}");
                std::process::exit(1);
            });
        std::process::exit(status.code().unwrap_or(1));
    }
}

// ---------------------------------------------------------------------------
// Platform detection
// ---------------------------------------------------------------------------

/// Return the Rust target triple for the currently running binary.
#[cfg(not(windows))]
fn current_target() -> &'static str {
    // These are set at compile time via cfg.
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        "aarch64-apple-darwin"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "x86_64-apple-darwin"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        "x86_64-unknown-linux-musl"
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        "aarch64-unknown-linux-musl"
    }

    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
    )))]
    {
        "unknown"
    }
}

/// Returns `true` when the binary is a development build (running via `cargo run`
/// or from a `target/debug` path).
fn is_dev_build() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let path = exe.to_string_lossy();
    cfg!(debug_assertions) || path.contains("target/debug") || path.contains("target/release")
}

// ---------------------------------------------------------------------------
// Cooldown / state file
// ---------------------------------------------------------------------------

fn state_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beebeeb")
        .join("last_update_check")
}

/// Returns `true` when enough time has passed since the last check.
fn cooldown_elapsed(path: &std::path::Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return true; // No state file — first run.
    };
    let Ok(ts) = contents.trim().parse::<u64>() else {
        return true; // Corrupt file — re-check.
    };
    let last = UNIX_EPOCH + Duration::from_secs(ts);
    let Ok(elapsed) = SystemTime::now().duration_since(last) else {
        return true; // Clock went backwards — re-check.
    };
    elapsed >= CHECK_INTERVAL
}

fn write_timestamp(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = std::fs::write(path, now.to_string());
}

// ---------------------------------------------------------------------------
// GitHub API types (minimal)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[cfg(not(windows))]
    assets: Vec<GitHubAsset>,
}

#[cfg(not(windows))]
#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// cargo-dist manifest types (minimal — we only read the fields we trust)
// ---------------------------------------------------------------------------

#[cfg(any(not(windows), test))]
#[derive(serde::Deserialize)]
struct DistManifest {
    /// Map of artifact filename -> metadata. cargo-dist emits one entry per
    /// release asset (tarballs, installers, checksums, etc.).
    #[serde(default)]
    artifacts: HashMap<String, DistArtifact>,
}

#[cfg(any(not(windows), test))]
#[derive(serde::Deserialize)]
struct DistArtifact {
    #[serde(default)]
    checksums: Option<DistChecksums>,
}

#[cfg(any(not(windows), test))]
#[derive(serde::Deserialize)]
struct DistChecksums {
    #[serde(default)]
    sha256: Option<String>,
}

// ---------------------------------------------------------------------------
// Hash helpers
// ---------------------------------------------------------------------------

/// Compute the SHA-256 of `bytes` and return it as a lowercase hex string.
#[cfg(any(not(windows), test))]
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut hex = String::with_capacity(out.len() * 2);
    for b in out.iter() {
        // Manual hex encode to avoid pulling in another crate. Two-char lowercase.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        hex.push(HEX[(b >> 4) as usize] as char);
        hex.push(HEX[(b & 0x0f) as usize] as char);
    }
    hex
}

/// Constant-time-ish equality for two equal-length hex strings (case-insensitive).
/// Hash comparison is not a high-value timing target, but folding the check in
/// constant time costs us nothing and avoids early-exit on byte mismatch.
#[cfg(any(not(windows), test))]
fn ct_eq_ignore_case(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x.to_ascii_lowercase() ^ y.to_ascii_lowercase();
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_update_bypasses_automatic_policy() {
        for disabled in [false, true] {
            for development in [false, true] {
                for elapsed in [false, true] {
                    assert!(should_check(UpdateMode::Manual, disabled, development, elapsed));
                    assert_eq!(
                        should_check(UpdateMode::Automatic, disabled, development, elapsed),
                        !disabled && !development && elapsed
                    );
                }
            }
        }
    }

    #[test]
    fn scoop_only_targets_its_managed_install() {
        assert_eq!(
            scoop_executable(std::path::Path::new("/users/test/scoop/apps/bb/0.14.1/bb.exe")),
            Some(PathBuf::from("/users/test/scoop/apps/bb/current/bb.exe"))
        );
        assert_eq!(scoop_executable(std::path::Path::new("/downloads/bb.exe")), None);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        struct Scratch(PathBuf);
        impl Scratch {
            fn new() -> Self {
                let root = std::env::temp_dir().join(format!("bb-update-test-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&root).unwrap();
                Self(root)
            }
            fn path(&self, name: &str) -> PathBuf {
                self.0.join(name)
            }
        }
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn write_executable(path: &std::path::Path, bytes: &[u8]) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn archive(binary: &[u8]) -> Vec<u8> {
            let encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
            let mut archive = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(binary.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive
                .append_data(&mut header, format!("beebeeb-cli-{}/bb", current_target()), binary)
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap()
        }

        struct Server {
            url: String,
            calls: Arc<AtomicUsize>,
            task: tokio::task::JoinHandle<()>,
        }
        impl Drop for Server {
            fn drop(&mut self) {
                self.task.abort();
            }
        }

        async fn server(version: &str, checksum: Option<&str>, binary: &[u8], status: u16) -> Server {
            use axum::{Router, response::IntoResponse, routing::get};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let name = format!("beebeeb-cli-{}.tar.xz", current_target());
            let release = serde_json::json!({"tag_name": version, "assets": [
                {"name": name, "browser_download_url": format!("{base}/archive")},
                {"name": "dist-manifest.json", "browser_download_url": format!("{base}/manifest")}
            ]})
            .to_string();
            let manifest = serde_json::json!({"artifacts": {name: {"checksums": {"sha256": checksum}}}}).to_string();
            let archive = archive(binary);
            let calls = Arc::new(AtomicUsize::new(0));
            let api = Router::new()
                .route(
                    "/latest",
                    get({
                        let calls = calls.clone();
                        move || {
                            let calls = calls.clone();
                            let release = release.clone();
                            async move {
                                calls.fetch_add(1, Ordering::SeqCst);
                                (
                                    axum::http::StatusCode::from_u16(status).unwrap(),
                                    [("content-type", "application/json")],
                                    release,
                                )
                                    .into_response()
                            }
                        }
                    }),
                )
                .route(
                    "/manifest",
                    get({
                        let calls = calls.clone();
                        move || {
                            let calls = calls.clone();
                            let manifest = manifest.clone();
                            async move {
                                calls.fetch_add(1, Ordering::SeqCst);
                                ([("content-type", "application/json")], manifest)
                            }
                        }
                    }),
                )
                .route(
                    "/archive",
                    get({
                        let calls = calls.clone();
                        move || {
                            let calls = calls.clone();
                            let archive = archive.clone();
                            async move {
                                calls.fetch_add(1, Ordering::SeqCst);
                                archive
                            }
                        }
                    }),
                );
            let task = tokio::spawn(async move {
                axum::serve(listener, api).await.unwrap();
            });
            Server {
                url: format!("{base}/latest"),
                calls,
                task,
            }
        }

        const BINARY: &[u8] = b"#!/bin/sh\nprintf 'bb 999.0.0\\n'\n";

        #[tokio::test]
        async fn already_current_reports_success_after_a_fresh_request() {
            let scratch = Scratch::new();
            let exe = scratch.path("bb");
            std::fs::write(&exe, b"original").unwrap();
            // A future cooldown cannot suppress an explicitly requested check.
            let state = scratch.path("last_update_check");
            std::fs::write(&state, "9999999999").unwrap();
            let server = server(CURRENT_VERSION, None, BINARY, 200).await;
            let outcome = try_update(&state, &exe, &server.url, &scratch.path("no-brew"))
                .await
                .unwrap();
            assert_eq!(outcome, UpdateOutcome::AlreadyCurrent);
            assert_eq!(server.calls.load(Ordering::SeqCst), 1);
            assert_eq!(std::fs::read(&exe).unwrap(), b"original");
            assert_ne!(std::fs::read_to_string(&state).unwrap(), "9999999999");
        }

        #[tokio::test]
        async fn release_http_error_is_surfaced() {
            let scratch = Scratch::new();
            let server = server("999.0.0", None, BINARY, 503).await;
            let error = try_update(
                &scratch.path("state"),
                &scratch.path("bb"),
                &server.url,
                &scratch.path("no-brew"),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("503"), "{error}");
            assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn verified_release_replaces_executable_without_restarting_update() {
            let scratch = Scratch::new();
            let exe = scratch.path("bb");
            std::fs::write(&exe, b"original").unwrap();
            let digest = sha256_hex(&archive(BINARY));
            let server = server("999.0.0", Some(&digest), BINARY, 200).await;
            let result = try_update(&scratch.path("state"), &exe, &server.url, &scratch.path("no-brew"))
                .await
                .unwrap();
            assert_eq!(
                result,
                UpdateOutcome::Updated {
                    version: "999.0.0".into(),
                    executable: exe.clone()
                }
            );
            assert_eq!(server.calls.load(Ordering::SeqCst), 3);
            assert_eq!(std::fs::read(&exe).unwrap(), BINARY);
            verify_installed_version(&exe, &semver::Version::parse("999.0.0").unwrap()).unwrap();
            assert!(
                !exe.with_extension(format!("update-tmp-{}", std::process::id()))
                    .exists()
            );
        }

        #[tokio::test]
        async fn checksum_failure_keeps_original_executable() {
            let scratch = Scratch::new();
            let exe = scratch.path("bb");
            std::fs::write(&exe, b"original").unwrap();
            let server = server("999.0.0", Some("00"), BINARY, 200).await;
            let error = try_update(&scratch.path("state"), &exe, &server.url, &scratch.path("no-brew"))
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("SHA-256 mismatch"), "{error}");
            assert_eq!(server.calls.load(Ordering::SeqCst), 3);
            assert_eq!(std::fs::read(&exe).unwrap(), b"original");
        }

        #[tokio::test]
        async fn missing_checksum_does_not_download_or_replace_executable() {
            let scratch = Scratch::new();
            let exe = scratch.path("bb");
            std::fs::write(&exe, b"original").unwrap();
            let server = server("999.0.0", None, BINARY, 200).await;
            let error = try_update(&scratch.path("state"), &exe, &server.url, &scratch.path("no-brew"))
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains("has no sha256"), "{error}");
            assert_eq!(server.calls.load(Ordering::SeqCst), 2);
            assert_eq!(std::fs::read(&exe).unwrap(), b"original");
        }

        #[test]
        fn existing_staging_file_is_never_overwritten_or_removed() {
            let scratch = Scratch::new();
            let exe = scratch.path("bb");
            std::fs::write(&exe, b"original").unwrap();
            let staged = exe.with_extension(format!("update-tmp-{}", std::process::id()));
            std::fs::write(&staged, b"existing").unwrap();
            assert!(replace_binary(&exe, BINARY, &semver::Version::parse("999.0.0").unwrap()).is_err());
            assert_eq!(std::fs::read(&staged).unwrap(), b"existing");
            assert_eq!(std::fs::read(&exe).unwrap(), b"original");
        }

        #[test]
        fn wrong_binary_version_keeps_original_and_removes_staging_file() {
            let scratch = Scratch::new();
            let exe = scratch.path("bb");
            std::fs::write(&exe, b"original").unwrap();
            let error = replace_binary(&exe, BINARY, &semver::Version::parse("999.0.1").unwrap())
                .unwrap_err()
                .to_string();
            assert!(error.contains("expected v999.0.1"), "{error}");
            assert_eq!(std::fs::read(&exe).unwrap(), b"original");
            assert!(
                !exe.with_extension(format!("update-tmp-{}", std::process::id()))
                    .exists()
            );
        }

        fn brew(scratch: &Scratch, fail: &str, version: &str) -> (PathBuf, PathBuf) {
            let prefix = scratch.path("custom-prefix");
            let linked = prefix.join("bin/bb");
            write_executable(&linked, format!("#!/bin/sh\nprintf 'bb {version}\\n'\n").as_bytes());
            let brew = scratch.path("brew");
            write_executable(
                &brew,
                format!(
                    r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
case "$1" in
  --prefix) printf '%s\n' '{}' ;;
  list) exit 0 ;;
  update|upgrade|link) [ "$1" != '{}' ] ;;
  *) exit 2 ;;
esac
"#,
                    scratch.path("brew.log").display(),
                    prefix.display(),
                    fail
                )
                .as_bytes(),
            );
            (brew, linked)
        }

        #[test]
        fn homebrew_uses_its_prefix_and_checks_upgrade_link_and_version() {
            let scratch = Scratch::new();
            let (brew, linked) = brew(&scratch, "", "999.0.0");
            assert_eq!(homebrew_executable(&linked, &brew).unwrap(), Some(linked.clone()));
            update_via_homebrew(&brew, &linked, &semver::Version::parse("999.0.0").unwrap()).unwrap();
            let log = std::fs::read_to_string(scratch.path("brew.log")).unwrap();
            assert_eq!(log.lines().count(), 5);
            assert!(log.contains("update --quiet"));
            assert!(log.contains("upgrade beebeeb-io/tap/bb"));
            assert!(log.contains("link --overwrite beebeeb-io/tap/bb"));
        }

        #[test]
        fn homebrew_progress_uses_stderr() {
            if std::env::var_os("BB_UPDATE_STDIO_CHILD").is_some() {
                let scratch = Scratch::new();
                let (brew, linked) = brew(&scratch, "", "999.0.0");
                let script = std::fs::read_to_string(&brew).unwrap().replace(
                    "  update|upgrade|link)",
                    "  update|upgrade|link) printf 'manager-progress\\n';",
                );
                std::fs::write(&brew, script).unwrap();
                update_via_homebrew(&brew, &linked, &semver::Version::parse("999.0.0").unwrap()).unwrap();
                return;
            }
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "update::tests::unix::homebrew_progress_uses_stderr",
                    "--nocapture",
                ])
                .env("BB_UPDATE_STDIO_CHILD", "1")
                .output()
                .unwrap();
            assert!(child.status.success());
            assert!(
                !String::from_utf8_lossy(&child.stdout).contains("manager-progress"),
                "package-manager output polluted command stdout"
            );
            assert_eq!(
                String::from_utf8_lossy(&child.stderr)
                    .matches("manager-progress")
                    .count(),
                3
            );
        }

        #[test]
        fn homebrew_failures_are_reported() {
            for fail in ["update", "upgrade", "link"] {
                let scratch = Scratch::new();
                let (brew, linked) = brew(&scratch, fail, "999.0.0");
                let error = update_via_homebrew(&brew, &linked, &semver::Version::parse("999.0.0").unwrap())
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(&format!("brew {fail} failed")), "{error}");
            }
        }

        #[test]
        fn homebrew_cannot_claim_latest_when_formula_is_stale() {
            let scratch = Scratch::new();
            let (brew, linked) = brew(&scratch, "", "0.13.2");
            let error = update_via_homebrew(&brew, &linked, &semver::Version::parse("999.0.0").unwrap())
                .unwrap_err()
                .to_string();
            assert!(error.contains("expected v999.0.0"), "{error}");
        }
    }

    #[test]
    fn sha256_hex_empty_input_matches_known_value() {
        // Standard SHA-256 of the empty string.
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256_hex(&[]), expected);
    }

    #[test]
    fn sha256_hex_known_value_for_abc() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), expected);
    }

    #[test]
    fn ct_eq_handles_case_insensitivity() {
        assert!(ct_eq_ignore_case("ABCD", "abcd"));
        assert!(ct_eq_ignore_case("ba7816bf", "BA7816BF"));
    }

    #[test]
    fn ct_eq_detects_mismatch() {
        assert!(!ct_eq_ignore_case("abcd", "abce"));
        assert!(!ct_eq_ignore_case("abcd", "abcde"));
    }

    #[test]
    fn dist_manifest_parses_real_world_shape() {
        // Trimmed shape of a real cargo-dist manifest. We only need the
        // artifacts -> checksums.sha256 path.
        let json = r#"{
            "dist_version": "0.31.0",
            "artifacts": {
                "beebeeb-cli-aarch64-apple-darwin.tar.xz": {
                    "name": "beebeeb-cli-aarch64-apple-darwin.tar.xz",
                    "kind": "executable-zip",
                    "checksums": {
                        "sha256": "27c788a7af0c4fde47f93355b37f956ebd8d6ce9b8ac3f62c19402fca2c32d72"
                    }
                },
                "no-checksum-artifact.sh": {
                    "name": "no-checksum-artifact.sh",
                    "kind": "installer"
                }
            }
        }"#;

        let manifest: DistManifest = serde_json::from_str(json).unwrap();
        let with_sha = manifest
            .artifacts
            .get("beebeeb-cli-aarch64-apple-darwin.tar.xz")
            .and_then(|a| a.checksums.as_ref())
            .and_then(|c| c.sha256.as_deref());
        assert_eq!(
            with_sha,
            Some("27c788a7af0c4fde47f93355b37f956ebd8d6ce9b8ac3f62c19402fca2c32d72")
        );

        let without_sha = manifest
            .artifacts
            .get("no-checksum-artifact.sh")
            .and_then(|a| a.checksums.as_ref())
            .and_then(|c| c.sha256.as_deref());
        assert_eq!(without_sha, None);
    }
}
