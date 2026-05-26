//! OTA self-update — checks GitHub releases on startup and auto-updates the binary.
//!
//! Behaviour:
//! - Cooldown of 1 hour between checks (timestamp persisted in `~/.config/beebeeb/last_update_check`).
//! - Network requests time out after 10 seconds so they never block startup noticeably.
//! - Skipped entirely when `BB_NO_UPDATE=1` or running from a cargo development build.
//! - On failure at any stage, we silently fall through — the user's command runs normally.
//! - After a successful update, the process re-execs itself with the same arguments.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use colored::Colorize;
use sha2::{Digest, Sha256};

const CHECK_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/beebeeb-io/cli/releases/latest";

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Check for updates and, if a newer version exists, download and replace the
/// current binary then re-exec the process. Returns normally when no update is
/// needed or when anything goes wrong (never surfaces errors to the user).
pub async fn check_and_update() {
    // Opt-out via env var.
    if std::env::var("BB_NO_UPDATE").unwrap_or_default() == "1" {
        return;
    }

    // Skip when running from a Cargo development build.
    if is_dev_build() {
        return;
    }

    // Cooldown — don't check more than once per hour.
    let state = state_file();
    if !cooldown_elapsed(&state) {
        return;
    }

    // Run the actual check + update, ignoring any errors.
    if let Err(_e) = try_update(&state).await {
        // Silently swallow — don't disturb the user.
        #[cfg(debug_assertions)]
        eprintln!("  [update debug] {_e}");
    }
}

// ---------------------------------------------------------------------------
// Core update logic
// ---------------------------------------------------------------------------

async fn try_update(state: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!("bb/{CURRENT_VERSION}"))
        .build()?;

    let resp = client.get(GITHUB_RELEASES_URL).send().await?;
    if !resp.status().is_success() {
        // Rate-limited or other API error — skip silently.
        return Ok(());
    }

    let release: GitHubRelease = resp.json().await?;
    let remote_ver = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);

    let local = semver::Version::parse(CURRENT_VERSION)?;
    let remote = semver::Version::parse(remote_ver)?;

    // Write the timestamp regardless of whether there's an update.
    write_timestamp(state);

    if remote <= local {
        return Ok(()); // Already up to date — print nothing.
    }

    // Detect if installed via Homebrew and delegate if so.
    if is_homebrew_install() {
        return update_via_homebrew(CURRENT_VERSION, remote_ver);
    }

    // Find the right asset for this platform.
    let target = current_target();
    let asset_name = format!("beebeeb-cli-{target}.tar.xz");
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| format!("no release asset for {target}"))?;

    // Locate the cargo-dist manifest published alongside the tarball.
    // Fail-closed: if the manifest is missing, refuse to update — the previous
    // behaviour of "just trust HTTPS to GitHub Releases" is not acceptable.
    let manifest_asset = release
        .assets
        .iter()
        .find(|a| a.name == "dist-manifest.json")
        .ok_or("dist-manifest.json missing from release — refusing to update without checksum verification")?;

    eprintln!(
        "  {} bb v{CURRENT_VERSION} -> v{remote}...",
        "Updating".custom_color(crate::colors::AMBER),
    );

    // Download the signed manifest first so we have the expected hash before
    // we have the bytes to compare it against.
    let manifest_bytes = client
        .get(&manifest_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    let manifest: DistManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| format!("dist-manifest.json is malformed: {e}"))?;

    let expected_sha256 = manifest
        .artifacts
        .get(&asset_name)
        .and_then(|a| a.checksums.as_ref())
        .and_then(|c| c.sha256.as_deref())
        .ok_or_else(|| format!("dist-manifest.json has no sha256 for {asset_name} — refusing to update"))?;

    // Download the tarball.
    let tarball_bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    // Verify SHA-256 BEFORE extracting or touching the binary. If the hash
    // does not match the manifest we abort and never run any code from the
    // downloaded archive.
    let actual_sha256 = sha256_hex(&tarball_bytes);
    if !ct_eq_ignore_case(&actual_sha256, expected_sha256) {
        return Err(format!(
            "SHA-256 mismatch for {asset_name}: expected {expected_sha256}, got {actual_sha256} — aborting update"
        )
        .into());
    }

    // Extract the `bb` binary from the tarball.
    let binary_data = extract_binary_from_tarball(&tarball_bytes, target)?;

    // Atomic replacement: write to a temp file next to the current exe, then rename.
    let current_exe = std::env::current_exe()?;
    let temp_path = current_exe.with_extension("update-tmp");

    std::fs::write(&temp_path, &binary_data)?;

    // Ensure executable permission on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Replace the old binary.
    std::fs::rename(&temp_path, &current_exe)?;

    eprintln!(
        "  {} Updated bb v{} -> v{}",
        "✓".custom_color(crate::colors::GREEN_OK),
        CURRENT_VERSION.custom_color(crate::colors::AMBER),
        remote.to_string().custom_color(crate::colors::AMBER),
    );

    // Re-exec with the same arguments — this never returns.
    re_exec(&current_exe)
}

// ---------------------------------------------------------------------------
// Homebrew detection and update
// ---------------------------------------------------------------------------

/// Returns `true` when the binary was installed via Homebrew — either a
/// symlink into the Cellar or a direct file under /opt/homebrew/bin/ (which
/// happens when a previous OTA update overwrote the symlink).
fn is_homebrew_install() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let path = exe.to_string_lossy();
    if path.contains("/Cellar/") {
        return true;
    }
    if path.contains("/homebrew/bin/") {
        // Check if Homebrew actually manages this formula
        let output = std::process::Command::new("brew")
            .args(["list", "--formula", "beebeeb-io/tap/bb"])
            .output();
        return output.map(|o| o.status.success()).unwrap_or(false);
    }
    false
}

/// Update via `brew upgrade` instead of self-replacing.
fn update_via_homebrew(old_ver: &str, new_ver: &str) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "  {} bb v{old_ver} -> v{new_ver} via Homebrew...",
        "Updating".custom_color(crate::colors::AMBER),
    );

    let status = std::process::Command::new("brew")
        .args(["upgrade", "beebeeb-io/tap/bb"])
        .status()?;

    if !status.success() {
        return Err("brew upgrade failed".into());
    }

    // Restore the symlink in case a previous OTA update overwrote it with a
    // direct binary. Without this, brew upgrades the Cellar but the bin path
    // stays stale.
    let _ = std::process::Command::new("brew")
        .args(["link", "--overwrite", "beebeeb-io/tap/bb"])
        .status();

    eprintln!(
        "  {} Updated bb v{} -> v{}",
        "✓".custom_color(crate::colors::GREEN_OK),
        old_ver.custom_color(crate::colors::AMBER),
        new_ver.custom_color(crate::colors::AMBER),
    );

    // Re-exec from the Homebrew-linked path, not current_exe() which may be
    // a stale direct binary from a previous OTA overwrite.
    let brew_bin = PathBuf::from("/opt/homebrew/bin/bb");
    let exe = if brew_bin.exists() {
        brew_bin
    } else {
        std::env::current_exe()?
    };
    re_exec(&exe)
}

/// Extract the `bb` binary from a `.tar.xz` archive.
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
    path.contains("target/debug") || path.contains("target/release")
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
fn cooldown_elapsed(path: &PathBuf) -> bool {
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

fn write_timestamp(path: &PathBuf) {
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
    assets: Vec<GitHubAsset>,
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// cargo-dist manifest types (minimal — we only read the fields we trust)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct DistManifest {
    /// Map of artifact filename -> metadata. cargo-dist emits one entry per
    /// release asset (tarballs, installers, checksums, etc.).
    #[serde(default)]
    artifacts: HashMap<String, DistArtifact>,
}

#[derive(serde::Deserialize)]
struct DistArtifact {
    #[serde(default)]
    checksums: Option<DistChecksums>,
}

#[derive(serde::Deserialize)]
struct DistChecksums {
    #[serde(default)]
    sha256: Option<String>,
}

// ---------------------------------------------------------------------------
// Hash helpers
// ---------------------------------------------------------------------------

/// Compute the SHA-256 of `bytes` and return it as a lowercase hex string.
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
