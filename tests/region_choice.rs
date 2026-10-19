//! `bb region` and the region rows of `bb whoami` / `bb account show`
//! (task 1919, by COUNTRY since task 2050), against an in-process mock of the
//! server contract (server PR #251):
//!
//! * `GET /api/v1/storage-regions` — public bare array of
//!   `europe` / `de` / `fi` / `nl` rows (`kind` region|country, no city);
//! * `GET /api/v1/me/region` — `{preferred_region, available_regions, choices}`;
//! * `PUT /api/v1/me/region` — validates exactly as the server does
//!   (`null` | `europe` | live country; the legacy datacenter ids
//!   `fsn1`/`nbg1`/`hel1` are mapped to their country for one release;
//!   anything else 400 `region_unavailable`) and stores the canonical value.
//!
//! A second mock shape (`Mock::legacy`) serves what a server BEFORE #251 sends:
//! one `datacenter` row per city. The CLI must fold those by country and must
//! never print a city, whichever server it talks to.
//!
//! Every test runs the real `bb` binary with a scratch HOME. The point under
//! test: the list is whatever the API says (change the mock's data, the output
//! changes, same binary), a coming-soon row is shown and cannot be chosen, the
//! id that goes on the wire is the list's id verbatim, and no city is ever
//! printed.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use serde_json::{Value, json};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// What server PR #251 serves: countries, no city key anywhere.
fn contract_list() -> Value {
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

/// What a server before #251 serves: one datacenter row per city, two of them
/// in Germany. The cities are fixture data that must never reach the output.
fn legacy_list() -> Value {
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

/// No place name a user could read as a city may appear in `text` (word match,
/// case-insensitive: `ede` the place, not "needed"). `allow` is what the user
/// typed themselves, which the CLI may echo back.
fn assert_no_places(text: &str, allow: &str) {
    let text = text.to_lowercase().replace(&allow.to_lowercase(), "");
    for word in text.split(|c: char| !(c.is_alphanumeric())) {
        for banned in ["falkenstein", "helsinki", "nuremberg", "nürnberg", "nurnberg", "ede"] {
            assert!(word != banned, "city {banned:?} printed:\n{text}");
        }
    }
}

#[derive(Clone)]
struct Mock {
    list: Arc<Mutex<Value>>,
    preferred: Arc<Mutex<Value>>,
    /// Every PUT body seen, with whether a Bearer token came with it.
    puts: Arc<Mutex<Vec<(Value, bool)>>>,
    /// When set, PUT answers 400 `region_unavailable` regardless of the id
    /// (a datacenter that went away between the list and the choice).
    reject_all_puts: Arc<Mutex<bool>>,
    /// The wire shape of every 400 `region_unavailable` the mock sends.
    reject_shape: Arc<Mutex<RejectShape>>,
    /// Server #251 maps the legacy datacenter ids to their country for one
    /// release; a server before it does not know them (they are list ids).
    maps_legacy_ids: bool,
}

/// The shapes a `region_unavailable` refusal can arrive in. `bb` must explain
/// all of them the same way (cli#84 Codex thread on `region.rs:142`).
#[derive(Clone, Copy)]
enum RejectShape {
    /// What server#211 sends today (`ApiError::BadRequest(..)` →
    /// `{"error": "region_unavailable: '<id>' is not available"}`, no
    /// `message`): the code lands in `ApiError.code` with the detail glued on.
    ServerToday,
    /// The typed two-field contract (`{"error":"region_unavailable",
    /// "message": <human text>}`): the code is ONLY in `ApiError.code`, the
    /// message does not mention it.
    Typed,
    /// A generic code with the detail in `message` (the shape this mock used
    /// to send).
    MessageOnly,
}

impl Mock {
    fn new() -> Mock {
        Mock {
            list: Arc::new(Mutex::new(contract_list())),
            preferred: Arc::new(Mutex::new(Value::Null)),
            puts: Arc::new(Mutex::new(Vec::new())),
            reject_all_puts: Arc::new(Mutex::new(false)),
            reject_shape: Arc::new(Mutex::new(RejectShape::ServerToday)),
            maps_legacy_ids: true,
        }
    }

    /// A server from before the by-country change.
    fn legacy() -> Mock {
        let mut m = Mock::new();
        *m.list.lock().unwrap() = legacy_list();
        m.maps_legacy_ids = false;
        m
    }
}

fn spawn(mock: Mock) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let app = Router::new()
                .route(
                    "/api/v1/storage-regions",
                    get(|State(m): State<Mock>| async move { Json(m.list.lock().unwrap().clone()) }),
                )
                .route(
                    "/api/v1/me/region",
                    get(|State(m): State<Mock>| async move {
                        Json(json!({
                            "preferred_region": m.preferred.lock().unwrap().clone(),
                            "available_regions": [{"continent":"europe","display_name":"Europe",
                                "example_country":"Germany","example_city":"Falkenstein","is_default":true}],
                            "choices": m.list.lock().unwrap().clone(),
                        }))
                    })
                    .put(
                        |State(m): State<Mock>, headers: HeaderMap, Json(body): Json<Value>| async move {
                            let bearer = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .is_some_and(|v| v.starts_with("Bearer "));
                            m.puts.lock().unwrap().push((body.clone(), bearer));
                            let shape = *m.reject_shape.lock().unwrap();
                            let reject = |id: &str| {
                                let detail = format!("region_unavailable: '{id}' is not available");
                                let body = match shape {
                                    RejectShape::ServerToday => json!({"error": detail}),
                                    RejectShape::Typed => json!({"error": "region_unavailable",
                                        "message": "That datacenter can't take new uploads right now."}),
                                    RejectShape::MessageOnly => json!({"error": "bad_request", "message": detail}),
                                };
                                (StatusCode::BAD_REQUEST, Json(body))
                            };
                            if *m.reject_all_puts.lock().unwrap() {
                                return reject("any");
                            }
                            let want = body.get("preferred_region").cloned().unwrap_or(Value::Null);
                            if let Some(id) = want.as_str() {
                                let mut id = id.to_lowercase();
                                if m.maps_legacy_ids {
                                    id = match id.as_str() {
                                        "fsn1" | "nbg1" => "de".to_string(),
                                        "hel1" => "fi".to_string(),
                                        _ => id,
                                    };
                                }
                                let ok = id == "europe"
                                    || m.list.lock().unwrap().as_array().unwrap().iter().any(|c| {
                                        c["id"] == id.as_str() && c["status"] == "live" && c["kind"] != "region"
                                    });
                                if !ok {
                                    return reject(&id);
                                }
                                *m.preferred.lock().unwrap() = json!(id);
                            } else {
                                *m.preferred.lock().unwrap() = Value::Null;
                            }
                            (
                                StatusCode::OK,
                                Json(json!({"preferred_region": m.preferred.lock().unwrap().clone()})),
                            )
                        },
                    ),
                )
                // The rest of what `bb whoami` / `bb account show` read.
                .route(
                    "/api/v1/auth/me",
                    get(|| async { Json(json!({"email":"region@beebeeb.io"})) }),
                )
                .route(
                    "/api/v1/billing/subscription",
                    get(|| async {
                        Json(json!({"plan":"pro","quota_bytes":1_000_000_000_000i64,"account_state":"ok"}))
                    }),
                )
                .route("/api/v1/auth/sessions", get(|| async { Json(json!({"sessions": []})) }))
                .route(
                    "/api/v1/files/usage",
                    get(|| async { Json(json!({"used_bytes": 1000})) }),
                )
                .route(
                    "/api/v1/account/security-score",
                    get(|| async { Json(json!({"score": 4, "max": 5, "label": "Strong", "factors": []})) }),
                )
                .route(
                    "/api/v1/account/sessions",
                    get(|| async { Json(json!({"sessions": []})) }),
                )
                .route("/api/v1/auth/passkeys", get(|| async { Json(json!({"passkeys": []})) }))
                .route("/api/v1/files/count", get(|| async { Json(json!({"total_files": 3})) }))
                .with_state(mock);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = axum::serve(listener, app).await;
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn scratch_home(api: &str, signed_in: bool) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let home = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("region-choice-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let mut config = json!({ "api_url": api });
    if signed_in {
        config["session_token"] = json!("test-session-token");
        config["email"] = json!("region@beebeeb.io");
        config["master_key"] = json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
    }
    for dir in [
        home.join("Library/Application Support/beebeeb"),
        home.join(".config/beebeeb"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    }
    home
}

fn bb(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run bb")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn row<'a>(text: &'a str, id: &str) -> &'a str {
    text.lines()
        .find(|l| l.split_whitespace().any(|w| w == id))
        .unwrap_or_else(|| panic!("no row for {id} in:\n{text}"))
}

#[test]
fn list_shows_europe_default_the_countries_and_the_netherlands_as_coming_soon() {
    let mock = Mock::new();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    let o = bb(&home, &["region"]);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(0), "stdout: {text}\nstderr: {}", err(&o));

    let europe = row(&text, "europe");
    assert!(europe.contains("default"), "{text}");
    assert!(europe.contains("European data centers"), "{text}");
    assert!(
        europe.contains('\u{25cf}'),
        "no preference = the default is current:\n{text}"
    );
    assert!(row(&text, "de").contains("Germany") && !row(&text, "de").contains("coming soon"));
    assert!(row(&text, "fi").contains("Finland") && !row(&text, "fi").contains("coming soon"));
    let nl = row(&text, "nl");
    assert!(
        nl.contains("Netherlands") && nl.contains("coming soon") && nl.contains("not selectable"),
        "{nl}"
    );
    assert_no_places(&text, "");
    // `bb region list` is the same as bare `bb region`.
    assert_eq!(out(&bb(&home, &["region", "list"])), text);
}

#[test]
fn the_list_is_fetched_from_the_api_changing_it_changes_the_output() {
    let mock = Mock::new();
    let list = mock.list.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    let before = out(&bb(&home, &["region"]));
    assert!(!before.contains("Sweden"));
    assert!(row(&before, "nl").contains("coming soon"));

    // The Netherlands goes live and a brand-new country appears. Same binary, no release.
    {
        let mut l = list.lock().unwrap();
        let arr = l.as_array_mut().unwrap();
        for c in arr.iter_mut() {
            if c["id"] == "nl" {
                c["status"] = json!("live");
                c["selectable"] = json!(true);
            }
        }
        arr.push(json!({"id":"se","kind":"country","name":"Sweden","country":"Sweden",
            "country_code":"SE","status":"live","selectable":true,"is_default":false}));
    }
    let after = out(&bb(&home, &["region"]));
    assert!(!row(&after, "nl").contains("coming soon"), "{after}");
    assert!(row(&after, "se").contains("Sweden"), "{after}");

    for typed in ["sweden", "SE", "Netherlands"] {
        let o = bb(&home, &["region", "set", typed]);
        assert_eq!(o.status.code(), Some(0), "{typed}: {}", err(&o));
    }
}

#[test]
fn json_list_passes_the_server_rows_through_and_names_the_current_choice() {
    let api = spawn(Mock::new());
    let home = scratch_home(&api, true);
    let o = bb(&home, &["--json", "region"]);
    let v: Value = serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{e}: {}", out(&o)));
    assert_eq!(v["current"], "europe");
    assert_eq!(v["regions"].as_array().unwrap().len(), 4);
    assert_eq!(v["regions"][3]["status"], "coming_soon");
    assert_eq!(v["regions"][3]["selectable"], false);
    assert_eq!(v["regions"][1]["country"], "Germany");
    assert_eq!(v["regions"][1]["country_code"], "DE");
    assert_eq!(v["regions"], contract_list(), "rows are the server's, verbatim");
}

#[test]
fn set_accepts_a_country_name_or_code_in_any_case_and_sends_the_list_id_verbatim() {
    for typed in ["de", "DE", "Germany", "GERMANY", "  germany "] {
        let mock = Mock::new();
        let puts = mock.puts.clone();
        let api = spawn(mock);
        let home = scratch_home(&api, true);

        let o = bb(&home, &["region", "set", typed]);
        assert_eq!(o.status.code(), Some(0), "{typed}: {}\n{}", out(&o), err(&o));
        assert!(out(&o).contains("Storage location set to Germany."), "{}", out(&o));
        assert!(out(&o).contains("stay where they are"), "{}", out(&o));
        assert_no_places(&out(&o), "");
        assert_eq!(
            *puts.lock().unwrap(),
            vec![(json!({"preferred_region": "de"}), true)],
            "{typed}"
        );
    }

    // Read back through the API: the list marks Germany, `get` and `whoami` agree.
    let api = spawn(Mock::new());
    let home = scratch_home(&api, true);
    assert_eq!(bb(&home, &["region", "set", "Finland"]).status.code(), Some(0));
    let list = out(&bb(&home, &["region"]));
    assert!(row(&list, "fi").contains('\u{25cf}'), "{list}");
    assert!(!row(&list, "europe").contains('\u{25cf}'), "{list}");
    let get = out(&bb(&home, &["region", "get"]));
    assert!(get.contains("Finland"), "{get}");
    assert_no_places(&get, "");
    assert_eq!(out(&bb(&home, &["--quiet", "region", "get"])).trim(), "fi");
    let v: Value = serde_json::from_str(&out(&bb(&home, &["--json", "region", "get"]))).unwrap();
    assert_eq!(v["current"], "fi");
    assert_eq!(v["country"], "Finland");
    assert_eq!(v["country_code"], "FI");
    assert_eq!(v["preferred_region"], "fi", "existing machine fields stay");
}

#[test]
fn set_europe_chooses_the_default_any_data_center() {
    let mock = Mock::new();
    let puts = mock.puts.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    assert_eq!(bb(&home, &["region", "set", "fi"]).status.code(), Some(0));
    let o = bb(&home, &["region", "set", "europe"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("any of its European data centers"), "{}", out(&o));
    assert_eq!(
        puts.lock().unwrap().last().unwrap().0,
        json!({"preferred_region": "europe"})
    );
    let get = out(&bb(&home, &["region", "get"]));
    assert!(get.contains("Europe") && get.contains("default"), "{get}");
}

#[test]
fn the_netherlands_is_coming_soon_and_never_reaches_the_server() {
    let mock = Mock::new();
    let puts = mock.puts.clone();
    let preferred = mock.preferred.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    for typed in ["nl", "NL", "Netherlands"] {
        let o = bb(&home, &["region", "set", typed]);
        assert_eq!(o.status.code(), Some(1), "stdout: {}", out(&o));
        assert!(err(&o).contains("Netherlands is coming soon"), "{}", err(&o));
        assert!(err(&o).contains("unchanged"), "{}", err(&o));
    }
    assert!(
        puts.lock().unwrap().is_empty(),
        "a coming-soon country must not be sent"
    );
    assert_eq!(*preferred.lock().unwrap(), Value::Null);
}

#[test]
fn unknown_input_fails_without_a_request_and_lists_the_real_options() {
    let mock = Mock::new();
    let puts = mock.puts.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    let o = bb(&home, &["region", "set", "paris"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(
        e.contains("'paris'") && e.contains("de (Germany)") && e.contains("fi (Finland)") && e.contains("europe"),
        "{e}"
    );
    assert!(!e.contains("Netherlands"), "coming soon is not an option: {e}");
    assert!(puts.lock().unwrap().is_empty());
}

#[test]
fn a_city_is_no_longer_a_choice_and_the_error_prints_no_city_list() {
    let mock = Mock::new();
    let puts = mock.puts.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    for typed in ["helsinki", "Falkenstein", "nuremberg"] {
        let o = bb(&home, &["region", "set", typed]);
        assert_eq!(o.status.code(), Some(1), "{typed}");
        let e = err(&o);
        assert!(e.contains("chosen by country"), "{typed}: {e}");
        assert!(e.contains("'de'") && e.contains("'fi'"), "{typed}: {e}");
        // The CLI may echo what the user typed; it must not volunteer a city.
        assert_no_places(&e, typed);
    }
    assert!(puts.lock().unwrap().is_empty(), "a city never reaches the server");
}

#[test]
fn a_legacy_datacenter_id_is_passed_through_and_the_server_maps_it() {
    let mock = Mock::new();
    let puts = mock.puts.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    // `hel1` is not in the country list. It is datacenter-shaped, so it goes to
    // the server verbatim (one release of mapping), and the answer is read
    // back as the country the server stored.
    let o = bb(&home, &["region", "set", "HEL1"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("Storage location set to Finland."), "{}", out(&o));
    assert_no_places(&out(&o), "");
    assert_eq!(*puts.lock().unwrap(), vec![(json!({"preferred_region": "HEL1"}), true)]);
    assert_eq!(out(&bb(&home, &["--quiet", "region", "get"])).trim(), "fi");
}

/// A PUT refused with `region_unavailable` (the choice lost a race with the
/// list) is explained in `bb`'s words and leaves the stored choice unchanged,
/// whichever wire shape the refusal arrives in.
fn assert_rejection_explained(shape: RejectShape) {
    let mock = Mock::new();
    *mock.reject_all_puts.lock().unwrap() = true;
    *mock.reject_shape.lock().unwrap() = shape;
    let preferred = mock.preferred.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    let o = bb(&home, &["region", "set", "fi"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(
        e.contains("Finland") && e.contains("isn't available right now") && e.contains("unchanged"),
        "{e}"
    );
    assert!(!e.contains("region_unavailable"), "raw server code leaked: {e}");
    assert_eq!(*preferred.lock().unwrap(), Value::Null);

    // A passed-through legacy id the server refuses is explained the same way.
    let o = bb(&home, &["region", "set", "xyz9"]);
    assert_eq!(o.status.code(), Some(1));
    let e = err(&o);
    assert!(
        e.contains("isn't available right now") && e.contains("unchanged"),
        "{e}"
    );
    assert!(!e.contains("region_unavailable"), "raw server code leaked: {e}");
}

#[test]
fn a_server_rejection_is_explained_and_leaves_the_choice_unchanged() {
    assert_rejection_explained(RejectShape::ServerToday);
}

#[test]
fn a_typed_region_unavailable_rejection_is_explained_by_its_code() {
    assert_rejection_explained(RejectShape::Typed);
}

#[test]
fn a_region_unavailable_rejection_carried_in_the_message_is_explained() {
    assert_rejection_explained(RejectShape::MessageOnly);
}

#[test]
fn signed_out_the_list_still_prints_but_set_and_get_refuse() {
    let mock = Mock::new();
    let puts = mock.puts.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, false);

    let o = bb(&home, &["region"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(
        !text.contains('\u{25cf}'),
        "no session, no current-choice marker:\n{text}"
    );
    assert!(text.contains("bb login"), "{text}");
    assert!(row(&text, "fi").contains("Finland"));

    for args in [&["region", "set", "fi"][..], &["region", "get"][..]] {
        let o = bb(&home, args);
        assert_eq!(o.status.code(), Some(1));
        assert!(err(&o).contains("Not logged in"), "{}", err(&o));
    }
    assert!(puts.lock().unwrap().is_empty());
}

#[test]
fn whoami_names_the_chosen_country_from_the_api_list() {
    let api = spawn(Mock::new());
    let home = scratch_home(&api, true);

    let default = out(&bb(&home, &["whoami"]));
    assert!(default.contains("Europe \u{00b7} default"), "{default}");

    assert_eq!(bb(&home, &["region", "set", "de"]).status.code(), Some(0));
    let text = out(&bb(&home, &["whoami"]));
    let region_line = text.lines().find(|l| l.trim_start().starts_with("region")).unwrap();
    assert_eq!(
        region_line.split_whitespace().collect::<Vec<_>>(),
        ["region", "Germany"],
        "country only, no `Germany, Germany`"
    );
    assert_no_places(&text, "");

    let v: Value = serde_json::from_str(&out(&bb(&home, &["--json", "whoami"]))).unwrap();
    assert_eq!(v["region"], "Germany");
    assert_eq!(v["region_id"], "de");
}

#[test]
fn account_show_region_row_names_the_country_and_never_a_provider() {
    let api = spawn(Mock::new());
    let home = scratch_home(&api, true);
    assert_eq!(bb(&home, &["region", "set", "fi"]).status.code(), Some(0));

    let o = bb(&home, &["--json", "account", "show"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap_or_else(|e| panic!("{e}: {}", out(&o)));
    assert_eq!(v["region"]["preferred_region"], "fi", "{v}");
    assert_eq!(v["region"]["choices"].as_array().map(|a| a.len()), Some(4));

    let o = bb(&home, &["account", "show"]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(!text.to_lowercase().contains("hetzner"), "{text}");
    assert_no_places(&text, "");
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("region"))
        .unwrap_or_else(|| panic!("no region row in:\n{text}"));
    assert_eq!(
        line.split_whitespace().collect::<Vec<_>>(),
        ["region", "Finland"],
        "country only, no `Finland, Finland`"
    );
}

// ── a server from before the by-country change ──────────────────────────────

#[test]
fn an_older_server_with_city_rows_is_folded_by_country_and_prints_no_city() {
    let api = spawn(Mock::legacy());
    let home = scratch_home(&api, true);

    let o = bb(&home, &["region"]);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(0), "{text}\n{}", err(&o));
    assert_no_places(&text, "");
    // Two German datacenters are one Germany row.
    assert_eq!(
        text.lines().filter(|l| l.contains("Germany")).count(),
        1,
        "Germany once:\n{text}"
    );
    assert!(row(&text, "de").contains("Germany") && !row(&text, "de").contains("coming soon"));
    assert!(row(&text, "fi").contains("Finland"));
    let nl = row(&text, "nl");
    assert!(nl.contains("Netherlands") && nl.contains("coming soon") && nl.contains("not selectable"));
    assert!(row(&text, "europe").contains("default"));

    // Machine output: folded rows carry the stable fields plus country/code, no city.
    let v: Value = serde_json::from_str(&out(&bb(&home, &["--json", "region"]))).unwrap();
    let rows = v["regions"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "{v}");
    let de = rows.iter().find(|r| r["id"] == "de").unwrap();
    assert_eq!(de["country"], "Germany");
    assert_eq!(de["country_code"], "DE");
    assert_eq!(de["status"], "live");
    assert_eq!(de["selectable"], true);
    assert_no_places(&v.to_string(), "");
}

#[test]
fn an_older_server_still_gets_a_datacenter_id_it_understands() {
    let mock = Mock::legacy();
    let puts = mock.puts.clone();
    let api = spawn(mock);
    let home = scratch_home(&api, true);

    // `germany` folds two datacenters; the first selectable one is what that
    // server can store. Whoever reads it back sees the country.
    let o = bb(&home, &["region", "set", "germany"]);
    assert_eq!(o.status.code(), Some(0), "{}\n{}", out(&o), err(&o));
    assert!(out(&o).contains("Storage location set to Germany."), "{}", out(&o));
    assert_no_places(&out(&o), "");
    assert_eq!(*puts.lock().unwrap(), vec![(json!({"preferred_region": "fsn1"}), true)]);

    let list = out(&bb(&home, &["region"]));
    assert!(
        row(&list, "de").contains('\u{25cf}'),
        "stored fsn1 reads as Germany:\n{list}"
    );
    assert_no_places(&list, "");
    assert_eq!(out(&bb(&home, &["--quiet", "region", "get"])).trim(), "de");
    let who = out(&bb(&home, &["whoami"]));
    assert!(who.contains("Germany"), "{who}");
    assert_no_places(&who, "");

    // Typing the exact datacenter id keeps that datacenter.
    assert_eq!(bb(&home, &["region", "set", "hel1"]).status.code(), Some(0));
    assert_eq!(
        puts.lock().unwrap().last().unwrap().0,
        json!({"preferred_region": "hel1"})
    );
    assert_eq!(out(&bb(&home, &["--quiet", "region", "get"])).trim(), "fi");
}

#[test]
fn help_lists_the_region_command_by_country() {
    let api = spawn(Mock::new());
    let home = scratch_home(&api, false);
    let o = bb(&home, &["--help"]);
    assert!(
        out(&o).lines().any(|l| l.trim_start().starts_with("region")),
        "{}",
        out(&o)
    );
    for args in [&["region", "--help"][..], &["region", "set", "--help"][..]] {
        let o = bb(&home, args);
        let h = out(&o);
        assert!(h.contains("list") || h.contains("country"), "{h}");
        assert!(h.contains("country"), "help names the country model: {h}");
        assert!(
            !h.to_lowercase().contains("city"),
            "help must not advertise a city: {h}"
        );
        assert_no_places(&h, "");
    }
}
