//! README-vs-clap drift guard.
//!
//! The README once told users to run `bb share <id> --double-encrypted`, a
//! flag the shipped `bb` rejects with "unexpected argument". This test pulls
//! every `bb …` invocation and every `--flag` that goes with it out of
//! README.md and feeds each one through clap's real parser
//! (`try_get_matches_from`). A flag or subcommand that clap does not know is a
//! failure.
//!
//! What counts as an invocation:
//! - a line inside a fenced code block that starts with `bb ` (trailing `# …`
//!   comments are dropped);
//! - an inline code span outside code blocks that starts with `bb `
//!   (tables, bullets, prose).
//!
//! Flags attached to an invocation: `--flag` tokens inside the invocation
//! itself, plus any backticked `` `--flag` `` later on the same line (the
//! parenthetical flag lists in the Commands table). They belong to the
//! nearest `bb …` span to their left. A backticked `--flag` on a line with
//! no `bb …` span before it (security prose) must be defined by at least one
//! bb (sub)command.
//!
//! Placeholders: `<a|b|c>` expands to one invocation per alternative, any
//! other `<x>` becomes a dummy positional value, and `[x]` is dropped.

use clap::CommandFactory;
use clap::error::ErrorKind;

const README: &str = include_str!("../README.md");

/// One `bb` invocation from the README, with the flags that go with it.
#[derive(Debug, Clone)]
struct Invocation {
    line_no: usize,
    /// argv after `bb`, placeholders resolved.
    args: Vec<String>,
    /// extra flags to try, one at a time, on top of `args`.
    flags: Vec<String>,
    /// A flag named in prose with no `bb …` span to attach to.
    prose: bool,
}

fn is_flag(tok: &str) -> bool {
    tok.len() > 2 && tok.starts_with("--") && tok[2..].chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Split `bb push <path> [--x]` into argv variants (a `<a|b>` placeholder
/// multiplies them). Returns (variants, flags found inside the span).
fn expand(span: &str) -> (Vec<Vec<String>>, Vec<String>) {
    let span = span.replace("\\|", "|");
    let mut variants: Vec<Vec<String>> = vec![Vec::new()];
    let mut flags = Vec::new();
    let mut prev_was_flag = false;
    // Skip the leading `bb`.
    for tok in span.split_whitespace().skip(1) {
        let after_flag = std::mem::replace(&mut prev_was_flag, false);
        if tok.starts_with('#') {
            break; // shell comment
        }
        if tok.starts_with('[') {
            // Optional part; pick up any flag named inside it, drop the rest.
            let inner = tok.trim_matches(|c| c == '[' || c == ']');
            if is_flag(inner) {
                flags.push(inner.to_string());
            }
            continue;
        }
        if is_flag(tok) {
            flags.push(tok.to_string());
            prev_was_flag = true;
            continue;
        }
        if tok.starts_with('-') {
            continue; // short flag or odd token: not what this guard checks
        }
        if tok.starts_with('<') && tok.ends_with('>') {
            if after_flag {
                continue; // `--flag <value>`: the value placeholder, not a positional
            }
            let inner = &tok[1..tok.len() - 1];
            if inner.contains('|') {
                let alts: Vec<&str> = inner.split('|').collect();
                variants = variants
                    .into_iter()
                    .flat_map(|v| {
                        alts.iter().map(move |a| {
                            let mut v = v.clone();
                            v.push((*a).to_string());
                            v
                        })
                    })
                    .collect();
            } else {
                for v in &mut variants {
                    v.push("x".to_string());
                }
            }
            continue;
        }
        for v in &mut variants {
            v.push(tok.to_string());
        }
    }
    (variants, flags)
}

/// Every inline code span on a line, in order.
fn code_spans(line: &str) -> Vec<&str> {
    line.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s)
        .collect()
}

fn extract(readme: &str) -> Vec<Invocation> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (i, raw) in readme.lines().enumerate() {
        let line_no = i + 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if trimmed.starts_with("bb ") || trimmed == "bb" {
                let (variants, flags) = expand(trimmed);
                for args in variants {
                    out.push(Invocation {
                        line_no,
                        args,
                        flags: flags.clone(),
                        prose: false,
                    });
                }
            }
            continue;
        }
        // Outside code blocks: inline spans. A flag span attaches to the
        // most recent `bb …` span on the same line.
        let mut current: Vec<Invocation> = Vec::new();
        for span in code_spans(raw) {
            let s = span.trim();
            // A bare `bb` names the tool in prose; it is not an invocation.
            if s.starts_with("bb ") {
                out.append(&mut current);
                let (variants, flags) = expand(s);
                current = variants
                    .into_iter()
                    .map(|args| Invocation {
                        line_no,
                        args,
                        flags: flags.clone(),
                        prose: false,
                    })
                    .collect();
            } else if is_flag(s) {
                if current.is_empty() {
                    // A flag named in prose with no `bb …` span before it on
                    // the line: it must at least exist somewhere in the tree.
                    out.push(Invocation {
                        line_no,
                        args: Vec::new(),
                        flags: vec![s.to_string()],
                        prose: true,
                    });
                } else {
                    for inv in &mut current {
                        inv.flags.push(s.to_string());
                    }
                }
            }
        }
        out.append(&mut current);
    }
    out
}

/// Run one argv through clap. `Err` only when clap does not recognise a
/// subcommand or argument — a missing value or a required positional is a
/// usage detail, not README drift.
fn clap_accepts(argv: &[String]) -> Result<(), String> {
    let full: Vec<&str> = std::iter::once("bb").chain(argv.iter().map(String::as_str)).collect();
    match crate::Cli::command().try_get_matches_from(&full) {
        Ok(_) => Ok(()),
        Err(e) => match e.kind() {
            ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => {
                Err(format!("`{}`: {}", full.join(" "), e.render()))
            }
            _ => Ok(()),
        },
    }
}

fn flag_defined_anywhere(cmd: &clap::Command, long: &str) -> bool {
    // clap adds these lazily, so they are not in get_arguments() yet.
    if long == "help" || long == "version" {
        return true;
    }
    let here = cmd
        .get_arguments()
        .any(|a| a.get_long() == Some(long) || a.get_all_aliases().is_some_and(|al| al.contains(&long)));
    here || cmd.get_subcommands().any(|c| flag_defined_anywhere(c, long))
}

fn check(readme: &str) -> (usize, Vec<String>) {
    let mut checked = 0;
    let mut failures = Vec::new();
    for inv in extract(readme) {
        if inv.prose {
            // Bare prose flag: accept if any (sub)command defines it.
            for flag in &inv.flags {
                checked += 1;
                if !flag_defined_anywhere(&crate::Cli::command(), &flag[2..]) {
                    failures.push(format!(
                        "README.md:{}: `{flag}` is not a flag of any bb command",
                        inv.line_no
                    ));
                }
            }
            continue;
        }
        checked += 1;
        if let Err(e) = clap_accepts(&inv.args) {
            failures.push(format!("README.md:{}: {e}", inv.line_no));
        }
        for flag in &inv.flags {
            checked += 1;
            let mut argv = inv.args.clone();
            argv.push(flag.clone());
            if let Err(e) = clap_accepts(&argv) {
                failures.push(format!("README.md:{}: {e}", inv.line_no));
            }
        }
    }
    (checked, failures)
}

#[test]
fn readme_flags_exist() {
    let (checked, failures) = check(README);
    eprintln!("readme_flags_exist: {checked} README invocations/flags checked against clap");
    // Prove the extractor actually found the README's commands: a parser
    // change that silently matches nothing must not pass.
    assert!(
        checked >= 30,
        "only {checked} README invocations/flags checked — extractor is broken"
    );
    assert!(
        failures.is_empty(),
        "README.md names bb commands/flags that clap rejects:\n{}",
        failures.join("\n")
    );
}

/// The guard itself must be able to go red: the exact line that used to be
/// in the README, and a made-up subcommand, both have to fail.
#[test]
fn readme_flags_guard_catches_drift() {
    let bad = "| `bb share <file-id>` | Create a link (`--expires`, `--double-encrypted`) |\n\
               ```sh\nbb frobnicate ./x\n```\n\
               - Prose: `--double-encrypted` wraps the key; `--passphrase` gates it.\n";
    let (checked, failures) = check(bad);
    assert_eq!(
        checked, 6,
        "expected share + 2 flags + frobnicate + 2 prose flags, got {checked}"
    );
    assert_eq!(failures.len(), 3, "{failures:?}");
    assert!(failures[0].contains("--double-encrypted"), "{failures:?}");
    assert!(failures[1].contains("frobnicate"), "{failures:?}");
    assert!(
        failures[2].contains("README.md:5") && failures[2].contains("--double-encrypted"),
        "{failures:?}"
    );
}

#[test]
fn readme_flags_expands_alternatives() {
    let (variants, _) = expand("bb billing <show\\|usage\\|portal>");
    assert_eq!(
        variants,
        vec![
            vec!["billing", "show"],
            vec!["billing", "usage"],
            vec!["billing", "portal"]
        ]
    );
    let (variants, flags) = expand("bb billing invoices --open <id>");
    assert_eq!(variants, vec![vec!["billing", "invoices"]]);
    assert_eq!(flags, vec!["--open"]);
}
