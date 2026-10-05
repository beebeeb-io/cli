//! `bb signup` — points at the web signup; the CLI never creates accounts.
//!
//! Task 1037: there are no free accounts any more. Signing up starts a trial
//! that needs a payment mandate (a Mollie card or iDEAL first payment), and
//! that checkout only runs in the web app. Mobile and the CLI therefore don't
//! create accounts: `bb signup` prints the web signup URL (derived from the
//! configured API, see `crate::web_url`), best-effort opens it in a browser,
//! and exits 0. Login stays native (`bb login`).
//!
//! The command name is kept so muscle memory and older docs still land
//! somewhere useful.
//!
//! Task 1750: the server's onboarding document (`GET /api/v1/onboarding`,
//! anonymous, so `stage: "pre_account"`) is read for `signup.mode` and the
//! command says what it found. It changes WORDS only, never the destination:
//! the CLI is never lifted for native signup (policy matrix, task 1741), so a
//! `native` answer is explained, not obeyed. Older server, or any fetch
//! failure: the behaviour above, unchanged.

use colored::Colorize;

use crate::account_state::{OnboardingDoc, signup_explanation};

/// The anonymous onboarding document, or `None` when the server is older than
/// the endpoint, unreachable, or answers something that is not schema major 1.
/// Never fails the command: signup guidance does not depend on it.
async fn fetch_document() -> Option<OnboardingDoc> {
    let api = crate::api::ApiClient::from_config();
    let body = api.get_onboarding(false).await.ok()??;
    OnboardingDoc::parse(&body)
}

/// The web app's signup route.
const SIGNUP_PATH: &str = "/signup";

/// `{app_base}/signup`.
pub(crate) fn signup_url(app_base: &str) -> String {
    format!("{}{SIGNUP_PATH}", app_base.trim_end_matches('/'))
}

/// The one line `bb signup` prints.
pub(crate) fn signup_message(url: &str) -> String {
    format!("Create your account at {url} \u{2014} then run `bb login`.")
}

/// What `run` should do, given the output mode and environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignupAction {
    /// `--json`: `{ "url", "next" }`, never touch the browser.
    Json,
    /// `--quiet`: the bare URL, never touch the browser.
    PrintUrl,
    /// No display reachable (SSH, bare TTY): print the message only.
    Headless,
    /// Print the message and best-effort open the browser.
    Open,
}

fn plan_signup(is_json: bool, is_quiet: bool, headless: bool) -> SignupAction {
    if is_json {
        SignupAction::Json
    } else if is_quiet {
        SignupAction::PrintUrl
    } else if headless {
        SignupAction::Headless
    } else {
        SignupAction::Open
    }
}

pub async fn run() -> Result<(), String> {
    use crate::{colors, env_detect, ui};

    let url = signup_url(&crate::web_url::web_app_base());
    let doc = fetch_document().await;
    let explanation = doc.as_ref().and_then(|d| d.signup()).map(|i| signup_explanation(&i));
    let update_hint = doc
        .as_ref()
        .is_some_and(OnboardingDoc::update_required)
        .then_some("This version of bb is too old for the server. Update bb, then run `bb login`.");

    match plan_signup(ui::is_json(), ui::is_quiet(), env_detect::is_headless()) {
        SignupAction::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "url": url,
                    "next": "bb login",
                    "note": "accounts are created in the web app; the CLI only signs in",
                    // The server's `signup` block verbatim, `null` when the
                    // server has no onboarding document (task 1750).
                    "signup": doc.as_ref().and_then(|d| d.signup_raw()),
                })
            );
        }
        SignupAction::PrintUrl => println!("{url}"),
        SignupAction::Headless => {
            println!();
            println!("  {}", signup_message(&url).custom_color(colors::INK));
            print_extra(explanation.as_deref(), update_hint);
            println!();
        }
        SignupAction::Open => {
            println!();
            println!("  {}", signup_message(&url).custom_color(colors::INK));
            print_extra(explanation.as_deref(), update_hint);
            // Best-effort — the URL is already printed, so a failed launch
            // (no default browser) still leaves something to click or paste.
            if open::that(&url).is_ok() {
                println!("  {}", "opened in your browser".custom_color(colors::INK_DIM));
            }
            println!();
        }
    }

    Ok(())
}

fn print_extra(explanation: Option<&str>, update_hint: Option<&str>) {
    for line in [explanation, update_hint].into_iter().flatten() {
        println!("  {}", line.custom_color(crate::colors::INK_DIM));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signup_url_appends_the_signup_route() {
        assert_eq!(signup_url("https://app.beebeeb.io"), "https://app.beebeeb.io/signup");
    }

    #[test]
    fn signup_url_follows_a_local_web_app() {
        assert_eq!(signup_url("http://localhost:5173"), "http://localhost:5173/signup");
    }

    #[test]
    fn signup_url_for_the_default_api_is_the_production_signup() {
        // Default config API → app.beebeeb.io (the spec's fallback).
        let base = crate::web_url::resolve_web_app_base(None, "https://api.beebeeb.io");
        assert_eq!(signup_url(&base), "https://app.beebeeb.io/signup");
    }

    #[test]
    fn signup_message_names_the_url_and_the_next_step() {
        assert_eq!(
            signup_message("https://app.beebeeb.io/signup"),
            "Create your account at https://app.beebeeb.io/signup \u{2014} then run `bb login`."
        );
    }

    #[test]
    fn plan_signup_json_wins_and_never_opens_a_browser() {
        assert_eq!(plan_signup(true, true, false), SignupAction::Json);
        assert_eq!(plan_signup(true, false, true), SignupAction::Json);
    }

    #[test]
    fn plan_signup_quiet_prints_just_the_url() {
        assert_eq!(plan_signup(false, true, false), SignupAction::PrintUrl);
        assert_eq!(plan_signup(false, true, true), SignupAction::PrintUrl);
    }

    #[test]
    fn plan_signup_headless_does_not_open() {
        assert_eq!(plan_signup(false, false, true), SignupAction::Headless);
    }

    #[test]
    fn plan_signup_interactive_opens_the_browser() {
        assert_eq!(plan_signup(false, false, false), SignupAction::Open);
    }
}
