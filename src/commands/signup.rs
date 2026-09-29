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

use colored::Colorize;

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

    match plan_signup(ui::is_json(), ui::is_quiet(), env_detect::is_headless()) {
        SignupAction::Json => {
            println!(
                "{}",
                serde_json::json!({
                    "url": url,
                    "next": "bb login",
                    "note": "accounts are created in the web app; the CLI only signs in",
                })
            );
        }
        SignupAction::PrintUrl => println!("{url}"),
        SignupAction::Headless => {
            println!();
            println!("  {}", signup_message(&url).custom_color(colors::INK));
            println!();
        }
        SignupAction::Open => {
            println!();
            println!("  {}", signup_message(&url).custom_color(colors::INK));
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
