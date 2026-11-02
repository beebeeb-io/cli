//! Turning a `bb pull` argument into a concrete target: an id prefix or a
//! remote path, plus the overwrite question that must be asked at most once per
//! file even when the download is retried.
//!
//! Owns: `resolve_file_arg`, `resolve_as_path`, `resolve_request_key`,
//! `looks_like_id_prefix`, `ask_once`, `guard_existing_output`,
//! `confirm_overwrite`.
//! Must not: touch the network beyond resolving a path (see `pull::folder`).
//! Tests: `failure_tests` at the bottom of this file.

use super::*;

/// Resolve a user-supplied file reference to `(full_uuid, resolved_name)`.
///
/// Accepts, in order: a full UUID (used as-is, name `None`), a hex short-ID
/// prefix such as the 8-char IDs `bb ls` prints (falls through to path
/// resolution when no file matches), or a plaintext vault path like
/// `folder1/a.bin`. Folders and the vault root are rejected. Shared by
/// `bb pull` and `bb share` so both accept exactly the same references.
pub(crate) async fn resolve_file_arg(api: &ApiClient, arg: &str) -> Result<(String, Option<String>), String> {
    if uuid::Uuid::parse_str(arg).is_ok() {
        // Already a full UUID — use it directly.
        return Ok((arg.to_string(), None));
    }
    if looks_like_id_prefix(arg) {
        // Looks like a hex prefix (e.g. "3e15382b" from `bb ls` output).
        if let Some(full_id) = api.find_file_by_id_prefix(arg).await? {
            return Ok((full_id, None));
        }
        // No match by prefix — fall through to path resolution.
    }
    resolve_as_path(api, arg).await
}

/// The overwrite question, asked unless it was already answered: a second attempt after a version
/// change must not put the same question to the user again.
pub(super) fn ask_once(
    already_confirmed: bool,
    out_path: &std::path::Path,
    force: bool,
    guard: impl FnOnce(&std::path::Path, bool) -> Result<(), String>,
) -> Result<(), String> {
    if already_confirmed {
        Ok(())
    } else {
        guard(out_path, force)
    }
}

/// Refuse to overwrite an existing local path unless the user said so.
///
/// Before this guard `bb pull note.txt` replaced a local `note.txt` with the
/// remote content — exit 0, no prompt, no backup — so unsaved local edits were
/// lost. Now:
/// - nothing at `out_path` → proceed;
/// - a directory at `out_path` → always an error (a file cannot replace it);
/// - `--force` → proceed (the atomic `.tmp` + rename replaces the file);
/// - a rich terminal with a real tty on stdin → `y/N` prompt (default no);
/// - otherwise (`--json`, `--quiet`, piped/redirected stdin, scripts) → refuse
///   with a non-zero exit, naming `--force` and `-o <path>`. A piped `y` is not
///   consent (same rule as `bb passkey remove`, Codex PR #24).
pub(super) fn guard_existing_output(out_path: &std::path::Path, force: bool) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(out_path) {
        Ok(m) => m,
        // Not there (or unreadable metadata): nothing to clobber here; any real
        // I/O problem surfaces when the file is written.
        Err(_) => return Ok(()),
    };
    let shown = out_path.display();
    if meta.is_dir() {
        return Err(format!(
            "{shown} is a directory — pass -o <path> to choose where to save the file"
        ));
    }
    if force {
        return Ok(());
    }
    if crate::ui::is_rich() && std::io::stdin().is_terminal() {
        if confirm_overwrite(out_path)? {
            return Ok(());
        }
        return Err(format!("{shown} left unchanged — nothing downloaded"));
    }
    Err(format!(
        "{shown} already exists — use --force to overwrite it, or -o <path> to save elsewhere"
    ))
}

/// Minimal interactive y/N confirmation (no extra deps; default no). Only
/// called on a rich terminal with a real tty on stdin.
pub(super) fn confirm_overwrite(out_path: &std::path::Path) -> Result<bool, String> {
    use std::io::Write;

    print!(
        "  {} {} already exists. Overwrite? [y/N] ",
        "?".custom_color(crate::colors::AMBER),
        out_path.display().to_string().custom_color(crate::colors::INK)
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

/// Returns `true` if the string looks like a UUID prefix: 8-36 hex chars
/// (with optional hyphens). This covers the 8-char short IDs shown by
/// `bb ls` as well as longer partial UUIDs.
pub(super) fn looks_like_id_prefix(s: &str) -> bool {
    let len = s.len();
    (8..=36).contains(&len)
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && s.chars().any(|c| c.is_ascii_hexdigit())
}

/// Resolve a plaintext vault path (e.g. "/Documents/report.pdf") into a
/// `(file_id, Some(name))` pair.
pub(super) async fn resolve_as_path(api: &ApiClient, path: &str) -> Result<(String, Option<String>), String> {
    let master_key = load_master_key()?;
    let resolved = crate::path::resolve_path(api, &master_key, path).await?;

    if resolved.file_id.is_none() {
        return Err("cannot download the vault root".to_string());
    }
    if resolved.is_folder {
        return Err(format!(
            "'{}' is a folder. Use `bb ls` to list its contents.",
            resolved.name,
        ));
    }

    Ok((resolved.file_id.unwrap(), Some(resolved.name)))
}

/// Resolve a row's request content key, lazily loading the shared resolver the
/// first time a request-uploaded file is seen in the tree.
pub(super) async fn resolve_request_key(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file: &serde_json::Value,
    request_keys: &mut Option<crate::commands::request::RequestKeyResolver>,
) -> Option<beebeeb_core::kdf::FileKey> {
    if !crate::commands::request::is_request_upload(file) {
        return None;
    }
    if request_keys.is_none() {
        *request_keys = Some(match crate::commands::request::RequestKeyResolver::load(api).await {
            Ok(rk) => rk,
            Err(e) => {
                // Say so once; an empty resolver stops every later request
                // upload from retrying (and failing silently) on its own.
                eprintln!(
                    "  {} could not load your file-request keys ({e}); files received through a file request cannot be decrypted in this run",
                    "!".custom_color(crate::colors::AMBER)
                );
                crate::commands::request::RequestKeyResolver::empty()
            }
        });
    }
    request_keys.as_mut().and_then(|rk| rk.content_key(master_key, file))
}
#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::download::DownloadError;

    #[test]
    fn only_a_version_change_is_retried() {
        assert!(matches!(
            PullFailure::from(DownloadError::Changed("m".into())),
            PullFailure::Changed(m) if m == "m"
        ));
        for other in [DownloadError::Unreadable("u".into()), DownloadError::Other("o".into())] {
            let text = other.to_string();
            assert!(matches!(PullFailure::from(other), PullFailure::Other(m) if m == text));
        }
        assert_eq!(String::from(PullFailure::Changed("c".into())), "c");
        assert_eq!(String::from(PullFailure::Other("o".into())), "o");
    }

    #[test]
    fn the_overwrite_question_is_asked_once() {
        let asked = std::cell::Cell::new(0u32);
        let guard = |_: &std::path::Path, _: bool| {
            asked.set(asked.get() + 1);
            Ok(())
        };
        let out = std::path::Path::new("out.bin");
        ask_once(false, out, false, guard).unwrap();
        assert_eq!(asked.get(), 1, "the first attempt asks");
        ask_once(true, out, false, |_: &std::path::Path, _: bool| -> Result<(), String> {
            asked.set(asked.get() + 1);
            Err("asked again".into())
        })
        .unwrap();
        assert_eq!(asked.get(), 1, "the retry does not");
        assert!(ask_once(false, out, true, |_: &std::path::Path, _: bool| Err("no".to_string())).is_err());
    }
}
