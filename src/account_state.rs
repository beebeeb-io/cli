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

// ── Onboarding document (task 1750, spec 5.9 / T13) ─────────────────────────
//
// `GET /api/v1/onboarding` is the server's single statement of (a) whether this
// client may create an account and (b) what state the signed-in account is in.
// The CLI reads it; it never acts on it beyond explaining:
//
// - **The CLI is never lifted for native signup** (policy matrix, task 1741).
//   Whatever `signup.mode` says, `bb signup` sends the user to the web app. A
//   `native` answer is reported honestly ("the server would allow it, the CLI
//   does not do it"), never obeyed.
// - **Lenient on purpose** (contract rule 3): unknown fields are ignored, an
//   unknown `account.state` is only a label (decide from `capabilities`), an
//   unknown `signup.mode` reads as `web_only`. The schema file is for
//   validating server output, not a strict parser.
// - **Legacy fallback** (rule 6): `Ok(None)` / an error / a document that is not
//   schema major 1 means the callers keep the `/billing/subscription`
//   `AccountState` logic above, unchanged.
//
// Crypto, prices and purchase flows are not in this module and never will be:
// the document carries parameters only.

/// `signup.mode`. Unknown values fail closed to [`SignupMode::WebOnly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignupMode {
    Native,
    WebHandoff,
    WebOnly,
}

impl SignupMode {
    pub(crate) fn from_wire(raw: &str) -> Self {
        match raw {
            "native" => SignupMode::Native,
            "web_handoff" => SignupMode::WebHandoff,
            _ => SignupMode::WebOnly,
        }
    }
}

/// `account.state`. `Other` carries a value this CLI does not know yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DocState {
    Allowance,
    NeedsPlan,
    TrialingNoCard,
    TrialEnded,
    Trialing,
    TrialCancelling,
    Active,
    PastDue,
    ReadOnly,
    Frozen,
    Lapsed,
    LegacyFree,
    Other(String),
}

impl DocState {
    pub(crate) fn from_wire(raw: &str) -> Self {
        match raw {
            "allowance" => DocState::Allowance,
            "needs_plan" => DocState::NeedsPlan,
            "trialing_no_card" => DocState::TrialingNoCard,
            "trial_ended" => DocState::TrialEnded,
            "trialing" => DocState::Trialing,
            "trial_cancelling" => DocState::TrialCancelling,
            "active" => DocState::Active,
            "past_due" => DocState::PastDue,
            "read_only" => DocState::ReadOnly,
            "frozen" => DocState::Frozen,
            "lapsed" => DocState::Lapsed,
            "legacy_free" => DocState::LegacyFree,
            other => DocState::Other(other.to_string()),
        }
    }

    pub(crate) fn slug(&self) -> &str {
        match self {
            DocState::Allowance => "allowance",
            DocState::NeedsPlan => "needs_plan",
            DocState::TrialingNoCard => "trialing_no_card",
            DocState::TrialEnded => "trial_ended",
            DocState::Trialing => "trialing",
            DocState::TrialCancelling => "trial_cancelling",
            DocState::Active => "active",
            DocState::PastDue => "past_due",
            DocState::ReadOnly => "read_only",
            DocState::Frozen => "frozen",
            DocState::Lapsed => "lapsed",
            DocState::LegacyFree => "legacy_free",
            DocState::Other(s) => s,
        }
    }
}

/// The parts of the document the CLI reads. Everything is optional.
#[derive(Debug, Clone)]
pub(crate) struct OnboardingDoc {
    raw: Value,
}

/// `signup` of a `pre_account` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignupInfo {
    pub(crate) mode: SignupMode,
    pub(crate) allowed: bool,
    pub(crate) reason: Option<String>,
    pub(crate) web_url: Option<String>,
}

/// One capability the account is denied, with the server's reason code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Denied {
    pub(crate) capability: String,
    pub(crate) reason: Option<String>,
}

/// What `bb whoami` shows for a signed-in account, from the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountSummary {
    pub(crate) state: DocState,
    /// Short row text for `state`.
    pub(crate) label: String,
    /// `None` when there is nothing to explain.
    pub(crate) notice: Option<String>,
    pub(crate) denied: Vec<Denied>,
}

impl AccountSummary {
    pub(crate) fn upload_denied(&self) -> bool {
        self.denied.iter().any(|d| d.capability == "upload")
    }
}

/// Is upload blocked for this account?
///
/// The onboarding document is authoritative whenever it is available: its
/// `upload` capability is the answer, even when the legacy subscription
/// notice disagrees (an `allowance` account is `needs_plan` in the legacy
/// view yet may upload). The legacy notice is consulted ONLY when the
/// document is unavailable.
pub(crate) fn upload_blocked(legacy_notice: Option<&str>, doc: Option<&AccountSummary>) -> bool {
    match doc {
        Some(d) => d.upload_denied(),
        None => legacy_notice.is_some(),
    }
}

/// Why an upload must be refused up front, in words, or `None` to let it
/// proceed. Same rule as [`upload_blocked`]: the document decides when it is
/// available (its notice, else a plain sentence), the legacy notice only when
/// it is not.
pub(crate) fn upload_refusal(legacy_notice: Option<String>, doc: Option<&AccountSummary>) -> Option<String> {
    match doc {
        Some(d) if d.upload_denied() => Some(
            d.notice
                .clone()
                .unwrap_or_else(|| "Your account cannot upload right now.".to_string()),
        ),
        Some(_) => None,
        None => legacy_notice,
    }
}

impl OnboardingDoc {
    /// Parse a response body. `None` unless it is a JSON object of schema
    /// major 1 (the only major this CLI asks for and understands).
    pub(crate) fn parse(body: &Value) -> Option<Self> {
        if body.get("schema").and_then(Value::as_u64) != Some(1) || !body.is_object() {
            return None;
        }
        Some(OnboardingDoc { raw: body.clone() })
    }

    pub(crate) fn stage(&self) -> Option<&str> {
        self.raw.get("stage").and_then(Value::as_str)
    }

    /// The document's `signup` block, if it has one.
    pub(crate) fn signup(&self) -> Option<SignupInfo> {
        let s = self.raw.get("signup").filter(|v| v.is_object())?;
        let str_of = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_string);
        Some(SignupInfo {
            mode: SignupMode::from_wire(s.get("mode").and_then(Value::as_str).unwrap_or("web_only")),
            // A missing `allowed` is not permission.
            allowed: s.get("allowed").and_then(Value::as_bool).unwrap_or(false),
            reason: str_of("reason"),
            web_url: str_of("web_url"),
        })
    }

    /// The `signup` block exactly as the server sent it (for `--json`).
    pub(crate) fn signup_raw(&self) -> Option<&Value> {
        self.raw.get("signup").filter(|v| v.is_object())
    }

    /// `client.status == "update_required"`: this `bb` is too old for the
    /// server. Said in words, never acted on.
    pub(crate) fn update_required(&self) -> bool {
        self.raw.pointer("/client/status").and_then(Value::as_str) == Some("update_required")
    }

    /// The signed-in account, if this is a `stage: account` document with a
    /// state. `app_base` is the web app base (for the "do this on the web"
    /// links), no trailing slash needed.
    pub(crate) fn account_summary(&self, app_base: &str) -> Option<AccountSummary> {
        if self.stage() == Some("pre_account") {
            return None;
        }
        let acct = self.raw.get("account").filter(|v| v.is_object())?;
        let state = DocState::from_wire(acct.get("state").and_then(Value::as_str)?);
        let denied = denied_capabilities(acct);
        let base = app_base.trim_end_matches('/');
        let date_at = |ptr: &str| {
            self.raw
                .pointer(ptr)
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| format_deletion_date(&d.with_timezone(&Utc)))
        };
        let deletion = date_at("/account/lifecycle/data_deletion_at");
        let trial_end = date_at("/account/trial/ends_at");
        let will_be_deleted = |lead: &str| match &deletion {
            Some(d) => format!("{lead} and will be deleted on {d}."),
            None => format!("{lead}."),
        };

        // Deletion is a capability like any other (contract rule 5): when the
        // document denies it, say so and why instead of assuming it is allowed.
        let delete_note = denied
            .iter()
            .find(|d| d.capability == "delete")
            .map(|d| match d.reason.as_deref() {
                Some(r) => format!(" Deleting files is not available right now ({}).", r.replace('_', " ")),
                None => " Deleting files is not available right now.".to_string(),
            })
            .unwrap_or_default();
        let ro_lead = if delete_note.is_empty() {
            "Your vault is read-only: you can download and delete, not upload or share"
        } else {
            "Your vault is read-only: you can download, not upload, share or delete"
        };

        let (label, notice): (String, Option<String>) = match &state {
            DocState::Allowance => ("free allowance".into(), None),
            DocState::LegacyFree => ("free (legacy plan)".into(), None),
            DocState::Active => ("active".into(), None),
            DocState::NeedsPlan => (
                "no plan yet".into(),
                Some(format!(
                    "Your account has no plan yet \u{2014} choose one at {base}/choose-plan"
                )),
            ),
            DocState::Trialing => ("trial".into(), trial_end.map(|d| format!("Your trial runs until {d}."))),
            DocState::TrialingNoCard => (
                "trial (no card)".into(),
                Some(match trial_end {
                    Some(d) => format!(
                        "Your trial runs until {d}. No card is on file, so it will not convert by itself \u{2014} \
                         choose a plan at {base}/choose-plan before then."
                    ),
                    None => format!(
                        "No card is on file, so your trial will not convert by itself \u{2014} choose a plan at {base}/choose-plan."
                    ),
                }),
            ),
            DocState::TrialCancelling => (
                "trial (cancelling)".into(),
                Some(match trial_end {
                    Some(d) => format!("Your trial is cancelled and ends on {d}; it will not convert to a paid plan."),
                    None => "Your trial is cancelled; it will not convert to a paid plan.".to_string(),
                }),
            ),
            DocState::TrialEnded | DocState::Lapsed => (
                "read-only (trial ended)".into(),
                Some(format!(
                    "{}{delete_note} Subscribe at {base}/billing?view=change",
                    will_be_deleted("Your trial has ended; your vault is read-only")
                )),
            ),
            DocState::PastDue => (
                "payment overdue".into(),
                Some(format!(
                    "Your last payment did not go through. Update it at {base}/billing"
                )),
            ),
            DocState::ReadOnly => (
                "read-only".into(),
                Some(format!(
                    "{}{delete_note} Resolve it at {base}/billing",
                    will_be_deleted(ro_lead)
                )),
            ),
            DocState::Frozen => (
                "frozen".into(),
                Some("Your account is frozen. Contact support to find out why and to unfreeze it.".into()),
            ),
            // A state this CLI does not know: the label is only a label, the
            // capabilities decide (contract rule 5).
            DocState::Other(s) => (
                s.replace('_', " "),
                (!denied.is_empty()).then(|| {
                    format!(
                        "Some actions are turned off for this account ({}). Details at {base}",
                        denied
                            .iter()
                            .map(|d| d.capability.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }),
            ),
        };
        Some(AccountSummary {
            state,
            label,
            notice,
            denied,
        })
    }
}

/// Capabilities the document marks `allowed: false`, plus `delete` when the
/// document omits it (absence is not permission). Unknown names are included
/// as sent; ordering follows the document.
fn denied_capabilities(acct: &Value) -> Vec<Denied> {
    let empty = serde_json::Map::new();
    let caps = acct.get("capabilities").and_then(Value::as_object).unwrap_or(&empty);
    let mut out: Vec<Denied> = caps
        .iter()
        .filter(|(_, v)| v.get("allowed").and_then(Value::as_bool) == Some(false))
        .map(|(name, v)| Denied {
            capability: name.clone(),
            reason: v.get("reason").and_then(Value::as_str).map(str::to_string),
        })
        .collect();
    // Contract: a capability the document does not mention is not allowed. An
    // older schema-v1 server may omit `delete`; fail closed with no reason
    // (we do not invent one the server never gave).
    if !caps.contains_key("delete") {
        out.push(Denied {
            capability: "delete".to_string(),
            reason: None,
        });
    }
    out.sort_by(|a, b| a.capability.cmp(&b.capability));
    out
}

/// What `bb signup` says about the document's `signup` block. The destination
/// never depends on it (see the module notes above): the CLI always sends the
/// user to the web app.
pub(crate) fn signup_explanation(info: &SignupInfo) -> String {
    let why = |r: &Option<String>| r.as_deref().map(|r| format!(" ({r})")).unwrap_or_default();
    match (info.allowed, info.mode) {
        // The ordinary answer for a client the policy keeps on the web
        // (`signup_web_only`, task 1741): not a closed door, just the web.
        (false, SignupMode::WebOnly) if info.reason.as_deref() == Some("signup_web_only") => {
            "Accounts are created in the web app; the terminal only signs in.".to_string()
        }
        (false, _) => format!(
            "The server is not accepting new accounts from this client right now{}. \
             The web app is the place to check.",
            why(&info.reason)
        ),
        (true, SignupMode::Native) => {
            "The server would let a native app create an account, but the terminal does not: \
             accounts are created in the web app."
                .to_string()
        }
        (true, SignupMode::WebHandoff) => {
            "Accounts are created in the web app; it hands you back here when you are done.".to_string()
        }
        (true, SignupMode::WebOnly) => "Accounts are created in the web app; the terminal only signs in.".to_string(),
    }
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
    // ── onboarding document (task 1750) ──────────────────────────────────

    use std::path::PathBuf;

    const BASE: &str = "https://app.beebeeb.io";

    fn contract_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts/onboarding")
    }

    /// Every vendored fixture as (file name, parsed JSON), sorted by name.
    fn fixtures() -> Vec<(String, Value)> {
        let mut out: Vec<(String, Value)> = std::fs::read_dir(contract_dir().join("fixtures"))
            .expect("vendored fixtures dir")
            .map(|e| {
                let e = e.unwrap();
                let body = std::fs::read_to_string(e.path()).unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    serde_json::from_str(&body).unwrap(),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn fixture(name: &str) -> Value {
        fixtures()
            .into_iter()
            .find(|(n, _)| n == &format!("{name}.json"))
            .unwrap_or_else(|| panic!("no fixture {name}"))
            .1
    }

    fn summary(name: &str) -> AccountSummary {
        OnboardingDoc::parse(&fixture(name))
            .unwrap()
            .account_summary(BASE)
            .unwrap_or_else(|| panic!("{name} has no account summary"))
    }

    #[test]
    fn every_vendored_fixture_parses_as_schema_major_one() {
        let all = fixtures();
        // The count is derived, not pinned: `scripts/check-onboarding-contract.sh` (workspace
        // root) diffs this directory byte-for-byte against the server's, so the file set IS the
        // server's. A hard-coded N went stale when the contract gained a coupon fixture (task
        // 1814). Guard the real failure instead: an empty or truncated dir (a bad vendor copy).
        let on_disk = std::fs::read_dir(contract_dir().join("fixtures"))
            .expect("vendored fixtures dir")
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "json"))
            .count();
        assert_eq!(all.len(), on_disk, "every *.json fixture on disk must be loaded");
        assert!(
            all.len() >= 12,
            "fewer fixtures ({}) than account states: bad vendor copy",
            all.len()
        );
        for (name, body) in &all {
            assert!(OnboardingDoc::parse(body).is_some(), "{name} must parse");
        }
    }

    #[test]
    fn every_account_state_in_the_schema_has_a_summary_with_the_same_slug() {
        let schema: Value =
            serde_json::from_str(&std::fs::read_to_string(contract_dir().join("schema.v1.json")).unwrap()).unwrap();
        let states: Vec<String> = schema["$defs"]["account"]["properties"]["state"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(states.len(), 12);
        let mut covered = 0;
        for want in &states {
            let (name, body) = fixtures()
                .into_iter()
                .find(|(_, b)| b["account"]["state"] == json!(want))
                .unwrap_or_else(|| panic!("no fixture for {want}"));
            let sum = OnboardingDoc::parse(&body).unwrap().account_summary(BASE).unwrap();
            assert_eq!(sum.state.slug(), want, "{name}");
            assert!(!matches!(sum.state, DocState::Other(_)), "{want} must be a known state");
            assert!(!sum.label.is_empty(), "{want} needs a label");
            covered += 1;
        }
        assert_eq!(covered, 12);
    }

    #[test]
    fn states_that_need_no_action_say_nothing() {
        for name in ["account.active.web", "account.allowance.web", "account.legacy_free.web"] {
            assert_eq!(summary(name).notice, None, "{name}");
        }
    }

    #[test]
    fn trial_ended_names_the_deletion_date_and_points_at_the_web() {
        let s = summary("account.trial_ended.ios");
        assert_eq!(s.state, DocState::TrialEnded);
        assert_eq!(s.label, "read-only (trial ended)");
        assert_eq!(
            s.notice.as_deref(),
            Some(
                "Your trial has ended; your vault is read-only and will be deleted on November 1, 2026. \
                 Subscribe at https://app.beebeeb.io/billing?view=change"
            )
        );
        assert!(s.upload_denied());
        // Downloads and deletes stay allowed in this state.
        assert_eq!(
            s.denied.iter().map(|d| d.capability.as_str()).collect::<Vec<_>>(),
            vec!["share", "upload"]
        );
        assert!(s.denied.iter().all(|d| d.reason.as_deref() == Some("trial_ended")));
    }

    #[test]
    fn no_card_trial_says_it_will_not_convert_and_when_it_ends() {
        let n = summary("account.trialing_no_card.desktop").notice.unwrap();
        assert!(n.contains("October 18, 2026"), "{n}");
        assert!(n.contains("will not convert by itself"), "{n}");
        assert!(n.contains("https://app.beebeeb.io/choose-plan"), "{n}");
    }

    #[test]
    fn frozen_and_read_only_deny_upload_and_explain() {
        let frozen = summary("account.frozen.desktop");
        assert!(frozen.upload_denied());
        assert!(frozen.notice.unwrap().contains("frozen"));
        let ro = summary("account.read_only.web");
        assert!(ro.upload_denied());
        assert!(ro.notice.unwrap().starts_with("Your vault is read-only"));
    }

    #[test]
    fn read_only_says_deletion_is_unavailable_when_the_document_denies_it() {
        let ro = summary("account.read_only.web");
        assert!(ro.denied.iter().any(|d| d.capability == "delete"));
        let n = ro.notice.unwrap();
        assert!(!n.contains("download and delete"), "{n}");
        assert!(
            n.contains("Deleting files is not available right now (billing read only)."),
            "{n}"
        );
        // Allowed -> unchanged wording.
        let ended = summary("account.trial_ended.ios").notice.unwrap();
        assert!(!ended.contains("Deleting files is not available"), "{ended}");
    }

    #[test]
    fn past_due_still_uploads_but_says_the_payment_failed() {
        let s = summary("account.past_due.web");
        assert!(!s.upload_denied());
        assert!(s.notice.unwrap().contains("payment did not go through"));
    }

    #[test]
    fn document_notices_follow_the_web_app_base_they_are_given() {
        let doc = OnboardingDoc::parse(&fixture("account.needs_plan.ios")).unwrap();
        let n = doc.account_summary("http://localhost:5173/").unwrap().notice.unwrap();
        assert!(n.ends_with("http://localhost:5173/choose-plan"), "{n}");
        assert!(!n.contains("beebeeb.io"), "{n}");
    }

    #[test]
    fn unknown_state_is_only_a_label_and_capabilities_decide() {
        let allowed = json!({"schema":1,"stage":"account","account":{"state":"some_future_state",
            "capabilities":{"download":{"allowed":true},"upload":{"allowed":true},"share":{"allowed":true},"delete":{"allowed":true}}}});
        let s = OnboardingDoc::parse(&allowed).unwrap().account_summary(BASE).unwrap();
        assert_eq!(s.state, DocState::Other("some_future_state".into()));
        assert_eq!(s.label, "some future state");
        assert_eq!(s.notice, None);
        assert!(!s.upload_denied());

        let denied = json!({"schema":1,"stage":"account","account":{"state":"some_future_state",
            "capabilities":{"download":{"allowed":true},"upload":{"allowed":false,"reason":"x"},"share":{"allowed":false},"delete":{"allowed":true}}}});
        let s = OnboardingDoc::parse(&denied).unwrap().account_summary(BASE).unwrap();
        assert!(s.upload_denied());
        assert!(s.notice.unwrap().contains("share, upload"));
    }

    #[test]
    fn absent_delete_capability_fails_closed_with_no_invented_reason() {
        // An older schema-v1 server may omit `capabilities.delete`; the
        // contract says absence is not permission.
        let doc = json!({"schema":1,"stage":"account","account":{"state":"read_only",
            "capabilities":{"download":{"allowed":true},"upload":{"allowed":false},"share":{"allowed":false}}}});
        let s = OnboardingDoc::parse(&doc).unwrap().account_summary(BASE).unwrap();
        let d = s
            .denied
            .iter()
            .find(|d| d.capability == "delete")
            .expect("delete denied");
        assert_eq!(d.reason, None);
        let n = s.notice.unwrap();
        assert!(!n.contains("download and delete"), "{n}");
        assert!(n.contains("Deleting files is not available right now."), "{n}");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut body = fixture("account.active.web");
        body["brand_new_top_level"] = json!({"x": 1});
        body["account"]["brand_new"] = json!(true);
        assert!(OnboardingDoc::parse(&body).unwrap().account_summary(BASE).is_some());
        let fwd = fixture("forward_compat.unknown_step.ios");
        assert!(OnboardingDoc::parse(&fwd).is_some());
    }

    #[test]
    fn only_schema_major_one_parses() {
        assert!(OnboardingDoc::parse(&json!({"schema": 2, "stage": "account"})).is_none());
        assert!(OnboardingDoc::parse(&json!({"stage": "account"})).is_none());
        assert!(OnboardingDoc::parse(&json!({"schema": "1"})).is_none());
        assert!(OnboardingDoc::parse(&json!([1])).is_none());
        assert!(OnboardingDoc::parse(&Value::Null).is_none());
    }

    #[test]
    fn a_pre_account_document_has_no_account_summary() {
        let doc = OnboardingDoc::parse(&fixture("pre_account.web")).unwrap();
        assert_eq!(doc.stage(), Some("pre_account"));
        assert!(doc.account_summary(BASE).is_none());
    }

    #[test]
    fn update_required_is_reported() {
        assert!(
            OnboardingDoc::parse(&fixture("client.update_required.ios"))
                .unwrap()
                .update_required()
        );
        assert!(
            !OnboardingDoc::parse(&fixture("account.active.web"))
                .unwrap()
                .update_required()
        );
    }

    // ── signup.mode ──────────────────────────────────────────────────────

    #[test]
    fn signup_block_is_read_from_the_pre_account_fixtures() {
        // Signup is web-only (tasks 1834/1836): only the web fixture offers a native signup.
        for (name, mode, allowed, reason) in [
            ("pre_account.web", SignupMode::Native, true, None),
            ("pre_account.ios", SignupMode::WebOnly, false, Some("signup_web_only")),
            (
                "pre_account.desktop",
                SignupMode::WebOnly,
                false,
                Some("signup_web_only"),
            ),
        ] {
            let info = OnboardingDoc::parse(&fixture(name)).unwrap().signup().unwrap();
            assert_eq!(info.mode, mode, "{name}");
            assert_eq!(info.allowed, allowed, "{name}");
            assert_eq!(info.reason.as_deref(), reason, "{name}");
            assert_eq!(info.web_url.as_deref(), Some("https://app.beebeeb.io/signup"));
        }
    }

    #[test]
    fn signup_mode_parsing_fails_closed() {
        assert_eq!(SignupMode::from_wire("native"), SignupMode::Native);
        assert_eq!(SignupMode::from_wire("web_handoff"), SignupMode::WebHandoff);
        assert_eq!(SignupMode::from_wire("web_only"), SignupMode::WebOnly);
        assert_eq!(SignupMode::from_wire("teleport"), SignupMode::WebOnly);
        assert_eq!(SignupMode::from_wire(""), SignupMode::WebOnly);
        let doc =
            OnboardingDoc::parse(&json!({"schema":1,"stage":"pre_account","signup":{"mode":"teleport"}})).unwrap();
        let info = doc.signup().unwrap();
        assert_eq!(info.mode, SignupMode::WebOnly);
        assert!(!info.allowed, "a missing `allowed` is not permission");
    }

    fn info(mode: SignupMode, allowed: bool, reason: Option<&str>) -> SignupInfo {
        SignupInfo {
            mode,
            allowed,
            reason: reason.map(str::to_string),
            web_url: None,
        }
    }

    #[test]
    fn signup_explanations_never_promise_a_native_flow() {
        let native = signup_explanation(&info(SignupMode::Native, true, None));
        assert!(native.contains("the terminal does not"), "{native}");
        assert!(native.contains("web app"), "{native}");
        assert!(signup_explanation(&info(SignupMode::WebOnly, true, None)).contains("the terminal only signs in"));
        assert!(signup_explanation(&info(SignupMode::WebHandoff, true, None)).contains("hands you back here"));
        // The ordinary web-only policy answer is not worded as a closed door.
        let policy = signup_explanation(&info(SignupMode::WebOnly, false, Some("signup_web_only")));
        assert!(policy.contains("the terminal only signs in"), "{policy}");
        assert!(!policy.contains("not accepting"), "{policy}");
        let closed = signup_explanation(&info(SignupMode::WebOnly, false, Some("signups_paused")));
        assert!(
            closed.contains("not accepting new accounts from this client"),
            "{closed}"
        );
        assert!(closed.contains("(signups_paused)"), "{closed}");
        assert!(!signup_explanation(&info(SignupMode::Native, false, None)).contains("would let"));
    }
}
