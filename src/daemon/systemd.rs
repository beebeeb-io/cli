//! The Linux `systemd --user` back-end: installing and removing
//! `beebeeb-sync-<slug>.service` units, and rendering their unit files.
//!
//! Owns: the unit template (with and without an explicit transport) and the
//! Linux/no-op `install_systemd_unit`/`uninstall_systemd_unit` pair.
//! Must not: be reached without passing `daemon::home`'s guard, or call
//! `systemctl` directly instead of `service_manager`.
//! Tests: `daemon::migrate`'s `restart_policy_tests` covers the restart policy
//! and the migration round-trip against the rendered unit.

use super::*;

/// Render the sync systemd user unit.
pub fn render_systemd_unit(slug: &str, exe: &str, local: &str, remote: &str) -> String {
    render_systemd_unit_with(slug, exe, local, remote, None)
}

/// [`render_systemd_unit`] with the transport the installing invocation selected (`api` or `auto`).
pub fn render_systemd_unit_with(slug: &str, exe: &str, local: &str, remote: &str, transport: Option<&str>) -> String {
    let transport_env = transport
        .map(|t| format!("Environment=BB_TRANSPORT={t}\n"))
        .unwrap_or_default();
    format!(
        r#"[Unit]
Description=Beebeeb Sync - {slug}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={exe} sync {local} {remote}
Environment=BEEBEEB_SYNC_SERVICE=1
{transport_env}Restart=on-failure
RestartSec=30
RestartPreventExitStatus=77

[Install]
WantedBy=default.target
"#,
        slug = slug,
        exe = exe,
        local = local,
        remote = remote,
        transport_env = transport_env,
    )
}

/// Install a systemd user service unit for the given sync session.
#[cfg(target_os = "linux")]
pub fn install_systemd_unit(local_dir: &Path, remote_path: &str, slug: &str) -> Result<(), String> {
    ensure_real_home()?;
    let unit_dir = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join(".config/systemd/user");
    fs::create_dir_all(&unit_dir).map_err(|e| format!("create systemd user dir: {e}"))?;

    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let unit_name = format!("beebeeb-sync-{slug}.service");
    let unit_path = unit_dir.join(&unit_name);

    let unit_content = render_systemd_unit_with(
        slug,
        &exe.to_string_lossy(),
        &local_dir.to_string_lossy(),
        remote_path,
        Some(crate::transport::selected_name()),
    );

    fs::write(&unit_path, &unit_content).map_err(|e| format!("write systemd unit: {e}"))?;

    // Reload and enable
    let _ = service_manager("systemctl", &["--user", "daemon-reload"]);

    let status = service_manager("systemctl", &["--user", "enable", "--now", &unit_name])?;

    if !status.success() {
        return Err(format!("systemctl enable --now {unit_name} failed"));
    }

    Ok(())
}

/// Stop and remove a systemd user service unit for the given slug.
#[cfg(target_os = "linux")]
pub fn uninstall_systemd_unit(slug: &str) -> Result<bool, String> {
    let unit_name = format!("beebeeb-sync-{slug}.service");
    let unit_path = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join(".config/systemd/user")
        .join(&unit_name);

    if !unit_path.exists() {
        return Ok(false);
    }
    ensure_real_home()?;

    let _ = service_manager("systemctl", &["--user", "stop", &unit_name]);
    let _ = service_manager("systemctl", &["--user", "disable", &unit_name]);

    fs::remove_file(&unit_path).map_err(|e| format!("remove systemd unit: {e}"))?;

    let _ = service_manager("systemctl", &["--user", "daemon-reload"]);

    Ok(true)
}

/// No-op on non-Linux for systemd install.
#[cfg(not(target_os = "linux"))]
pub fn install_systemd_unit(_local_dir: &Path, _remote_path: &str, _slug: &str) -> Result<(), String> {
    Ok(())
}

/// No-op on non-Linux for systemd uninstall.
#[cfg(not(target_os = "linux"))]
pub fn uninstall_systemd_unit(_slug: &str) -> Result<bool, String> {
    Ok(false)
}
