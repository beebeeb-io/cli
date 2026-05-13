//! OTA self-update — checks GitHub releases on startup and auto-updates the binary.
//!
//! Behaviour:
//! - Cooldown of 1 hour between checks (timestamp persisted in `~/.config/beebeeb/last_update_check`).
//! - Network requests time out after 10 seconds so they never block startup noticeably.
//! - Skipped entirely when `BB_NO_UPDATE=1` or running from a cargo development build.
//! - On failure at any stage, we silently fall through — the user's command runs normally.
//! - After a successful update, the process re-execs itself with the same arguments.

use std::io::Read as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use colored::Colorize;

const CHECK_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/beebeeb-io/cli/releases/latest";

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

    eprintln!(
        "  {} bb v{CURRENT_VERSION} -> v{remote}...",
        "Updating".custom_color(crate::colors::AMBER),
    );

    // Download the tarball.
    let tarball_bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

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
fn update_via_homebrew(
    old_ver: &str,
    new_ver: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let exe = if brew_bin.exists() { brew_bin } else { std::env::current_exe()? };
    re_exec(&exe)
}

/// Extract the `bb` binary from a `.tar.xz` archive.
fn extract_binary_from_tarball(
    data: &[u8],
    target: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
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
    { "aarch64-apple-darwin" }

    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    { "x86_64-apple-darwin" }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    { "x86_64-unknown-linux-musl" }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    { "aarch64-unknown-linux-musl" }

    #[cfg(not(any(
        all(target_arch = "aarch64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "macos"),
        all(target_arch = "x86_64", target_os = "linux"),
        all(target_arch = "aarch64", target_os = "linux"),
    )))]
    { "unknown" }
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
