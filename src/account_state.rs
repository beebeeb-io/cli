//! Account lifecycle state off `GET /api/v1/billing/subscription` (task 1037).
//!
//! Since 1037 nobody gets a free account: signup starts a trial that needs a
//! payment mandate. The subscription response carries two additive fields
//! that say whether the account may upload at all:
//!
//! - `account_state`: `"ok"` (entitled, or grandfathered Free), `"needs_plan"`
//!   (a new-model account that never started a trial or plan — upload quota 0)
//!   or `"lapsed"` (a trial/plan that ended unpaid — read-only, quota 0,
//!   deleted at `data_deletion_at`).
//! - `data_deletion_at`: RFC3339, set only for `lapsed`.
//!
//! A server that predates 1037 sends neither field; that (and any value this
//! CLI doesn't know yet) reads as [`AccountState::Ok`], so an old server or a
//! future state never blocks a command here — the server stays the enforcer
//! (it refuses upload init / share create with `409 plan_required` /
//! `account_lapsed`, older servers with `413 quota_exceeded`; see
//! [`Refusal`]), this module only explains *why* to the user.

use chrono::{DateTime, Utc};
use serde_json::Value;

/// See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountState {
    Ok,
    NeedsPlan,
    Lapsed { data_deletion_at: Option<DateTime<Utc>> },
}

impl AccountState {
    /// Parse the state off a subscription response. Missing, `null`,
    /// non-string or unknown `account_state` ⇒ `Ok`.
    pub(crate) fn from_subscription(sub: &Value) -> Self {
        match sub.get("account_state").and_then(Value::as_str) {
            Some("needs_plan") => AccountState::NeedsPlan,
            Some("lapsed") => AccountState::Lapsed {
                data_deletion_at: sub
                    .get("data_deletion_at")
                    .and_then(Value::as_str)
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc)),
            },
            _ => AccountState::Ok,
        }
    }

    /// The wire value, for `--json` output.
    pub(crate) fn slug(&self) -> &'static str {
        match self {
            AccountState::Ok => "ok",
            AccountState::NeedsPlan => "needs_plan",
            AccountState::Lapsed { .. } => "lapsed",
        }
    }

    /// `data_deletion_at` as RFC3339 (UTC), for `--json` output. `None`
    /// unless lapsed with a known date.
    pub(crate) fn data_deletion_at_rfc3339(&self) -> Option<String> {
        match self {
            AccountState::Lapsed { data_deletion_at } => data_deletion_at.map(|d| d.to_rfc3339()),
            _ => None,
        }
    }

    /// Short label for the `state` row in `bb whoami` / `bb quota`.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            AccountState::Ok => "ok",
            AccountState::NeedsPlan => "no plan yet",
            AccountState::Lapsed { .. } => "read-only (trial ended)",
        }
    }

    /// The user-facing explanation (and what to do about it) for a state
    /// that blocks uploads; `None` for `Ok`. Used both when an upload is
    /// refused and as the notice under `bb whoami` / `bb quota` /
    /// `bb billing show`. `app_base` is the web app's base URL
    /// (`crate::web_url::web_app_base()`), no trailing slash.
    pub(crate) fn notice(&self, app_base: &str) -> Option<String> {
        let base = app_base.trim_end_matches('/');
        match self {
            AccountState::Ok => None,
            AccountState::NeedsPlan => Some(format!(
                "Your account has no plan yet \u{2014} choose one at {base}/choose-plan"
            )),
            AccountState::Lapsed { data_deletion_at } => {
                let when = match data_deletion_at {
                    Some(at) => format!("on {}", format_deletion_date(at)),
                    None => "60 days after it lapsed".to_string(),
                };
                Some(format!(
                    "Your trial has ended; your vault is read-only and will be deleted {when}. \
                     Subscribe at {base}/billing?view=change"
                ))
            }
        }
    }
}

/// Why the server refused an upload init / share create, as far as account
/// state is concerned. Task 1037 server contract: an account without a plan
/// gets `409 { error: "plan_required" | "account_lapsed" }`; the older
/// `413 quota_exceeded` (quota 0) path is still recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `plan_required` — the account never started a trial or plan.
    PlanRequired,
    /// `account_lapsed` — read-only; the deletion date lives on the
    /// subscription, not on the error body.
    AccountLapsed,
    /// `quota_exceeded` / 413 — only an account-state problem if the
    /// subscription says so.
    Quota,
}

impl Refusal {
    /// Classify a refusal by its stable `error` code and HTTP status.
    pub(crate) fn classify(code: Option<&str>, status: u16) -> Option<Self> {
        match code {
            Some("plan_required") => Some(Refusal::PlanRequired),
            Some("account_lapsed") => Some(Refusal::AccountLapsed),
            Some("quota_exceeded") => Some(Refusal::Quota),
            _ if status == 413 => Some(Refusal::Quota),
            _ => None,
        }
    }

    /// Whether resolving this refusal needs `GET /billing/subscription`.
    pub(crate) fn needs_subscription(self) -> bool {
        !matches!(self, Refusal::PlanRequired)
    }

    /// The account state that explains the refusal, given the subscription's
    /// state if it was fetched (`None`: not fetched, or the fetch failed).
    /// `None` result: nothing to explain — keep the server's error.
    pub(crate) fn resolve(self, fetched: Option<&AccountState>) -> Option<AccountState> {
        match self {
            Refusal::PlanRequired => Some(AccountState::NeedsPlan),
            // The server's code wins; the subscription only adds the date.
            Refusal::AccountLapsed => Some(match fetched {
                Some(lapsed @ AccountState::Lapsed { .. }) => lapsed.clone(),
                _ => AccountState::Lapsed { data_deletion_at: None },
            }),
            Refusal::Quota => fetched.filter(|s| **s != AccountState::Ok).cloned(),
        }
    }
}

/// Human date for `data_deletion_at`, e.g. `November 27, 2026` (the same
/// shape `bb billing show` uses for renewal dates). UTC calendar date.
pub(crate) fn format_deletion_date(at: &DateTime<Utc>) -> String {
    at.format("%B %-d, %Y").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    // ── parsing ──────────────────────────────────────────────────────────

    #[test]
    fn missing_account_state_is_ok() {
        // A pre-1037 server sends neither field.
        let sub = json!({ "plan": "free", "status": "active", "quota_bytes": 5_000_000_000i64 });
        assert_eq!(AccountState::from_subscription(&sub), AccountState::Ok);
    }

    #[test]
    fn null_or_non_string_account_state_is_ok() {
        assert_eq!(
            AccountState::from_subscription(&json!({ "account_state": null })),
            AccountState::Ok
        );
        assert_eq!(
            AccountState::from_subscription(&json!({ "account_state": 3 })),
            AccountState::Ok
        );
        // A non-object body (e.g. `unwrap_or_default()` on a failed fetch).
        assert_eq!(AccountState::from_subscription(&Value::Null), AccountState::Ok);
    }

    #[test]
    fn unknown_account_state_is_ok() {
        // Forward-compatible: a state this CLI doesn't know never blocks.
        assert_eq!(
            AccountState::from_subscription(&json!({ "account_state": "frozen_by_future_server" })),
            AccountState::Ok
        );
    }

    #[test]
    fn explicit_ok_is_ok() {
        assert_eq!(
            AccountState::from_subscription(&json!({ "account_state": "ok", "data_deletion_at": null })),
            AccountState::Ok
        );
    }

    #[test]
    fn needs_plan_parses() {
        let sub = json!({ "account_state": "needs_plan", "effective_plan": "none", "quota_bytes": 0 });
        assert_eq!(AccountState::from_subscription(&sub), AccountState::NeedsPlan);
    }

    #[test]
    fn lapsed_parses_with_its_deletion_date() {
        let sub = json!({
            "account_state": "lapsed",
            "data_deletion_at": "2026-11-27T10:00:00Z",
        });
        assert_eq!(
            AccountState::from_subscription(&sub),
            AccountState::Lapsed {
                data_deletion_at: Some(at("2026-11-27T10:00:00Z"))
            }
        );
    }

    #[test]
    fn lapsed_normalises_an_offset_deletion_date_to_utc() {
        let sub = json!({
            "account_state": "lapsed",
            "data_deletion_at": "2026-11-27T01:00:00+02:00",
        });
        assert_eq!(
            AccountState::from_subscription(&sub),
            AccountState::Lapsed {
                data_deletion_at: Some(at("2026-11-26T23:00:00Z"))
            }
        );
    }

    #[test]
    fn lapsed_without_or_with_a_bad_deletion_date_still_parses_as_lapsed() {
        assert_eq!(
            AccountState::from_subscription(&json!({ "account_state": "lapsed" })),
            AccountState::Lapsed { data_deletion_at: None }
        );
        assert_eq!(
            AccountState::from_subscription(&json!({ "account_state": "lapsed", "data_deletion_at": "soon" })),
            AccountState::Lapsed { data_deletion_at: None }
        );
    }

    #[test]
    fn slug_and_json_date_round_trip_the_wire_values() {
        assert_eq!(AccountState::Ok.slug(), "ok");
        assert_eq!(AccountState::NeedsPlan.slug(), "needs_plan");
        let lapsed = AccountState::Lapsed {
            data_deletion_at: Some(at("2026-11-27T10:00:00Z")),
        };
        assert_eq!(lapsed.slug(), "lapsed");
        assert_eq!(
            lapsed.data_deletion_at_rfc3339().as_deref(),
            Some("2026-11-27T10:00:00+00:00")
        );
        assert_eq!(AccountState::Ok.data_deletion_at_rfc3339(), None);
        assert_eq!(AccountState::NeedsPlan.data_deletion_at_rfc3339(), None);
    }

    #[test]
    fn labels_are_short_and_distinct() {
        assert_eq!(AccountState::Ok.label(), "ok");
        assert_eq!(AccountState::NeedsPlan.label(), "no plan yet");
        assert_eq!(
            AccountState::Lapsed { data_deletion_at: None }.label(),
            "read-only (trial ended)"
        );
    }

    // ── messages ─────────────────────────────────────────────────────────

    #[test]
    fn ok_has_no_notice() {
        assert_eq!(AccountState::Ok.notice("https://app.beebeeb.io"), None);
    }

    #[test]
    fn needs_plan_notice_points_at_choose_plan() {
        assert_eq!(
            AccountState::NeedsPlan.notice("https://app.beebeeb.io").as_deref(),
            Some("Your account has no plan yet \u{2014} choose one at https://app.beebeeb.io/choose-plan")
        );
    }

    #[test]
    fn lapsed_notice_names_the_deletion_date_and_points_at_billing() {
        let lapsed = AccountState::Lapsed {
            data_deletion_at: Some(at("2026-11-27T10:00:00Z")),
        };
        assert_eq!(
            lapsed.notice("https://app.beebeeb.io").as_deref(),
            Some(
                "Your trial has ended; your vault is read-only and will be deleted on November 27, 2026. \
                 Subscribe at https://app.beebeeb.io/billing?view=change"
            )
        );
    }

    #[test]
    fn lapsed_notice_without_a_date_still_says_what_happens() {
        let lapsed = AccountState::Lapsed { data_deletion_at: None };
        assert_eq!(
            lapsed.notice("https://app.beebeeb.io").as_deref(),
            Some(
                "Your trial has ended; your vault is read-only and will be deleted 60 days after it lapsed. \
                 Subscribe at https://app.beebeeb.io/billing?view=change"
            )
        );
    }

    #[test]
    fn notices_follow_the_web_app_base_they_are_given() {
        // A CLI pointed at a local API must never print a production link.
        let n = AccountState::NeedsPlan.notice("http://localhost:5173").unwrap();
        assert!(n.ends_with("http://localhost:5173/choose-plan"), "{n}");
        assert!(!n.contains("beebeeb.io"), "{n}");
    }

    // ── refusal classification (409 plan_required / account_lapsed) ──────

    #[test]
    fn classify_maps_the_typed_409_codes() {
        assert_eq!(
            Refusal::classify(Some("plan_required"), 409),
            Some(Refusal::PlanRequired)
        );
        assert_eq!(
            Refusal::classify(Some("account_lapsed"), 409),
            Some(Refusal::AccountLapsed)
        );
    }

    #[test]
    fn classify_keeps_the_quota_path() {
        assert_eq!(Refusal::classify(Some("quota_exceeded"), 413), Some(Refusal::Quota));
        assert_eq!(Refusal::classify(None, 413), Some(Refusal::Quota));
    }

    #[test]
    fn classify_ignores_other_conflicts_and_errors() {
        // A 409 "upload already in progress" / stale base version is NOT an
        // account-state refusal — it must reach the caller untouched.
        assert_eq!(Refusal::classify(None, 409), None);
        assert_eq!(Refusal::classify(Some("conflict"), 409), None);
        assert_eq!(Refusal::classify(Some("unauthorized"), 401), None);
        assert_eq!(Refusal::classify(None, 500), None);
    }

    #[test]
    fn plan_required_needs_no_lookup_and_is_needs_plan() {
        assert!(!Refusal::PlanRequired.needs_subscription());
        assert_eq!(Refusal::PlanRequired.resolve(None), Some(AccountState::NeedsPlan));
    }

    #[test]
    fn account_lapsed_takes_the_date_from_the_subscription() {
        assert!(Refusal::AccountLapsed.needs_subscription());
        let fetched = AccountState::Lapsed {
            data_deletion_at: Some(at("2026-11-27T10:00:00Z")),
        };
        assert_eq!(Refusal::AccountLapsed.resolve(Some(&fetched)), Some(fetched.clone()));
    }

    #[test]
    fn account_lapsed_still_explains_without_a_usable_subscription() {
        // Fetch failed, or a stale/odd subscription disagrees: the server's
        // code wins, just without a date.
        let dateless = Some(AccountState::Lapsed { data_deletion_at: None });
        assert_eq!(Refusal::AccountLapsed.resolve(None), dateless);
        assert_eq!(Refusal::AccountLapsed.resolve(Some(&AccountState::Ok)), dateless);
    }

    #[test]
    fn quota_refusal_is_explained_only_by_a_blocking_state() {
        assert!(Refusal::Quota.needs_subscription());
        assert_eq!(Refusal::Quota.resolve(None), None);
        assert_eq!(Refusal::Quota.resolve(Some(&AccountState::Ok)), None);
        assert_eq!(
            Refusal::Quota.resolve(Some(&AccountState::NeedsPlan)),
            Some(AccountState::NeedsPlan)
        );
    }

    #[test]
    fn deletion_date_uses_the_utc_calendar_day() {
        assert_eq!(format_deletion_date(&at("2026-11-27T23:59:59Z")), "November 27, 2026");
        assert_eq!(format_deletion_date(&at("2026-01-05T00:00:00Z")), "January 5, 2026");
    }
}
