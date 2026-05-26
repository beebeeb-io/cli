//! `bb account` — profile show, email change, export, delete.
//!
//! Routes used:
//!   GET    /api/v1/auth/me
//!   GET    /api/v1/billing/subscription
//!   GET    /api/v1/account/security-score   (note: /account, not /auth/account)
//!   GET    /api/v1/account/sessions
//!   GET    /api/v1/auth/passkeys
//!   GET    /api/v1/me/region
//!
//!   POST   /api/v1/me/email/start            (OPAQUE)
//!   POST   /api/v1/me/email/finish           (OPAQUE)
//!   PUT    /api/v1/auth/account/email        (legacy password accounts)
//!
//!   POST   /api/v1/auth/account/export
//!   GET    /api/v1/auth/account/export/{id}
//!   GET    /api/v1/auth/account/export/{id}/download
//!
//!   DELETE /api/v1/auth/account
//!
//! All mutating endpoints require X-Confirm-Token from POST /api/v1/auth/confirm.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::api::ApiClient;

/// Build the canonical `bb account show --json` payload from the raw API responses.
/// Each input is `Result<Value, String>` so the caller can pass the join! tuple
/// directly. Missing sections are represented as `{"unavailable": "<error>"}`.
fn build_show_payload(
    me: &Result<Value, String>,
    sub: &Result<Value, String>,
    region: &Result<Value, String>,
    score: &Result<Value, String>,
    sessions: &Result<Value, String>,
    passkeys: &Result<Value, String>,
) -> Value {
    let unavailable = |e: &String| json!({ "unavailable": e });

    let user = match me {
        Ok(v) => json!({
            "id": v.get("user_id").or_else(|| v.get("id")),
            "email": v.get("email"),
            "email_verified": v.get("email_verified"),
            "created_at": v.get("created_at"),
        }),
        Err(e) => unavailable(e),
    };

    let plan = match sub {
        Ok(v) => json!({
            "id": v.get("plan"),
            "billing_cycle": v.get("billing_cycle"),
            "current_period_end": v.get("current_period_end"),
            "billing_state": v.get("billing_state"),
            "pending_downgrade_plan": v.get("pending_downgrade_plan"),
            "stripe_configured": v.get("stripe_configured"),
        }),
        Err(e) => unavailable(e),
    };

    let region_v = match region {
        Ok(v) => v.clone(),
        Err(e) => unavailable(e),
    };

    let security = match score {
        Ok(v) => v.clone(),
        Err(e) => unavailable(e),
    };

    // Storage comes from /billing/subscription (it includes used_bytes / quota_bytes)
    let storage = match sub {
        Ok(v) => {
            let used = v.get("used_bytes").and_then(|x| x.as_i64()).unwrap_or(0);
            let quota = v.get("quota_bytes").and_then(|x| x.as_i64()).unwrap_or(0);
            let pct = if quota > 0 { used as f64 / quota as f64 } else { 0.0 };
            json!({
                "used_bytes": used,
                "quota_bytes": quota,
                "percentage": pct,
            })
        }
        Err(e) => unavailable(e),
    };

    let session_count = sessions
        .as_ref()
        .ok()
        .and_then(|v| v.get("sessions"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let passkey_count = passkeys
        .as_ref()
        .ok()
        .and_then(|v| v.get("passkeys"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    json!({
        "user": user,
        "plan": plan,
        "region": region_v,
        "storage": storage,
        "security": security,
        "session_count": session_count,
        "passkey_count": passkey_count,
    })
}

pub async fn show() -> Result<(), String> {
    use crate::{colors, ui};
    use beebeeb_types::quota::format_storage_si;
    use colored::Colorize;

    let api = ApiClient::from_config();
    let _ = api.require_auth()?;

    let (me, sub, region, score, sessions, passkeys) = tokio::join!(
        api.get_me(),
        api.get_subscription(),
        api.get_my_region(),
        api.security_score(),
        api.list_sessions_v2(),
        api.list_passkeys(),
    );

    let payload = build_show_payload(&me, &sub, &region, &score, &sessions, &passkeys);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    // ── Human view ─────────────────────────────────────────────────────────
    let email = payload["user"]["email"].as_str().unwrap_or("unknown");
    let created = payload["user"]["created_at"].as_str().unwrap_or("");
    let created_rel = if created.is_empty() {
        String::new()
    } else {
        format!("  {}", ui::relative_time(created))
    };

    let w = 58;
    println!("{}", ui::box_header("BEEBEEB ACCOUNT", w));
    println!("{}", ui::box_line(&email.custom_color(colors::INK).to_string(), w));
    if !created_rel.is_empty() {
        println!(
            "{}",
            ui::box_line(
                &format!("joined{}", created_rel)
                    .custom_color(colors::INK_DIM)
                    .to_string(),
                w
            ),
        );
    }
    println!("{}", ui::box_footer(w));
    println!();

    // PLAN
    println!("  {}", "PLAN".custom_color(colors::AMBER));
    let plan_id = payload["plan"]["id"].as_str().unwrap_or("free");
    let cycle = payload["plan"]["billing_cycle"].as_str().unwrap_or("monthly");
    let period_end = payload["plan"]["current_period_end"].as_str().unwrap_or("");
    let billing_state = payload["plan"]["billing_state"].as_str().unwrap_or("active");
    let pending = payload["plan"]["pending_downgrade_plan"].as_str();

    println!(
        "  {} \u{00b7} {} cycle",
        plan_id.custom_color(colors::INK),
        cycle.custom_color(colors::INK_DIM)
    );
    if !period_end.is_empty() {
        println!(
            "  renews {}",
            ui::relative_time(period_end).custom_color(colors::INK_DIM)
        );
    }
    let city = payload["region"]["city"].as_str().unwrap_or("");
    let provider = payload["region"]["provider"].as_str().unwrap_or("");
    if !city.is_empty() {
        println!(
            "  region   europe \u{00b7} {} \u{00b7} {}",
            city.to_lowercase().custom_color(colors::INK_DIM),
            provider.to_lowercase().custom_color(colors::INK_DIM)
        );
    }
    if billing_state == "active" {
        println!("  status   {}", "active".custom_color(colors::GREEN_OK));
    } else {
        println!("  status   {}", billing_state.custom_color(colors::RED_ERR));
    }
    if let Some(p) = pending {
        println!(
            "  {}",
            format!("switching to {p} at period end").custom_color(colors::AMBER)
        );
    }
    println!();

    // STORAGE
    println!("  {}", "STORAGE".custom_color(colors::AMBER));
    let used = payload["storage"]["used_bytes"].as_i64().unwrap_or(0);
    let quota = payload["storage"]["quota_bytes"].as_i64().unwrap_or(0);
    let bar = render_progress_bar(used, quota, 20);
    println!(
        "  {}  {} / {}  ({:.0}%)",
        bar,
        format_storage_si(used),
        format_storage_si(quota),
        if quota > 0 {
            used as f64 / quota as f64 * 100.0
        } else {
            0.0
        }
    );
    println!();

    // SECURITY
    println!(
        "  {}                                          score {}/{} {}",
        "SECURITY".custom_color(colors::AMBER),
        payload["security"]["score"],
        payload["security"]["max"],
        payload["security"]["label"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .custom_color(colors::INK_DIM)
    );

    if let Some(factors) = payload["security"]["factors"].as_array() {
        for f in factors {
            let key = f["key"].as_str().unwrap_or("");
            let ok = f["satisfied"].as_bool().unwrap_or(false);
            let label = match key {
                "email_verified" => "email verified",
                "phrase_saved" => "recovery phrase saved",
                "two_factor_enabled" => "2fa enabled",
                "phrase_recently_tested" => "recovery phrase tested in last 90d",
                "all_devices_recognized" => "all sessions on recognised devices",
                other => other,
            };
            if ok {
                println!(
                    "  {} {}",
                    "ok".custom_color(colors::GREEN_OK),
                    label.custom_color(colors::INK_DIM)
                );
            } else {
                println!(
                    "  {} {}",
                    "!!".custom_color(colors::RED_ERR),
                    label.custom_color(colors::INK_DIM)
                );
            }
        }
    }
    println!();

    // SESSIONS + PASSKEYS summary
    println!(
        "  {}  {} active \u{00b7} {} passkeys registered",
        "SESSIONS".custom_color(colors::AMBER),
        payload["session_count"],
        payload["passkey_count"]
    );

    Ok(())
}

/// 20-cell unicode progress bar. Filled cells = block, empty = light shade.
fn render_progress_bar(used: i64, quota: i64, width: usize) -> String {
    if quota <= 0 || used < 0 {
        return "\u{2591}".repeat(width);
    }
    let pct = (used as f64 / quota as f64).clamp(0.0, 1.0);
    let filled = (pct * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("{}{}", "\u{2593}".repeat(filled), "\u{2591}".repeat(empty))
}

pub async fn update_email(_new_email: String) -> Result<(), String> {
    Err("bb account update — not implemented yet".to_string())
}

pub async fn export_start() -> Result<(), String> {
    Err("bb account export — not implemented yet".to_string())
}

pub async fn export_status(_job_id: String) -> Result<(), String> {
    Err("bb account export status — not implemented yet".to_string())
}

pub async fn export_download(_job_id: String, _output: Option<PathBuf>) -> Result<(), String> {
    Err("bb account export download — not implemented yet".to_string())
}

pub async fn delete(_confirm: String) -> Result<(), String> {
    Err("bb account delete — not implemented yet".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_show_payload_assembles_all_sections() {
        let me = Ok(json!({
            "user_id": "u1", "email": "a@b.com", "email_verified": true,
            "created_at": "2026-01-14T00:00:00Z"
        }));
        let sub = Ok(json!({
            "plan": "pro", "billing_cycle": "monthly",
            "current_period_end": "2026-06-22T00:00:00Z",
            "billing_state": "active", "pending_downgrade_plan": null,
            "stripe_configured": true,
            "used_bytes": 3_200_000_000_000i64, "quota_bytes": 5_000_000_000_000i64
        }));
        let region = Ok(json!({
            "preferred_region": "europe", "city": "Falkenstein", "provider": "Hetzner"
        }));
        let score = Ok(json!({ "score": 4, "max": 5, "label": "Strong", "factors": [] }));
        let sessions = Ok(json!({ "sessions": [{"id": "s1"}, {"id": "s2"}, {"id": "s3"}] }));
        let passkeys = Ok(json!({ "passkeys": [{"id": "p1"}, {"id": "p2"}] }));

        let out = build_show_payload(&me, &sub, &region, &score, &sessions, &passkeys);
        assert_eq!(out["user"]["email"], "a@b.com");
        assert_eq!(out["plan"]["id"], "pro");
        assert_eq!(out["region"]["city"], "Falkenstein");
        assert_eq!(out["storage"]["used_bytes"], 3_200_000_000_000i64);
        assert_eq!(out["session_count"], 3);
        assert_eq!(out["passkey_count"], 2);
        assert!((out["storage"]["percentage"].as_f64().unwrap() - 0.64).abs() < 1e-6);
    }

    #[test]
    fn build_show_payload_degrades_gracefully_per_section() {
        let me = Ok(json!({ "email": "a@b.com" }));
        let bad = Err::<Value, String>("network: connection refused".to_string());
        let sub_ok = Ok(json!({ "plan": "free", "used_bytes": 0, "quota_bytes": 5_000_000_000i64 }));

        let out = build_show_payload(&me, &sub_ok, &bad, &bad, &bad, &bad);
        assert_eq!(out["user"]["email"], "a@b.com");
        assert_eq!(out["region"]["unavailable"], "network: connection refused");
        assert_eq!(out["security"]["unavailable"], "network: connection refused");
        assert_eq!(out["session_count"], 0);
        assert_eq!(out["passkey_count"], 0);
    }
}
