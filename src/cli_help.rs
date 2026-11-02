//! The hand-written `bb --help` screen: the curated command rows, the account
//! section, the footer links, and the overflow line that keeps the screen
//! honest as commands are added.
//!
//! Owns: `HELP_*` tables, `build_help_text`, `print_custom_help`, and the
//! `help_screen_tests` that pin the screen to the real clap surface.
//! Must not: parse arguments or run a command.

use super::*;

/// The curated, hand-grouped rows shown in the `COMMANDS` section. Kept as a
/// named const (rather than inline in `build_help_text`) so the "everything
/// else" overflow line below can be computed against it.
pub(super) const HELP_MAIN_COMMANDS: &[(&str, &str, &str)] = &[
    ("ls", "[path]", "list files (decrypts names locally)"),
    ("mkdir", "<path>", "create a folder · mirrors mkdir -p"),
    ("mv", "<src> <dst>", "move or rename · mirrors mv"),
    ("rm", "<path|id>...", "trash files/folders · -r · reversible"),
    ("restore", "<name|id>", "restore from trash"),
    ("trash", "list", "browse the trash"),
    ("search", "<query>", "find files by name · --regex"),
    ("push", "<path>", "upload · encrypts on the fly"),
    ("pull", "<id|path>", "download and decrypt"),
    ("share", "<path|id>", "create encrypted link (expiry, passphrase)"),
    ("sync", "<dir> [remote]", "sync + watch · continuous by default"),
    ("webdav", "", "serve vault over WebDAV (Finder, rclone, Cyberduck)"),
    ("whoami", "", "user · plan · region · quota · session"),
    ("speedtest", "", "benchmark network + crypto speed"),
    ("repair", "", "fix cross-client encryption"),
    ("update", "", "install the latest bb release"),
];

/// The curated `ACCOUNT` section — account-security commands added in 0.10.0
/// (task 1506: these five were shipped but missing from `--help` entirely).
pub(super) const HELP_ACCOUNT_COMMANDS: &[(&str, &str, &str)] = &[
    ("account", "<action>", "profile, plan & security · show, update"),
    ("billing", "<action>", "plan, usage, invoices, portal, add-ons"),
    ("2fa", "<action>", "two-factor auth (TOTP) · setup, enable, disable"),
    ("sessions", "<action>", "active sessions across devices · list, revoke"),
    ("passkey", "<action>", "WebAuthn passkeys · list, add, remove"),
    ("region", "[action]", "where new uploads are stored · list, get, set"),
];

/// Links printed at the foot of every help surface (the custom `bb --help`
/// screen and clap's own rendering). Full https URLs to pages that exist —
/// the previous footer pointed at `beebeeb.io/cli` and
/// `beebeeb.io/fingerprints`, both 404. `help_links_return_200` (run in CI)
/// checks every URL here answers 200.
pub(super) const HELP_FOOTER_LINKS: &[(&str, &str)] = &[
    ("docs", "https://github.com/beebeeb-io/cli"),
    ("security", "https://beebeeb.io/security"),
];

/// The footer block, one `# label  url` line per link, plus a pointer to
/// per-command help (the top-level screen lists no per-command flags).
pub(super) fn help_footer_text() -> String {
    let mut out = String::from("# flags    bb <command> --help");
    for (label, url) in HELP_FOOTER_LINKS {
        out.push_str(&format!("\n# {label:<8} {url}"));
    }
    out
}

/// Every top-level subcommand name the clap tree actually declares
/// (non-hidden). Used both to render the "+ …" overflow line and, in tests,
/// to prove the help screen never drops a command silently again.
pub(super) fn top_level_command_names() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| c.get_name().to_string())
        .collect()
}

/// Builds the full `bb --help` screen as a plain `String` — split out from
/// `print_custom_help` so tests can assert against the text directly instead
/// of capturing stdout.
pub(super) fn build_help_text() -> String {
    use std::fmt::Write as _;

    use crate::{colors, ui};

    let version = env!("CARGO_PKG_VERSION");
    let w = 58;
    let mut out = String::new();

    let _ = writeln!(out, "{}", ui::box_header("BEEBEEB", w));
    let _ = writeln!(
        out,
        "{}",
        ui::box_line(
            &format!(
                "{}",
                "end-to-end encrypted vault, from the terminal".custom_color(colors::INK_DIM)
            ),
            w,
        )
    );
    let _ = writeln!(
        out,
        "{}",
        ui::box_line(
            &format!(
                "v{} · {}",
                version.custom_color(colors::INK_DIM),
                "e2ee".custom_color(colors::GREEN_OK)
            ),
            w,
        )
    );
    let _ = writeln!(out, "{}", ui::box_footer(w));
    let _ = writeln!(out);

    let _ = writeln!(out, "  {}", "COMMANDS".custom_color(colors::AMBER));
    for (name, args, desc) in HELP_MAIN_COMMANDS {
        let _ = writeln!(
            out,
            "  {:<10}{:<18}{}",
            name.custom_color(colors::GREEN_OK),
            args.custom_color(colors::PATH),
            desc.custom_color(colors::INK_DIM)
        );
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "  {}", "ACCOUNT".custom_color(colors::AMBER));
    for (name, args, desc) in HELP_ACCOUNT_COMMANDS {
        let _ = writeln!(
            out,
            "  {:<10}{:<18}{}",
            name.custom_color(colors::GREEN_OK),
            args.custom_color(colors::PATH),
            desc.custom_color(colors::INK_DIM)
        );
    }
    let _ = writeln!(out);

    // Every remaining top-level subcommand, derived from the clap tree
    // itself rather than hand-maintained — a new subcommand can never go
    // missing from --help again (task 1506).
    let curated: std::collections::HashSet<&str> = HELP_MAIN_COMMANDS
        .iter()
        .map(|(name, _, _)| *name)
        .chain(HELP_ACCOUNT_COMMANDS.iter().map(|(name, _, _)| *name))
        .collect();
    let overflow: Vec<String> = top_level_command_names()
        .into_iter()
        .filter(|name| !curated.contains(name.as_str()))
        .collect();
    if !overflow.is_empty() {
        let _ = writeln!(
            out,
            "  {}",
            format!("+ {}", overflow.join(", ")).custom_color(colors::INK_DIM)
        );
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "  {}", "FLAGS".custom_color(colors::AMBER));
    let flags: &[(&str, &str)] = &[
        ("--json", "structured JSON output"),
        ("--quiet", "minimal · no progress"),
        ("--api <url>", "override API endpoint"),
    ];
    for (flag, desc) in flags {
        let _ = writeln!(
            out,
            "  {:<14}{}",
            flag.custom_color(colors::CYAN),
            desc.custom_color(colors::INK_DIM)
        );
    }
    let _ = writeln!(out);
    for line in help_footer_text().lines() {
        let _ = writeln!(out, "  {}", line.custom_color(colors::INK_SAGE));
    }

    out
}

pub(super) fn print_custom_help() {
    // Ensure color system is initialised (ui::init hasn't been called yet).
    ui::init(false, false, false);
    print!("{}", build_help_text());
}

#[cfg(test)]
mod help_screen_tests {
    use super::*;

    /// Task 1506: the hand-written `bb --help` screen listed 24 of the 32
    /// top-level commands and silently dropped `2fa`, `sessions`, `passkey`,
    /// `billing` and `account` — exactly the headline features of the
    /// 0.10.0 release. This asserts every non-hidden name in the clap
    /// command tree shows up SOMEWHERE in the rendered help text (curated
    /// section or the derived overflow line), so a future subcommand can't
    /// go missing the same way.
    #[test]
    fn manual_update_is_an_explicit_command() {
        assert!(
            Cli::try_parse_from(["bb", "update"]).is_ok(),
            "bb update must be recognized"
        );
    }

    #[test]
    fn help_screen_lists_every_top_level_subcommand() {
        // Force plain text so ANSI escape codes never hide a name from the
        // substring/word-boundary check below. Independent of `ui::MODE`
        // (a once-per-process OnceLock) so this is safe next to other tests.
        colored::control::set_override(false);

        let help = build_help_text();

        let missing: Vec<String> = top_level_command_names()
            .into_iter()
            .filter(|name| {
                let pattern = format!(r"\b{}\b", regex::escape(name));
                let re = regex::Regex::new(&pattern).expect("valid word-boundary regex");
                !re.is_match(&help)
            })
            .collect();

        assert!(
            missing.is_empty(),
            "bb --help is missing top-level subcommand(s): {missing:?}\n--- rendered help ---\n{help}"
        );
    }

    /// Every link-shaped token (`host.tld/path`, with or without a scheme)
    /// in a rendered help screen. Deliberately matches BARE domains too, so a
    /// footer written as `beebeeb.io/cli` is caught, not just full URLs.
    fn help_urls(text: &str) -> Vec<String> {
        let re = regex::Regex::new(r"(?:https?://)?(?:[a-z0-9-]+\.)+[a-z]{2,}(?:/[A-Za-z0-9._~/#-]*)?")
            .expect("valid url regex");
        re.find_iter(text).map(|m| m.as_str().to_string()).collect()
    }

    /// Both help surfaces: the custom top-level screen (`bb`, `bb --help`)
    /// and clap's own rendering (reached via e.g. `bb --json --help`), which
    /// carries the `after_help` footer.
    fn all_help_text() -> String {
        colored::control::set_override(false);
        format!("{}\n{}", build_help_text(), Cli::command().render_long_help())
    }

    /// Pages checked to answer HTTP 200 on 2026-09-25 (flow-6 issue 8:
    /// `beebeeb.io/cli` and `beebeeb.io/fingerprints` were 404). The ignored
    /// network test `help_links_return_200` re-checks them live in CI.
    const CHECKED_LIVE_URLS: &[&str] = &["https://github.com/beebeeb-io/cli", "https://beebeeb.io/security"];

    #[test]
    fn help_links_are_full_urls_to_checked_live_pages() {
        let help = all_help_text();
        let urls = help_urls(&help);
        assert!(
            !urls.is_empty(),
            "help footer must link somewhere; found no URLs in:\n{help}"
        );
        let bad: Vec<&String> = urls
            .iter()
            .filter(|u| !CHECKED_LIVE_URLS.contains(&u.as_str()))
            .collect();
        assert!(bad.is_empty(), "help links not in the checked-live list: {bad:?}");
    }

    /// Network check: every link printed by any help surface answers 200.
    /// Ignored by default (needs the internet); CI runs it with `--ignored`.
    #[tokio::test]
    #[ignore = "network: run with cargo test help_links_return_200 -- --ignored"]
    async fn help_links_return_200() {
        let help = all_help_text();
        let urls = help_urls(&help);
        assert!(!urls.is_empty(), "no URLs extracted from help");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent("beebeeb-cli-help-link-check")
            .build()
            .expect("http client");
        let mut failures = Vec::new();
        let mut checked = 0usize;
        for u in &urls {
            let full = if u.starts_with("http") {
                u.clone()
            } else {
                format!("https://{u}")
            };
            match client.get(&full).send().await {
                Ok(r) if r.status().as_u16() == 200 => checked += 1,
                Ok(r) => failures.push(format!("{full} -> {}", r.status())),
                Err(e) => failures.push(format!("{full} -> {e}")),
            }
        }
        println!("help link check: {checked} of {} URLs answered 200", urls.len());
        assert!(failures.is_empty(), "dead help links: {failures:?}");
    }

    /// The custom top-level screen lists no per-command flags; the README
    /// must not promise that it does.
    #[test]
    fn readme_points_flag_reference_at_subcommand_help() {
        let readme = include_str!("../README.md");
        assert!(
            !readme.contains("including every flag: `bb --help`"),
            "README still claims `bb --help` shows every flag"
        );
        assert!(
            readme.contains("`bb <command> --help`"),
            "README must point the flag reference at `bb <command> --help`"
        );
    }

    /// Sanity check on the helper itself: it must actually see the full
    /// clap tree (guards against a future refactor that swaps in a stale or
    /// partial `Command`).
    #[test]
    fn top_level_command_names_is_not_suspiciously_short() {
        let names = top_level_command_names();
        assert!(
            names.len() >= 30,
            "expected ~32 top-level subcommands, got {}: {names:?}",
            names.len()
        );
    }
}
