use colored::Colorize;

use crate::api::ApiClient;

pub async fn run() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let me = api.get_me().await?;
    let region_info = api.get_region().await.ok();

    let email = me
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let user_id = me
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let device = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let region = region_info
        .as_ref()
        .and_then(|v| v.get("region").and_then(|r| r.as_str()))
        .unwrap_or("unknown");
    let jurisdiction = region_info
        .as_ref()
        .and_then(|v| v.get("jurisdiction").and_then(|j| j.as_str()))
        .unwrap_or("unknown");

    let dim = |s: &str| s.truecolor(106, 101, 91);

    println!(
        "  {} {}",
        dim("user   "),
        format!("{user_id} <{email}>").truecolor(233, 230, 221),
    );
    println!(
        "  {} {}",
        dim("device "),
        device.truecolor(208, 200, 154),
    );
    println!(
        "  {} {}",
        dim("region "),
        format!("{region} · {jurisdiction}").truecolor(245, 184, 0),
    );

    Ok(())
}
