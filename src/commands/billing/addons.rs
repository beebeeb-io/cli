//! `bb billing portal` and `bb billing addons`: open the billing portal and
//! buy extra storage through the catalog.
//!
//! Owns: `portal`, `addons`, `purchase_addon`, `print_addons`, and the catalog
//! price lookup.
//! Must not: render the plan view (see `billing::show`).

use super::*;

/// `bb billing portal` / `bb account billing` — manage billing in the web app.
/// Navigation must not initiate a payment-provider checkout or mandate change.
pub async fn portal() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let url = format!("{}/billing", crate::web_url::web_app_base());
    if !crate::env_detect::is_headless() {
        let _ = open::that(&url);
    }

    println!("Billing: {url}");
    Ok(())
}

pub async fn addons() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let addons = api.get_billing_addons().await?;
    print_addons(&addons);
    Ok(())
}

pub async fn purchase_addon(addon_id: String) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let addons = api.get_billing_addons().await?;
    let field = match addon_id.as_str() {
        "extra_storage_tb" => "extra_storage_tb",
        "extra_users" => "extra_users",
        _ => {
            return Err(
                "unknown ADDON_ID; use one of the server add-on fields: extra_storage_tb, extra_users".to_string(),
            );
        }
    };

    let current = addons
        .get(field)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("server did not return {field}"))?;
    let next = current
        .checked_add(1)
        .ok_or_else(|| format!("{field} is too large to increment"))?;

    let mut body = serde_json::Map::new();
    body.insert(field.to_string(), serde_json::json!(next));
    let response = api.update_billing_addons(Value::Object(body)).await?;
    let confirmed = response.get(field).and_then(|v| v.as_i64()).unwrap_or(next);

    println!("Add-on purchase confirmed: {field}={confirmed}");
    Ok(())
}

pub(super) fn print_addons(addons: &Value) {
    let field_width = ADDON_FIELDS.iter().map(|field| field.len()).max().unwrap_or(5);

    println!("{:<field_width$}  value", "field", field_width = field_width);
    for field in ADDON_FIELDS {
        let value = addons.get(field).map(format_json_value).unwrap_or_else(|| "-".into());
        println!("{:<field_width$}  {value}", field, field_width = field_width);
    }
}

/// Price for an arbitrary plan slug, read from the public plan catalog
/// (`GET /api/v1/billing/plans`). Used to price a *pending downgrade* plan,
/// whose amount isn't on the current subscription. Returns `None` when the slug
/// isn't in the catalog or the price is zero/free.
pub(super) fn price_from_catalog(plans: &Value, slug: &str, billing_cycle: &str) -> Option<String> {
    let list = plans.get("plans").and_then(|v| v.as_array())?;
    let plan = list
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(slug))?;
    let key = if billing_cycle == "yearly" {
        "price_yearly_eur"
    } else {
        "price_eur"
    };
    let eur = plan.get(key).and_then(|v| v.as_f64())?;
    let cents = (eur * 100.0).round() as i64;
    if cents <= 0 {
        return None;
    }
    Some(format_price(cents, billing_cycle))
}

/// Format an integer cents amount as `€X.XX / month` (or `/ year`).
pub(super) fn format_price(cents: i64, billing_cycle: &str) -> String {
    let period = if billing_cycle == "yearly" { "year" } else { "month" };
    format!("€{}.{:02} / {}", cents / 100, cents % 100, period)
}
