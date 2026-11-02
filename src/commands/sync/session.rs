//! `session` items split out of `sync.rs`.

use super::*;

pub(super) const HIDDEN_NAME: &str = "(name hidden)";

/// Seal one client-meta field. `record_key` is the row's own client key;
/// `device_key` is the parent device's client key for session fields (2-4) and
/// `None` for the device hostname (field 1). Text is fitted to the padding
/// limit before it is sealed.
pub(super) fn seal_meta(
    master: &MasterKey,
    record_key: &Uuid,
    device_key: Option<&Uuid>,
    field: ClientMetaField,
    plaintext: &str,
) -> Result<String, String> {
    seal_client_meta(master, record_key, device_key, field, &fit_for_seal(plaintext))
        .map_err(|e| format!("seal client metadata: {e}"))
}

pub(super) fn row_uuid(row: &serde_json::Value, field: &str) -> Option<Uuid> {
    row.get(field)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Open one envelope from a session/device row. `None` means "nothing to show":
/// no record key, no envelope (the server nulls anything it cannot validate),
/// or the envelope does not open under this master key.
pub(super) fn open_meta(
    master: &MasterKey,
    record_key: Option<Uuid>,
    device_key: Option<&Uuid>,
    field: ClientMetaField,
    envelope: Option<&serde_json::Value>,
) -> Option<String> {
    let record_key = record_key?;
    let envelope = envelope?.as_str().filter(|s| !s.is_empty())?;
    open_client_meta(master, &record_key, device_key, field, envelope).ok()
}

/// The decrypted text of one session row, already labelled for display.
pub(super) struct SessionText {
    pub(super) name: String,
    pub(super) local_path: String,
    pub(super) remote_path: String,
    pub(super) device_name: String,
}

pub(super) fn session_text(master: &MasterKey, s: &serde_json::Value) -> SessionText {
    let record_key = row_uuid(s, "client_key");
    let device_key = row_uuid(s, "device_client_key");
    SessionText {
        name: open_meta(
            master,
            record_key,
            device_key.as_ref(),
            ClientMetaField::SessionName,
            s.get("name_encrypted"),
        )
        .unwrap_or_else(|| HIDDEN_NAME.to_string()),
        local_path: open_meta(
            master,
            record_key,
            device_key.as_ref(),
            ClientMetaField::SessionLocalPath,
            s.get("local_path_encrypted"),
        )
        .unwrap_or_default(),
        remote_path: open_meta(
            master,
            record_key,
            device_key.as_ref(),
            ClientMetaField::SessionRemotePath,
            s.get("remote_path_encrypted"),
        )
        .unwrap_or_default(),
        device_name: open_meta(
            master,
            device_key,
            None,
            ClientMetaField::DeviceHostname,
            s.get("device_hostname_encrypted"),
        )
        .unwrap_or_default(),
    }
}

/// Parse the JSON response from list_client_sessions into SessionInfo structs.
pub(super) fn parse_sessions_from_json(
    resp: &serde_json::Value,
    master: &MasterKey,
) -> Vec<crate::tui::state::SessionInfo> {
    use crate::tui::state::SessionInfo;
    let sessions = match resp.get("sessions").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    sessions
        .iter()
        .map(|s| {
            let heartbeat_ago = s
                .get("last_heartbeat")
                .and_then(|v| v.as_str())
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| {
                    let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                    if elapsed.num_seconds() < 60 {
                        format!("{}s ago", elapsed.num_seconds())
                    } else if elapsed.num_minutes() < 60 {
                        format!("{}m ago", elapsed.num_minutes())
                    } else {
                        format!("{}h ago", elapsed.num_hours())
                    }
                });
            let text = session_text(master, s);

            SessionInfo {
                name: text.name,
                device_name: text.device_name,
                remote_path: text.remote_path,
                status: s
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                files_synced: s.get("files_synced").and_then(|v| v.as_u64()).unwrap_or(0),
                bytes_synced: s.get("bytes_synced").and_then(|v| v.as_u64()).unwrap_or(0),
                last_heartbeat: heartbeat_ago,
                speed_bps: s.get("speed_bps").and_then(|v| v.as_u64()),
            }
        })
        .collect()
}

pub(super) async fn print_session_status(api: &ApiClient, master: &MasterKey) -> Result<(), String> {
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    if sessions.is_empty() {
        println!(
            "  {} {}",
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            "no active sync sessions".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    let w = 62;
    println!("{}", crate::ui::box_header("SESSIONS", w));

    for session in sessions {
        let text = session_text(master, session);
        let session_status = session.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
        let files_synced = session.get("files_synced").and_then(|v| v.as_u64()).unwrap_or(0);
        let bytes_synced = session.get("bytes_synced").and_then(|v| v.as_u64()).unwrap_or(0);
        let speed_bps = session.get("speed_bps").and_then(|v| v.as_u64());

        // Compute time since last heartbeat
        let heartbeat_ago = session
            .get("last_heartbeat")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| {
                let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                if elapsed.num_seconds() < 60 {
                    format!("{}s ago", elapsed.num_seconds())
                } else if elapsed.num_minutes() < 60 {
                    format!("{}m ago", elapsed.num_minutes())
                } else {
                    format!("{}h ago", elapsed.num_hours())
                }
            });

        let (indicator, status_text, status_color) = match session_status {
            "watching" | "idle" => ("\u{25CF}", "watching", crate::colors::GREEN_OK),
            "syncing" | "uploading" | "downloading" => ("\u{25C6}", session_status, crate::colors::AMBER),
            "stopped" => ("\u{25CB}", "stopped", crate::colors::INK_DIM),
            "error" => ("\u{25CF}", "error", crate::colors::RED_ERR),
            _ => ("\u{25CF}", session_status, crate::colors::INK_DIM),
        };

        // Line 1: indicator + name + device → remote + status. Every text field
        // here is decrypted locally; a row that opened nothing shows
        // "(name hidden)" and drops the empty device/path segments.
        let mut line1 = format!(
            "  {} {}",
            indicator.custom_color(status_color),
            text.name.custom_color(crate::colors::INK),
        );
        if !text.device_name.is_empty() {
            line1.push_str(&format!("  {}", text.device_name.custom_color(crate::colors::INK_DIM)));
        }
        if !text.remote_path.is_empty() {
            line1.push_str(&format!(
                " \u{2192} {}",
                text.remote_path.custom_color(crate::colors::INK_DIM)
            ));
        }
        line1.push_str(&format!("  {}", status_text.custom_color(status_color)));
        println!("{}", crate::ui::box_line(&line1, w));

        // Line 2: stats
        let mut detail_parts: Vec<String> = Vec::new();
        if files_synced > 0 {
            detail_parts.push(format!("{} files", format_count(files_synced)));
        }
        if bytes_synced > 0 {
            detail_parts.push(crate::ui::human_size(bytes_synced));
        }
        if !text.local_path.is_empty() {
            detail_parts.push(format!("local {}", text.local_path));
        }
        if let Some(ref ago) = heartbeat_ago {
            detail_parts.push(format!("last heartbeat {ago}"));
        }
        if let Some(speed) = speed_bps.filter(|s| *s > 0) {
            detail_parts.push(format!("{}/s", crate::ui::human_speed(speed as f64)));
        }

        if !detail_parts.is_empty() {
            let line2 = format!("    {}", detail_parts.join(" \u{00b7} "));
            println!(
                "{}",
                crate::ui::box_line(&line2.custom_color(crate::colors::INK_DIM).to_string(), w,)
            );
        }
    }

    println!("{}", crate::ui::box_footer(w));
    Ok(())
}

pub(super) fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        format!("{n}")
    }
}

/// Best-effort: mark the session this invocation owns as `stopped` so the
/// server's heartbeat checker stops sending the user "no sync activity"
/// notifications after the agent exits. Never fails or blocks the caller — a
/// stop error is logged and swallowed (the request is bounded by the
/// `ApiClient`'s existing per-request timeout). Only ever called with the
/// current invocation's own `session_id`, never another user's session.
pub(super) async fn stop_session_best_effort(api: &ApiClient, session_id: &str) {
    if let Err(e) = api.stop_client_session(session_id).await {
        if !ui::is_quiet() {
            eprintln!(
                "  {} could not stop sync session on exit: {e}",
                "!".custom_color(crate::colors::AMBER),
            );
        }
    }
}

/// A server older than 1922 (server#363) has no envelopes and still returns the
/// plaintext `name`. An explicit stop command keeps working against it; the
/// display paths never read this field (they show "(name hidden)" instead).
pub(super) fn legacy_plaintext_name(s: &serde_json::Value) -> Option<String> {
    s.get("name")
        .and_then(|v| v.as_str())
        .filter(|n| !n.is_empty())
        .map(String::from)
}

/// The name a stop command matches and slugs against: the decrypted one when the
/// row opens, else the legacy plaintext name, else none.
pub(super) fn stop_match_name(master: &MasterKey, s: &serde_json::Value) -> Option<String> {
    let text = session_text(master, s);
    if text.name == HIDDEN_NAME {
        legacy_plaintext_name(s)
    } else {
        Some(text.name)
    }
}

pub(super) async fn stop_session_by_name(api: &ApiClient, master: &MasterKey, name: &str) -> Result<(), String> {
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    // Match on the DECRYPTED name (task 1922). An unreadable row has no name to
    // match, so it is never stopped by accident.
    let name_lower = name.to_lowercase();
    let matching: Vec<(&serde_json::Value, String)> = sessions
        .iter()
        .filter_map(|s| {
            let candidate = stop_match_name(master, s)?;
            let n_lower = candidate.to_lowercase();
            (n_lower == name_lower || n_lower.ends_with(&format!("/{name_lower}")) || n_lower.contains(&name_lower))
                .then_some((s, candidate))
        })
        .collect();

    if matching.is_empty() {
        return Err(format!("no session found with name \"{name}\""));
    }

    for (session, session_name) in matching {
        let id = session.get("id").and_then(|v| v.as_str()).ok_or("session missing id")?;

        api.stop_client_session(id).await?;

        // Kill the local daemon process + remove auto-start
        let slug = crate::daemon::slugify(&session_name);
        let _ = crate::daemon::stop_daemon(&slug);

        println!(
            "  {} stopped {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            session_name.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

pub(super) async fn stop_all_sessions(api: &ApiClient, master: &MasterKey) -> Result<(), String> {
    let device_info = crate::device::load_or_create();
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    // Filter to sessions on this device by the device's own client key — the
    // exact identity. The old plaintext `device_name` never existed in the
    // listing, so this used to match nothing at all.
    let device_sessions: Vec<(&serde_json::Value, String)> = sessions
        .iter()
        .filter(|s| match row_uuid(s, "device_client_key") {
            Some(key) => key == device_info.device_id,
            // A server older than 1922 has no device key; its plaintext
            // `device_name` was the only identity available.
            None => s.get("device_name").and_then(|v| v.as_str()) == Some(device_info.hostname.as_str()),
        })
        .map(|s| (s, stop_match_name(master, s).unwrap_or_else(|| HIDDEN_NAME.to_string())))
        .collect();

    if device_sessions.is_empty() {
        println!(
            "  {} {}",
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            "no active sessions on this device".custom_color(crate::colors::INK_DIM),
        );
        // Still stop any orphaned local daemons
        let _ = crate::daemon::stop_all_daemons();
        return Ok(());
    }

    let mut stopped = 0u32;
    for (session, name) in &device_sessions {
        let id = session.get("id").and_then(|v| v.as_str()).ok_or("session missing id")?;

        api.stop_client_session(id).await?;

        // Kill local daemon + remove auto-start
        let slug = crate::daemon::slugify(name);
        let _ = crate::daemon::stop_daemon(&slug);

        println!(
            "  {} stopped {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            name.custom_color(crate::colors::INK),
        );
        stopped += 1;
    }

    // Also stop any orphaned daemons (PID files with no matching server session)
    let _ = crate::daemon::stop_all_daemons();

    println!(
        "\n  {} {} session{} stopped on {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        stopped,
        if stopped == 1 { "" } else { "s" },
        device_info.hostname.custom_color(crate::colors::INK),
    );

    Ok(())
}
