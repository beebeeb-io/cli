//! Where the Beebeeb **web app** lives for the API this CLI is pointed at.
//!
//! Several commands hand the user a web-app link instead of doing the work
//! in the terminal: `bb passkey add` (WebAuthn only runs in a browser),
//! `bb signup` (accounts are created on the web — task 1037: signup needs a
//! payment mandate, which only the web checkout can collect), and the
//! account-state notices (`needs_plan` → `/choose-plan`, `lapsed` →
//! `/billing`). They all derive the host here, from the configured API URL,
//! so a CLI pointed at a local or staging server never hands out a
//! production link.
//!
//! Resolution order ([`resolve_web_app_base`]):
//! 1. `APP_URL` (env, non-empty) — the same override `bb share` / `bb request`
//!    already honour for the links they build.
//! 2. Derived from the API URL ([`web_app_base_from_api`]). The default
//!    config (`https://api.beebeeb.io`) resolves to `https://app.beebeeb.io`.

/// Pull just the host (no scheme, no port, no path) out of an API base URL.
/// Used instead of a whole-string substring search so a custom domain that
/// merely *contains* "localhost" as a label (`api.localhost.example.com`)
/// isn't misidentified as the local dev API (Codex review, PR #23).
pub(crate) fn host_of(api_url: &str) -> &str {
    let without_scheme = api_url
        .strip_prefix("https://")
        .or_else(|| api_url.strip_prefix("http://"))
        .unwrap_or(api_url);
    let host_and_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    // IPv6 literals are bracketed (`[::1]:3001`) — don't split those on ':'.
    if let Some(rest) = host_and_port.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    host_and_port.split(':').next().unwrap_or(host_and_port)
}

/// Derive the web app's base URL (scheme + host, no trailing slash) from a
/// configured API base URL, never a hard-coded production host. Pure
/// function of the API URL string so it's unit-testable without touching the
/// real (possibly-live) on-disk config.
///
/// - `localhost`/`127.0.0.1`/`::1` (any port, matched on the parsed host —
///   not a substring search) → the local dev web app, `localhost:5173`
///   (`repos/web` `bun dev` default — see `repos/web/CLAUDE.md`).
/// - `https://api.<host>` / `http://api.<host>` → the same scheme + host
///   with `api.` swapped for `app.` — the convention every other
///   Beebeeb-operated environment (prod `api.beebeeb.io` → `app.beebeeb.io`,
///   and any future staging `api.<env>.beebeeb.io` → `app.<env>.beebeeb.io`)
///   already follows.
/// - Anything else (an API host with no recognizable `api.` prefix) → the
///   API's own scheme+host, so an unrecognized `--api` never silently
///   resolves to Beebeeb's production web app. It may well 404, but a 404
///   is honest; a link to the wrong company's data is not.
pub(crate) fn web_app_base_from_api(api_url: &str) -> String {
    let api = api_url.trim_end_matches('/');
    let host = host_of(api);

    if host == "localhost" || host == "127.0.0.1" || host == "::1" {
        return "http://localhost:5173".to_string();
    }
    if let Some(rest) = api.strip_prefix("https://api.") {
        return format!("https://app.{rest}");
    }
    if let Some(rest) = api.strip_prefix("http://api.") {
        return format!("http://app.{rest}");
    }
    api.to_string()
}

/// The web-app base for an explicit `APP_URL` override (if any) and the
/// configured API URL. Pure — see the module docs for the order.
pub(crate) fn resolve_web_app_base(app_url_env: Option<&str>, api_url: &str) -> String {
    match app_url_env.map(str::trim).filter(|s| !s.is_empty()) {
        Some(app_url) => app_url.trim_end_matches('/').to_string(),
        None => web_app_base_from_api(api_url),
    }
}

/// The web-app base for this process: `APP_URL`, else derived from the
/// configured API URL (respecting `--api`, same as every other command).
pub(crate) fn web_app_base() -> String {
    let config = crate::config::load_config();
    resolve_web_app_base(std::env::var("APP_URL").ok().as_deref(), &config.api_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prod_api_maps_to_prod_web_app() {
        assert_eq!(
            web_app_base_from_api("https://api.beebeeb.io"),
            "https://app.beebeeb.io"
        );
    }

    #[test]
    fn local_api_maps_to_local_dev_web_app() {
        assert_eq!(web_app_base_from_api("http://localhost:3001"), "http://localhost:5173");
    }

    #[test]
    fn host_of_strips_scheme_port_and_path() {
        assert_eq!(host_of("https://api.beebeeb.io:8443/foo"), "api.beebeeb.io");
        assert_eq!(host_of("http://localhost:3001"), "localhost");
        assert_eq!(host_of("api.beebeeb.io"), "api.beebeeb.io");
    }

    // ── resolve_web_app_base (task 1037) ─────────────────────────────────

    #[test]
    fn resolve_without_override_derives_from_the_default_api() {
        // No APP_URL, default config API → the production web app.
        assert_eq!(
            resolve_web_app_base(None, "https://api.beebeeb.io"),
            "https://app.beebeeb.io"
        );
    }

    #[test]
    fn resolve_without_override_follows_a_staging_api() {
        assert_eq!(
            resolve_web_app_base(None, "https://api.staging.example.net/"),
            "https://app.staging.example.net"
        );
    }

    #[test]
    fn resolve_prefers_an_explicit_app_url_and_trims_its_trailing_slash() {
        assert_eq!(
            resolve_web_app_base(Some("https://web.example.org/"), "https://api.beebeeb.io"),
            "https://web.example.org"
        );
    }

    #[test]
    fn resolve_ignores_an_empty_or_blank_app_url() {
        assert_eq!(
            resolve_web_app_base(Some(""), "https://api.beebeeb.io"),
            "https://app.beebeeb.io"
        );
        assert_eq!(
            resolve_web_app_base(Some("   "), "http://localhost:3001"),
            "http://localhost:5173"
        );
    }
}
