//! Storage-location choices (task 1919, by COUNTRY since task 2050) — the pure,
//! testable half.
//!
//! The list of places a user can store their files comes from the API
//! (`GET /api/v1/storage-regions`, mirrored as `choices` in
//! `GET /api/v1/me/region`). This client keeps NO list of its own: no static
//! id or country map. A country that goes live, or "coming soon" that turns
//! live, shows up here without a CLI release.
//!
//! Model (Guus, 2026-10-19): the user-facing unit is a COUNTRY, never a city.
//! "Europe" is the default: any of our European data centers, the server
//! places the data. Below it the user picks a country. A country that is not
//! live yet is listed "coming soon" and cannot be selected.
//!
//! Item shape (server contract, task 2050), a bare array:
//! `{"id":"europe"|"de"|"fi"|"nl","kind":"region"|"country","name","country",
//!   "country_code","status":"live"|"coming_soon","selectable","is_default"}`.
//! There is no city key.
//!
//! **Older servers** (before task 2050) send one `datacenter` row per city.
//! Those are FOLDED by country (`country_code`) into one row per country, so
//! the output is the same and no city is ever read, let alone printed. A
//! folded row remembers its datacenter ids (`aliases`): an older server only
//! stores those, so a choice made on one is sent as the first selectable
//! datacenter id, and a stored datacenter id reads as its country.
//!
//! The reader is lenient (unknown fields ignored, missing optional fields
//! tolerated) but never invents a row: an item without `id` and `name` is
//! dropped, and a `datacenter` row with no country at all is dropped rather
//! than shown under a city name.

use serde_json::{Value, json};

/// One row of the location list.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What `PUT /me/region` takes: `europe`, or a lower-case country code.
    pub id: String,
    pub kind: String,
    /// The display name: `Europe`, or the country.
    pub name: String,
    pub country: Option<String>,
    pub country_code: Option<String>,
    pub status: String,
    pub selectable: bool,
    pub is_default: bool,
    /// Datacenter ids folded into this row (older servers only). Lower case.
    pub aliases: Vec<String>,
    /// What goes on the wire for this row when the user did not type an
    /// alias: the list id, or (folded row) the first selectable datacenter id.
    wire_id: String,
    /// What `--json` prints for the row: the server's object verbatim, or the
    /// synthetic country row built from a fold.
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
            wire_id: id.clone(),
            id,
            kind: s("kind").unwrap_or("country").to_string(),
            name,
            country: s("country").map(String::from),
            country_code: s("country_code").map(String::from),
            status,
            selectable,
            is_default: v.get("is_default").and_then(|x| x.as_bool()).unwrap_or(false),
            aliases: Vec::new(),
            raw: v.clone(),
        })
    }

    pub fn is_coming_soon(&self) -> bool {
        self.status == "coming_soon"
    }

    /// The default/any row ("Europe"): a region, not one country.
    pub fn is_any(&self) -> bool {
        self.kind == "region"
    }

    /// "Europe" for the any-row, the country otherwise. Never a city.
    pub fn display(&self) -> String {
        self.name.clone()
    }

    /// The id to send for what the user typed: the exact datacenter id when
    /// they typed one this row folds (an older server stores those), else the
    /// row's own id.
    pub fn wire_id_for(&self, typed: &str) -> String {
        let typed = typed.trim().to_lowercase();
        if self.aliases.contains(&typed) {
            typed
        } else {
            self.wire_id.clone()
        }
    }

    /// Does a stored value (`preferred_region`) name this row?
    pub fn is_stored_value(&self, stored: &str) -> bool {
        let stored = stored.trim().to_lowercase();
        self.id.to_lowercase() == stored || self.aliases.contains(&stored)
    }
}

/// Folds one older-server `datacenter` row into `acc`: the first row of a
/// country makes the country row, later ones only add their id and may make it
/// live. A datacenter row with no country at all is dropped (its name is a
/// city).
fn fold_datacenter(acc: &mut Vec<Choice>, row: Choice) {
    let (code, country) = match (&row.country_code, &row.country) {
        (Some(code), country) => (code.clone(), country.clone()),
        (None, Some(country)) => (country.clone(), Some(country.clone())),
        (None, None) => return,
    };
    let key = code.to_lowercase();
    let name = country.clone().unwrap_or_else(|| code.clone());
    let alias = row.id.to_lowercase();
    if let Some(existing) = acc.iter_mut().find(|c| !c.is_any() && c.id == key) {
        existing.aliases.push(alias);
        let better = |status: &str| match status {
            "live" => 2,
            "coming_soon" => 1,
            _ => 0,
        };
        if row.selectable && !existing.selectable {
            existing.selectable = true;
            existing.status = row.status.clone();
            existing.wire_id = row.id.clone();
        } else if !existing.selectable && better(&row.status) > better(&existing.status) {
            existing.status = row.status.clone();
        }
        return;
    }
    acc.push(Choice {
        id: key,
        kind: "country".to_string(),
        name: name.clone(),
        country: Some(name),
        country_code: row.country_code.clone(),
        status: row.status.clone(),
        selectable: row.selectable,
        is_default: false,
        aliases: vec![alias],
        wire_id: row.id.clone(),
        raw: Value::Null,
    });
}

/// The row `--json` prints for a folded country.
fn synthetic_raw(c: &Choice) -> Value {
    json!({
        "id": c.id,
        "kind": "country",
        "name": c.name,
        "country": c.country,
        "country_code": c.country_code,
        "status": c.status,
        "selectable": c.selectable,
        "is_default": false,
    })
}

/// Deep copy of a server payload with every deprecated city key removed
/// (`city`, `example_city`). The server keeps `available_regions[].example_city`
/// one more release for the desktop; `--json` output must never carry it.
pub fn without_city(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(k, _)| k.as_str() != "city" && k.as_str() != "example_city")
                .map(|(k, x)| (k.clone(), without_city(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(without_city).collect()),
        other => other.clone(),
    }
}

/// Reads a bare array, or an object carrying it under `choices` (the
/// `/me/region` response). Anything else is an empty list. Older-server
/// datacenter rows are folded by country (see the module docs).
pub fn parse_choices(v: &Value) -> Vec<Choice> {
    let Some(arr) = v.as_array().or_else(|| v.get("choices").and_then(|c| c.as_array())) else {
        return Vec::new();
    };
    let mut out: Vec<Choice> = Vec::new();
    for item in arr {
        let Some(choice) = Choice::from_json(item) else {
            continue;
        };
        if choice.kind == "datacenter" {
            fold_datacenter(&mut out, choice);
        } else {
            out.push(choice);
        }
    }
    for c in out.iter_mut().filter(|c| c.raw.is_null()) {
        c.raw = synthetic_raw(c);
    }
    out
}

/// The id of the user's current choice. No stored preference means the
/// default: the any-row (`europe`). A stored datacenter id that a folded row
/// carries (older server) reads as that row's country.
pub fn current_id(me_region: &Value, choices: &[Choice]) -> String {
    match me_region.get("preferred_region").and_then(|v| v.as_str()) {
        Some(stored) if !stored.trim().is_empty() => choices
            .iter()
            .find(|c| c.is_stored_value(stored))
            .map(|c| c.id.clone())
            .unwrap_or_else(|| stored.trim().to_lowercase()),
        _ => choices
            .iter()
            .find(|c| c.is_default)
            .map(|c| c.id.clone())
            .unwrap_or_else(|| "europe".to_string()),
    }
}

/// How the current choice reads, from the API's own list.
///
/// * default/any row -> `Europe · default`
/// * a country       -> `Finland`
/// * stored id that the list no longer carries (the country is not live right
///   now) -> says so rather than guessing a name
/// * no list at all (older server) -> the raw id, capitalised
pub fn current_label(me_region: &Value) -> String {
    let choices = parse_choices(me_region);
    let id = current_id(me_region, &choices);
    match choices.iter().find(|c| c.id == id) {
        Some(c) if c.is_any() => format!("{} \u{00b7} default", c.name),
        Some(c) => c.display(),
        None if choices.is_empty() => capitalise(&id),
        None => format!("{id} (not available right now \u{00b7} new uploads go to another data center)"),
    }
}

/// Short name for machine output: `Europe`, `Finland`.
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
    /// Not in the list, but shaped like a datacenter id (`hel1`): sent to the
    /// server as typed. The server maps the legacy ids to their country for
    /// one release and answers `region_unavailable` for anything else.
    LegacyId(String),
    /// Nothing in the list matches (a city lands here).
    Unknown,
}

/// A datacenter id is 2 to 6 letters then 1 or 2 digits (`fsn1`, `hel1`, `ede2`).
/// Recognised by shape, so no id list is kept in this binary; a city never
/// has digits.
fn looks_like_datacenter_id(s: &str) -> bool {
    let letters = s.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let digits = s.len() - letters;
    (2..=6).contains(&letters) && (1..=2).contains(&digits) && s.chars().skip(letters).all(|c| c.is_ascii_digit())
}

/// Matches `input` case-insensitively against id first, then name, then
/// country code, then a folded datacenter id. The first of those that matches
/// anything decides, so `europe` can never be shadowed by a country that
/// happens to share a name.
pub fn resolve<'a>(choices: &'a [Choice], input: &str) -> Resolution<'a> {
    let want = input.trim().to_lowercase();
    if want.is_empty() {
        return Resolution::Unknown;
    }
    for column in 0..4 {
        let hits: Vec<&Choice> = choices
            .iter()
            .filter(|c| match column {
                0 => c.id.to_lowercase() == want,
                1 => c.name.to_lowercase() == want,
                2 => c.country_code.as_ref().is_some_and(|x| x.to_lowercase() == want),
                _ => c.aliases.contains(&want),
            })
            .collect();
        match hits.as_slice() {
            [] => continue,
            [one] if one.selectable => return Resolution::Found(one),
            [one] => return Resolution::NotSelectable(one),
            _ => return Resolution::Ambiguous(hits),
        }
    }
    if looks_like_datacenter_id(&want) {
        return Resolution::LegacyId(input.trim().to_string());
    }
    Resolution::Unknown
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The shape server #251 serves (task 2050 contract): countries, no city.
    /// Countries are fixture data here, never client constants.
    pub(crate) fn contract_list() -> Value {
        json!([
            {"id":"europe","kind":"region","name":"Europe","country":null,"country_code":null,
             "status":"live","selectable":true,"is_default":true},
            {"id":"de","kind":"country","name":"Germany","country":"Germany","country_code":"DE",
             "status":"live","selectable":true,"is_default":false},
            {"id":"fi","kind":"country","name":"Finland","country":"Finland","country_code":"FI",
             "status":"live","selectable":true,"is_default":false},
            {"id":"nl","kind":"country","name":"Netherlands","country":"Netherlands","country_code":"NL",
             "status":"coming_soon","selectable":false,"is_default":false}
        ])
    }

    /// What a server before task 2050 serves: one datacenter row per city.
    pub(crate) fn legacy_list() -> Value {
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
        assert_eq!(ids, ["europe", "de", "fi", "nl"]);
        assert!(c[0].is_any() && c[0].is_default && c[0].selectable);
        assert!(c[3].is_coming_soon() && !c[3].selectable);
        assert_eq!(c[2].display(), "Finland");
        assert_eq!(c[2].country_code.as_deref(), Some("FI"));
        assert_eq!(c[0].display(), "Europe");
        assert_eq!(c[2].raw, contract_list()[2], "rows are the server's, verbatim");
    }

    #[test]
    fn older_server_datacenter_rows_fold_into_one_row_per_country() {
        let c = parse_choices(&legacy_list());
        let ids: Vec<&str> = c.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["europe", "de", "fi", "nl"], "two German datacenters, one row");
        let de = &c[1];
        assert_eq!(de.display(), "Germany");
        assert_eq!(de.aliases, ["fsn1", "nbg1"]);
        assert!(de.selectable && !de.is_coming_soon());
        assert!(c[3].is_coming_soon() && !c[3].selectable);
        // No row, name or JSON object carries a city.
        for row in &c {
            let text = format!("{} {} {}", row.name, row.display(), row.raw).to_lowercase();
            for city in ["falkenstein", "helsinki", "nuremberg"] {
                assert!(!text.contains(city), "{city} in {text}");
            }
        }
    }

    #[test]
    fn a_folded_country_is_live_when_any_of_its_datacenters_is() {
        let v = json!([
            {"id":"a1","kind":"datacenter","name":"A","country":"Sweden","country_code":"SE",
             "status":"coming_soon","selectable":false},
            {"id":"a2","kind":"datacenter","name":"B","country":"Sweden","country_code":"SE",
             "status":"live","selectable":true}
        ]);
        let c = parse_choices(&v);
        assert_eq!(c.len(), 1);
        assert!(c[0].selectable && c[0].status == "live");
        assert_eq!(c[0].wire_id_for("sweden"), "a2", "the id the older server can store");
        assert_eq!(c[0].wire_id_for("A1"), "a1", "a typed datacenter id is kept");
    }

    #[test]
    fn a_datacenter_row_without_any_country_is_dropped_not_shown_by_its_city() {
        let v = json!([
            {"id":"zzz9","kind":"datacenter","name":"Qwerty","city":"Qwerty","status":"live","selectable":true},
            {"id":"europe","kind":"region","name":"Europe","status":"live","selectable":true,"is_default":true}
        ]);
        let c = parse_choices(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "europe");
    }

    #[test]
    fn me_region_choices_key_is_read_too() {
        let me = json!({"preferred_region": null, "available_regions": [], "choices": contract_list()});
        assert_eq!(parse_choices(&me).len(), 4);
    }

    #[test]
    fn rows_without_id_or_name_are_dropped_not_invented() {
        let v = json!([{"id":"fi"}, {"name":"x"}, "junk", {"id":"a","name":"A"}]);
        let c = parse_choices(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].id, "a");
        assert!(
            c[0].selectable,
            "status defaults to live, so selectable defaults to true"
        );
    }

    #[test]
    fn a_new_country_needs_no_client_release() {
        // A country this binary has never heard of is listed, matched by name,
        // code and id, and selectable — the only source is the payload.
        let v = json!([
            {"id":"europe","kind":"region","name":"Europe","status":"live","selectable":true,"is_default":true},
            {"id":"zz","kind":"country","name":"Nowhere","country":"Nowhere","country_code":"ZZ",
             "status":"live","selectable":true,"is_default":false}
        ]);
        let c = parse_choices(&v);
        for typed in ["nowhere", "ZZ", "Nowhere"] {
            assert!(
                matches!(resolve(&c, typed), Resolution::Found(x) if x.id == "zz"),
                "{typed}"
            );
        }
    }

    #[test]
    fn resolve_matches_id_name_and_country_code_case_insensitively() {
        let c = parse_choices(&contract_list());
        for typed in ["fi", "FI", "Finland", "FINLAND", "  finland "] {
            assert!(
                matches!(resolve(&c, typed), Resolution::Found(x) if x.id == "fi"),
                "{typed}"
            );
        }
        assert!(matches!(resolve(&c, "europe"), Resolution::Found(x) if x.id == "europe"));
        assert!(matches!(resolve(&c, "Europe"), Resolution::Found(x) if x.id == "europe"));
    }

    #[test]
    fn the_country_code_column_matches_when_the_id_differs() {
        let v = json!([{"id":"x1","kind":"country","name":"Foo","country":"Foo","country_code":"FO",
            "status":"live","selectable":true}]);
        let c = parse_choices(&v);
        assert!(matches!(resolve(&c, "fo"), Resolution::Found(x) if x.id == "x1"));
    }

    #[test]
    fn coming_soon_is_found_but_not_selectable() {
        let c = parse_choices(&contract_list());
        assert!(matches!(resolve(&c, "nl"), Resolution::NotSelectable(x) if x.is_coming_soon()));
        assert!(matches!(resolve(&c, "Netherlands"), Resolution::NotSelectable(_)));
    }

    #[test]
    fn a_datacenter_id_that_is_not_in_the_list_passes_through_by_shape() {
        let c = parse_choices(&contract_list());
        for typed in ["hel1", "FSN1", "nbg1", "xyz9"] {
            assert!(
                matches!(resolve(&c, typed), Resolution::LegacyId(ref id) if id == typed),
                "{typed}"
            );
        }
        // Cities and other words are not ids.
        for typed in ["helsinki", "falkenstein", "paris", "1", "hel", "hel123", "h1"] {
            assert_eq!(resolve(&c, typed), Resolution::Unknown, "{typed}");
        }
    }

    #[test]
    fn unknown_and_empty_input() {
        let c = parse_choices(&contract_list());
        assert_eq!(resolve(&c, "paris"), Resolution::Unknown);
        assert_eq!(resolve(&c, "  "), Resolution::Unknown);
        assert_eq!(resolve(&[], "europe"), Resolution::Unknown);
    }

    #[test]
    fn ambiguous_name_is_reported_not_guessed() {
        let v = json!([
            {"id":"a1","kind":"country","name":"Twin","status":"live","selectable":true},
            {"id":"b2","kind":"country","name":"Twin","status":"live","selectable":true}
        ]);
        let c = parse_choices(&v);
        assert!(matches!(resolve(&c, "twin"), Resolution::Ambiguous(h) if h.len() == 2));
    }

    #[test]
    fn current_label_default_country_missing_and_old_server() {
        let with = |pref: Value| json!({"preferred_region": pref, "choices": contract_list()});
        assert_eq!(current_label(&with(json!(null))), "Europe \u{00b7} default");
        assert_eq!(current_label(&with(json!("europe"))), "Europe \u{00b7} default");
        assert_eq!(current_label(&with(json!("fi"))), "Finland");
        assert_eq!(current_name(&with(json!("fi"))), "Finland");
        // Stored country the list no longer carries: say so, never guess.
        let gone = current_label(&with(json!("xx")));
        assert!(gone.starts_with("xx (not available right now"), "{gone}");
        // No `choices` key at all.
        assert_eq!(current_label(&json!({"preferred_region": "europe"})), "Europe");
        assert_eq!(current_label(&json!({"preferred_region": null})), "Europe");
    }

    #[test]
    fn a_stored_datacenter_id_reads_as_its_country_on_an_older_server() {
        let with = |pref: &str| json!({"preferred_region": pref, "choices": legacy_list()});
        assert_eq!(current_label(&with("hel1")), "Finland");
        assert_eq!(current_label(&with("nbg1")), "Germany");
        assert_eq!(current_id(&with("nbg1"), &parse_choices(&with("nbg1"))), "de");
        assert_eq!(current_name(&with("fsn1")), "Germany");
    }
}
