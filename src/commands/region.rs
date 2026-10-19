//! `bb region` — where new uploads are stored (task 1919, by COUNTRY since
//! task 2050).
//!
//! * `bb region` / `bb region list` — the API's list: the default "Europe" row
//!   (any of our European data centers, Beebeeb places the data), then one row
//!   per country. A country that is not live yet is shown as "coming soon" and
//!   cannot be chosen. Your current choice is marked when you are signed in.
//! * `bb region get` — just the current choice.
//! * `bb region set <europe|country>` — choose. Matches the API's list by id,
//!   then name, then country code, case-insensitively (`de`, `Germany`,
//!   `Europe`). A legacy datacenter id (`hel1`) is passed to the server as
//!   typed; it maps those to their country for one release. A city is not a
//!   choice any more.
//!
//! Routes: `GET /api/v1/storage-regions` (public), `GET`/`PUT
//! /api/v1/me/region`. The list is never kept in this binary; see
//! `crate::region`. The choice affects where NEW uploads go; files already
//! stored stay where they are. No city is ever printed.

use colored::Colorize;
use serde_json::{Value, json};

use crate::api::{ApiClient, ApiError};
use crate::colors;
use crate::region::{Choice, Resolution, current_id, current_label, parse_choices, resolve};
use crate::ui;

/// What the footer of the list says about existing files.
const EXISTING_FILES_NOTE: &str = "New uploads follow your choice. Files you already uploaded stay where they are.";

pub async fn list() -> Result<(), String> {
    let api = ApiClient::from_config();
    // The list is public. The current-choice marker needs a session: with
    // none, the list still prints; with one that fails, say so — the marker is
    // the point of being signed in, and a stale one would mislead.
    let signed_in = api.require_auth().is_ok();
    let (list_res, me_res) = tokio::join!(api.get_storage_regions(), async {
        if signed_in {
            Some(api.get_my_region().await)
        } else {
            None
        }
    });
    let choices = parse_choices(&list_res?);
    if choices.is_empty() {
        return Err("The server returned no storage regions.".to_string());
    }
    let me = match me_res {
        Some(r) => Some(r?),
        None => None,
    };
    let current = me.as_ref().map(|m| current_id(m, &choices));

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "current": current,
                "regions": choices.iter().map(|c| crate::region::without_city(&c.raw)).collect::<Vec<Value>>(),
            }))
            .unwrap()
        );
        return Ok(());
    }
    if ui::is_quiet() {
        for c in choices.iter().filter(|c| c.selectable) {
            println!("{}", c.id);
        }
        return Ok(());
    }

    print!("{}", render_list(&choices, current.as_deref(), signed_in));
    Ok(())
}

pub async fn get() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let me = api.get_my_region().await?;
    let choices = parse_choices(&me);
    let id = current_id(&me, &choices);
    let label = current_label(&me);
    let row = choices.iter().find(|c| c.id == id);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "preferred_region": me.get("preferred_region").cloned().unwrap_or(Value::Null),
                "current": id,
                "label": label,
                "country": row.and_then(|c| c.country.clone()),
                "country_code": row.and_then(|c| c.country_code.clone()),
            }))
            .unwrap()
        );
    } else if ui::is_quiet() {
        println!("{id}");
    } else {
        println!();
        println!(
            "  {} {}",
            "region  ".custom_color(colors::INK_DIM),
            label.custom_color(colors::INK)
        );
        println!("  {}", EXISTING_FILES_NOTE.custom_color(colors::INK_DIM));
        println!();
    }
    Ok(())
}

/// The server's code for "that region can't be chosen right now".
const REGION_UNAVAILABLE: &str = "region_unavailable";

/// True when a `PUT /api/v1/me/region` refusal means the region is not
/// choosable right now (cli#84 Codex thread). Matched on the stable code
/// first: the typed two-field contract puts `region_unavailable` in `error`
/// alone and a human `message` that need not mention it. The server today
/// sends `{"error": "region_unavailable: '<id>' is not available"}` (a plain
/// `BadRequest`, no `message`), so the code also counts when it carries that
/// detail after a colon; a `message` with the same prefix is the last resort.
/// Prefix, not substring: an unrelated error that merely mentions the word is
/// shown as the server wrote it.
fn is_region_unavailable(e: &ApiError) -> bool {
    let prefixed = |s: &str| {
        s.strip_prefix(REGION_UNAVAILABLE)
            .is_some_and(|rest| rest.starts_with(':'))
    };
    e.is_code(REGION_UNAVAILABLE) || e.code.as_deref().is_some_and(prefixed) || prefixed(&e.message)
}

pub async fn set(target: String) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let choices = parse_choices(&api.get_storage_regions().await?);
    if choices.is_empty() {
        return Err("The server returned no storage regions, so nothing can be chosen.".to_string());
    }

    // Decide against the API's list BEFORE sending anything: a coming-soon or
    // unknown name never reaches the server. The one exception is a legacy
    // datacenter id (`hel1`), which the server maps for one release.
    let (wire_id, chosen) = match resolve(&choices, &target) {
        Resolution::Found(c) => (c.wire_id_for(&target), Some(c)),
        Resolution::NotSelectable(c) if c.is_coming_soon() => {
            return Err(format!(
                "{} is coming soon and can't be chosen yet. Your choice is unchanged.",
                c.display()
            ));
        }
        Resolution::NotSelectable(c) => {
            return Err(format!(
                "{} is not available right now. Your choice is unchanged. See `bb region list`.",
                c.display()
            ));
        }
        Resolution::Ambiguous(hits) => {
            let ids: Vec<&str> = hits.iter().map(|c| c.id.as_str()).collect();
            return Err(format!(
                "'{target}' matches more than one location ({}). Use the id from `bb region list`.",
                ids.join(", ")
            ));
        }
        Resolution::LegacyId(id) => (id, None),
        Resolution::Unknown => return Err(unknown_message(&target, &choices)),
    };
    let asked = chosen.map(|c| c.display()).unwrap_or_else(|| wire_id.clone());

    // The id goes out exactly as the list gave it.
    let resp = api.set_my_region(Some(&wire_id)).await.map_err(|e| {
        if is_region_unavailable(&e) {
            // Lost a race with the list (a country went away between the two
            // calls), or a legacy id the server no longer knows. The stored
            // value did not change.
            format!("{asked} isn't available right now. Your choice is unchanged. See `bb region list`.")
        } else {
            e.message
        }
    })?;
    let stored = resp
        .get("preferred_region")
        .and_then(|v| v.as_str())
        .unwrap_or(&wire_id)
        .to_string();
    // What the server stored, as the list names it (a legacy id the server
    // mapped reads as its country).
    let stored_row = choices.iter().find(|c| c.is_stored_value(&stored));
    let name = stored_row
        .or(chosen)
        .map(|c| c.display())
        .unwrap_or_else(|| stored.clone());
    let is_any = stored_row.or(chosen).is_some_and(|c| c.is_any());
    let row = stored_row.or(chosen);

    if ui::is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "preferred_region": stored,
                "name": name,
                "country": row.and_then(|c| c.country.clone()),
                "country_code": row.and_then(|c| c.country_code.clone()),
            }))
            .unwrap()
        );
    } else if ui::is_quiet() {
        println!("{stored}");
    } else {
        println!();
        println!(
            "  {} {}",
            "\u{2713}".custom_color(colors::GREEN_OK),
            format!("Storage location set to {name}.").custom_color(colors::INK)
        );
        let how = if is_any {
            "Beebeeb places new uploads in any of its European data centers.".to_string()
        } else {
            format!("New uploads are stored in {name}.")
        };
        println!("  {}", how.custom_color(colors::INK_DIM));
        println!(
            "  {}",
            "Files you already uploaded stay where they are.".custom_color(colors::INK_DIM)
        );
        println!();
    }
    Ok(())
}

fn unknown_message(target: &str, choices: &[Choice]) -> String {
    let options: Vec<String> = choices
        .iter()
        .filter(|c| c.selectable)
        .map(|c| {
            if c.is_any() {
                c.id.clone()
            } else {
                format!("{} ({})", c.id, c.name)
            }
        })
        .collect();
    // Two real country ids for the hint, taken from the list itself.
    let tries: Vec<String> = choices
        .iter()
        .filter(|c| c.selectable && !c.is_any())
        .take(2)
        .map(|c| format!("'{}'", c.id))
        .collect();
    let hint = if tries.is_empty() {
        String::new()
    } else {
        format!(" Try {}.", tries.join(" or "))
    };
    format!(
        "No storage location matches '{target}'. Storage is chosen by country now.{hint} Choose one of: {}. See `bb region list`.",
        options.join(", ")
    )
}

/// The human list. Pure so tests can assert on the text.
pub(crate) fn render_list(choices: &[Choice], current: Option<&str>, signed_in: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let id_w = choices.iter().map(|c| c.id.len()).max().unwrap_or(4).max(4);
    let name_w = choices.iter().map(|c| c.name.chars().count()).max().unwrap_or(6).max(6);

    let _ = writeln!(out);
    let _ = writeln!(out, "  {}", "STORAGE LOCATION".custom_color(colors::AMBER));
    let _ = writeln!(out, "  {}", EXISTING_FILES_NOTE.custom_color(colors::INK_DIM));
    let _ = writeln!(out);
    for c in choices {
        let marker = if current == Some(c.id.as_str()) {
            "\u{25cf}".custom_color(colors::AMBER).to_string()
        } else {
            " ".to_string()
        };
        let detail = if c.is_any() {
            let mut d = String::from("any of our European data centers, we place your files");
            if c.is_default {
                d.push_str(" \u{00b7} default");
            }
            d
        } else if c.is_coming_soon() {
            "coming soon \u{00b7} not selectable".to_string()
        } else if !c.selectable {
            "not available right now".to_string()
        } else {
            String::new()
        };
        let dim_row = c.is_coming_soon() || !c.selectable;
        let name = format!("{:<name_w$}", c.name);
        let line = format!(
            "  {marker} {}  {}  {}",
            format!("{:<id_w$}", c.id).custom_color(colors::INK_DIM),
            if dim_row {
                name.custom_color(colors::INK_DIM)
            } else {
                name.custom_color(colors::INK)
            },
            detail.custom_color(colors::INK_DIM),
        );
        let _ = writeln!(out, "{}", line.trim_end());
    }
    let _ = writeln!(out);
    if signed_in {
        // The example is taken from the list itself, never a fixed country.
        let example = choices
            .iter()
            .find(|c| c.selectable && !c.is_any())
            .map(|c| c.id.clone())
            .unwrap_or_else(|| "europe".to_string());
        let _ = writeln!(
            out,
            "  {}",
            format!("Change it with `bb region set <id or country>`, for example `bb region set {example}`.")
                .custom_color(colors::INK_SAGE)
        );
    } else {
        let _ = writeln!(
            out,
            "  {}",
            "Sign in with `bb login` to see and change your choice.".custom_color(colors::INK_SAGE)
        );
    }
    let _ = writeln!(out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::parse_choices;

    fn api_error(code: Option<&str>, message: &str) -> ApiError {
        ApiError {
            code: code.map(String::from),
            message: message.to_string(),
            status: 400,
        }
    }

    #[test]
    fn region_unavailable_is_recognised_by_its_code_in_every_wire_shape() {
        // Typed contract: the code alone, a message that does not mention it.
        assert!(is_region_unavailable(&api_error(
            Some("region_unavailable"),
            "That datacenter can't take new uploads right now."
        )));
        // What the server sends today: the detail glued onto `error`.
        let today = "region_unavailable: 'hel1' is not available";
        assert!(is_region_unavailable(&api_error(Some(today), today)));
        // A generic code with the detail in `message`.
        assert!(is_region_unavailable(&api_error(Some("bad_request"), today)));
    }

    #[test]
    fn other_refusals_are_not_mistaken_for_region_unavailable() {
        assert!(!is_region_unavailable(&api_error(Some("unauthorized"), "unauthorized")));
        assert!(!is_region_unavailable(&api_error(
            Some("bad_request"),
            "preferred_region must be a string; region_unavailable is not the issue"
        )));
        assert!(!is_region_unavailable(&api_error(
            Some("region_unavailable_soon"),
            "something else"
        )));
        assert!(!is_region_unavailable(&api_error(None, "failed to read response")));
    }

    fn list() -> Vec<Choice> {
        parse_choices(&json!([
            {"id":"europe","kind":"region","name":"Europe","status":"live","selectable":true,"is_default":true},
            {"id":"de","kind":"country","name":"Germany","country":"Germany","country_code":"DE",
             "status":"live","selectable":true,"is_default":false},
            {"id":"fi","kind":"country","name":"Finland","country":"Finland","country_code":"FI",
             "status":"live","selectable":true,"is_default":false},
            {"id":"nl","kind":"country","name":"Netherlands","country":"Netherlands","country_code":"NL",
             "status":"coming_soon","selectable":false,"is_default":false}
        ]))
    }

    #[test]
    fn list_marks_the_current_choice_and_coming_soon() {
        colored::control::set_override(false);
        let out = render_list(&list(), Some("fi"), true);
        let line = |id: &str| {
            out.lines()
                .find(|l| l.split_whitespace().any(|w| w == id))
                .unwrap()
                .to_string()
        };
        assert!(line("fi").contains('\u{25cf}'), "{out}");
        assert!(!line("de").contains('\u{25cf}'), "{out}");
        assert!(line("europe").contains("default"), "{out}");
        assert!(line("europe").contains("European data centers"), "{out}");
        assert!(
            line("nl").contains("coming soon") && line("nl").contains("not selectable"),
            "{out}"
        );
        assert!(out.contains("stay where they are"), "{out}");
        assert!(out.contains("bb region set"), "{out}");
    }

    #[test]
    fn list_signed_out_has_no_marker_and_says_how_to_sign_in() {
        colored::control::set_override(false);
        let out = render_list(&list(), None, false);
        assert!(!out.contains('\u{25cf}'), "{out}");
        assert!(out.contains("bb login"), "{out}");
    }

    #[test]
    fn copy_never_names_the_provider() {
        colored::control::set_override(false);
        let out = render_list(&list(), Some("europe"), true).to_lowercase();
        for banned in ["hetzner", "leaseweb", "aws", "amazon", "cloudflare"] {
            assert!(!out.contains(banned), "{banned} in {out}");
        }
    }

    #[test]
    fn unknown_message_lists_only_selectable_choices_from_the_api() {
        let m = unknown_message("paris", &list());
        assert!(m.contains("'paris'"), "{m}");
        assert!(m.contains("fi (Finland)") && m.contains("europe"), "{m}");
        assert!(m.contains("chosen by country") && m.contains("'de' or 'fi'"), "{m}");
        assert!(!m.contains("Netherlands"), "coming soon is not an option: {m}");
    }
}
