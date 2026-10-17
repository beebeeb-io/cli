//! Storage-region choices (task 1919) — the pure, testable half.
//!
//! The list of places a user can store their files comes from the API
//! (`GET /api/v1/storage-regions`, mirrored as `choices` in
//! `GET /api/v1/me/region`). This client keeps NO list of its own: no static
//! id, city or country map. A datacenter that goes live, or "coming soon" that
//! turns live, shows up here without a CLI release.
//!
//! Model (Guus, 2026-10-17): "Europe" is the default — no specific country,
//! the server places the data in any of our European datacenters. Below it the
//! user may pick one datacenter. Datacenters that are not live yet are listed
//! as "coming soon" and cannot be selected.
//!
//! Item shape (bare array, server contract in task 1919):
//! `{"id","kind":"region"|"datacenter","name","city","country","country_code",
//!   "status":"live"|"coming_soon","selectable","is_default"}`.
//! The reader is lenient (unknown fields ignored, missing optional fields
//! tolerated) but never invents a row: an item without `id` and `name` is
//! dropped.

use serde_json::Value;

/// One row of the region list.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub city: Option<String>,
    pub country: Option<String>,
    pub status: String,
    pub selectable: bool,
    pub is_default: bool,
    /// The server's object, verbatim (what `--json` prints).
    pub raw: Value,
}

impl Choice {
    fn from_json(v: &Value) -> Option<Choice> {
        let s = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|x| !x.is_empty())
        };
        let id = s("id")?.to_string();
        let name = s("name")?.to_string();
        let status = s("status").unwrap_or("live").to_string();
        // `selectable` is authoritative; if an older server omits it, a live
        // row is selectable and anything else is not.
        let selectable = v
            .get("selectable")
            .and_then(|x| x.as_bool())
            .unwrap_or(status == "live");
        Some(Choice {
            id,
            kind: s("kind").unwrap_or("datacenter").to_string(),
            name,
            city: s("city").map(String::from),
            country: s("country").map(String::from),
            status,
            selectable,
            is_default: v.get("is_default").and_then(|x| x.as_bool()).unwrap_or(false),
            raw: v.clone(),
        })
    }

    pub fn is_coming_soon(&self) -> bool {
        self.status == "coming_soon"
    }

    /// The default/any row ("Europe"): a region, not one datacenter.
    pub fn is_any(&self) -> bool {
        self.kind == "region"
    }

    /// "Helsinki, Finland" for a datacenter, "Europe" for the any-row.
    pub fn display(&self) -> String {
        if self.is_any() {
            return self.name.clone();
        }
        match self.country.as_deref() {
            Some(c) => format!("{}, {c}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Reads a bare array, or an object carrying it under `choices` (the
/// `/me/region` response). Anything else is an empty list.
pub fn parse_choices(v: &Value) -> Vec<Choice> {
    let arr = v.as_array().or_else(|| v.get("choices").and_then(|c| c.as_array()));
    arr.map(|a| a.iter().filter_map(Choice::from_json).collect())
        .unwrap_or_default()
}

/// The id of the user's current choice. No stored preference means the
/// default: the any-row (`europe`).
pub fn current_id(me_region: &Value, choices: &[Choice]) -> String {
    match me_region.get("preferred_region").and_then(|v| v.as_str()) {
        Some(id) if !id.trim().is_empty() => id.trim().to_lowercase(),
        _ => choices
            .iter()
            .find(|c| c.is_default)
            .map(|c| c.id.clone())
            .unwrap_or_else(|| "europe".to_string()),
    }
}

/// How the current choice reads, from the API's own list.
///
/// * default/any row -> `Europe · default, any datacenter`
/// * a datacenter    -> `Helsinki, Finland`
/// * stored id that the list no longer carries (the datacenter is not live
///   right now) -> says so rather than guessing a name
/// * no list at all (older server) -> the raw id, capitalised
pub fn current_label(me_region: &Value) -> String {
    let choices = parse_choices(me_region);
    let id = current_id(me_region, &choices);
    match choices.iter().find(|c| c.id == id) {
        Some(c) if c.is_any() => format!("{} \u{00b7} default, any datacenter", c.name),
        Some(c) => c.display(),
        None if choices.is_empty() => capitalise(&id),
        None => format!("{id} (not available right now \u{00b7} new uploads go to another datacenter)"),
    }
}

/// Short name for machine output: `Europe`, `Helsinki`.
pub fn current_name(me_region: &Value) -> String {
    let choices = parse_choices(me_region);
    let id = current_id(me_region, &choices);
    choices
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| capitalise(&id))
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

/// Outcome of matching what the user typed against the API's list.
#[derive(Debug, PartialEq)]
pub enum Resolution<'a> {
    /// Exactly one selectable row.
    Found(&'a Choice),
    /// A row exists but cannot be chosen (coming soon, or not live now).
    NotSelectable(&'a Choice),
    /// More than one row matches; the ids are named so the user can retry.
    Ambiguous(Vec<&'a Choice>),
    /// Nothing in the list matches.
    Unknown,
}

/// Matches `input` case-insensitively against id first, then name, then city.
/// The first of those that matches anything decides, so `europe` can never be
/// shadowed by a city that happens to share a name.
pub fn resolve<'a>(choices: &'a [Choice], input: &str) -> Resolution<'a> {
    let want = input.trim().to_lowercase();
    if want.is_empty() {
        return Resolution::Unknown;
    }
    // id, then name, then city: the first column with any hit decides.
    for column in 0..3 {
        let key = |c: &Choice| -> Option<String> {
            match column {
                0 => Some(c.id.to_lowercase()),
                1 => Some(c.name.to_lowercase()),
                _ => c.city.as_ref().map(|x| x.to_lowercase()),
            }
        };
        let hits: Vec<&Choice> = choices
            .iter()
            .filter(|c| key(c).as_deref() == Some(want.as_str()))
            .collect();
        match hits.as_slice() {
            [] => continue,
            [one] if one.selectable => return Resolution::Found(one),
            [one] => return Resolution::NotSelectable(one),
            _ => return Resolution::Ambiguous(hits),
        }
    }
    Resolution::Unknown
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The shape prod serves once hel1 and nbg1 are loaded (task 1919
    /// contract). Cities and countries are fixture data here, never client
    /// constants.
    pub(crate) fn contract_list() -> Value {
        json!([
            {"id":"europe","kind":"region","name":"Europe","city":null,"country":null,"country_code":null,
             "status":"live","selectable":true,"is_default":true},
            {"id":"fsn1","kind":"datacenter","name":"Falkenstein","city":"Falkenstein","country":"Germany",
             "country_code":"DE","status":"live","selectable":true,"is_default":false},
            {"id":"hel1","kind":"datacenter","name":"Helsinki","city":"Helsinki","country":"Finland",
             "country_code":"FI","status":"live","selectable":true,"is_default":false},
            {"id":"nbg1","kind":"datacenter","name":"Nuremberg","city":"Nuremberg","country":"Germany",
             "country_code":"DE","status":"live","selectable":true,"is_default":false},
            {"id":"ede","kind":"datacenter","name":"Ede","city":"Ede","country":"Netherlands",
             "country_code":"NL","status":"coming_soon","selectable":false,"is_default":false}
        ])
    }

    #[test]
    fn parses_the_contract_list_in_server_order() {
        let c = parse_choices(&contract_list());
        let ids: Vec<&str> = c.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["europe", "fsn1", "hel1", "nbg1", "ede"]);
        assert!(c[0].is_any() && c[0].is_default && c[0].selectable);
        assert!(c[4].is_coming_soon() && !c[4].selectable);
        assert_eq!(c[2].display(), "Helsinki, Finland");
        assert_eq!(c[0].display(), "Europe");
    }

    #[test]
    fn me_region_choices_key_is_read_too() {
        let me = json!({"preferred_region": null, "available_regions": [], "choices": contract_list()});
        assert_eq!(parse_choices(&me).len(), 5);
    }

    #[test]
    fn rows_without_id_or_name_are_dropped_not_invented() {
        let v = json!([{"id":"hel1"}, {"name":"x"}, "junk", {"id":"a","name":"A"}]);
        let c = parse_choices(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "a");
        assert!(
            c[0].selectable,
            "status defaults to live, so selectable defaults to true"
        );
    }

    #[test]
    fn a_new_datacenter_needs_no_client_release() {
        // A city this binary has never heard of is listed, matched by name and
        // by id, and selectable — the only source is the payload.
        let v = json!([
            {"id":"europe","kind":"region","name":"Europe","status":"live","selectable":true,"is_default":true},
            {"id":"zzz9","kind":"datacenter","name":"Qwerty","city":"Qwerty","country":"Nowhere",
             "status":"live","selectable":true,"is_default":false}
        ]);
        let c = parse_choices(&v);
        assert!(matches!(resolve(&c, "qwerty"), Resolution::Found(x) if x.id == "zzz9"));
        assert!(matches!(resolve(&c, "ZZZ9"), Resolution::Found(x) if x.id == "zzz9"));
    }

    #[test]
    fn resolve_matches_id_name_and_city_case_insensitively() {
        let c = parse_choices(&contract_list());
        for typed in ["hel1", "Helsinki", "HELSINKI", "  helsinki "] {
            assert!(
                matches!(resolve(&c, typed), Resolution::Found(x) if x.id == "hel1"),
                "{typed}"
            );
        }
        assert!(matches!(resolve(&c, "europe"), Resolution::Found(x) if x.id == "europe"));
        assert!(matches!(resolve(&c, "Europe"), Resolution::Found(x) if x.id == "europe"));
    }

    #[test]
    fn coming_soon_is_found_but_not_selectable() {
        let c = parse_choices(&contract_list());
        assert!(matches!(resolve(&c, "ede"), Resolution::NotSelectable(x) if x.is_coming_soon()));
        assert!(matches!(resolve(&c, "Ede"), Resolution::NotSelectable(_)));
    }

    #[test]
    fn unknown_and_empty_input() {
        let c = parse_choices(&contract_list());
        assert_eq!(resolve(&c, "paris"), Resolution::Unknown);
        assert_eq!(resolve(&c, "  "), Resolution::Unknown);
        assert_eq!(resolve(&[], "europe"), Resolution::Unknown);
    }

    #[test]
    fn ambiguous_city_is_reported_not_guessed() {
        let v = json!([
            {"id":"a1","name":"A1","city":"Springfield","status":"live","selectable":true},
            {"id":"b2","name":"B2","city":"Springfield","status":"live","selectable":true}
        ]);
        let c = parse_choices(&v);
        assert!(matches!(resolve(&c, "springfield"), Resolution::Ambiguous(h) if h.len() == 2));
    }

    #[test]
    fn current_label_default_datacenter_missing_and_old_server() {
        let with = |pref: Value| json!({"preferred_region": pref, "choices": contract_list()});
        assert_eq!(
            current_label(&with(json!(null))),
            "Europe \u{00b7} default, any datacenter"
        );
        assert_eq!(
            current_label(&with(json!("europe"))),
            "Europe \u{00b7} default, any datacenter"
        );
        assert_eq!(current_label(&with(json!("hel1"))), "Helsinki, Finland");
        assert_eq!(current_name(&with(json!("hel1"))), "Helsinki");
        // Stored datacenter the list no longer carries: say so, never guess.
        let gone = current_label(&with(json!("xyz1")));
        assert!(gone.starts_with("xyz1 (not available right now"), "{gone}");
        // Older server: no `choices` key at all.
        assert_eq!(current_label(&json!({"preferred_region": "europe"})), "Europe");
        assert_eq!(current_label(&json!({"preferred_region": null})), "Europe");
    }
}
