//! Turning a server-provided name into a local path, safely (task 1733).
//!
//! Every file and folder name the CLI writes to disk comes from the server (or,
//! for file-request uploads, from an anonymous stranger: the uploader seals the
//! name under a content key *they* picked, so the decrypted name is fully
//! attacker-controlled). `out_dir.join(name)` is not safe on such input:
//!
//! - an absolute name (`/etc/cron.d/x`) REPLACES `out_dir` in `Path::join`;
//! - `..` components climb out of `out_dir`;
//! - a name with a separator creates (and writes into) arbitrary subtrees;
//! - a pre-existing symlink inside `out_dir` redirects the write elsewhere.
//!
//! Every place that maps a remote name to a local path goes through this
//! module: [`safe_child`] for one name under a directory (`bb pull`),
//! [`safe_rel_join`] for a `/`-separated remote-relative path (`bb sync`), and
//! [`default_output_path`] for the cwd-relative default of `bb pull <id>`.
//!
//! Two rule sets ([`Rules`]):
//!
//! - **Portable** (`bb pull`): what a pulled name must satisfy on any host.
//!   Pulled trees are artefacts users copy around, so names that are separators
//!   or drive prefixes on *some* platform are refused everywhere.
//! - **Host** (`bb sync`): only what is dangerous on THIS host. Sync mirrors a
//!   tree both ways and must keep working for legitimate local names that were
//!   uploaded earlier (a Linux file named `a\b`); refusing those would make the
//!   remote copy look deleted and trigger a local delete. On Windows hosts this
//!   is the full Windows rule set.
//!
//! This is client-local logic. Shared path-safety logic belongs in
//! `beebeeb-core` eventually (desktop has its own copy of this problem, see
//! task 1735); core has no such helper at the pinned rev, and this lane does
//! not change core.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Which rule set to apply; see the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rules {
    /// Reject anything that is a separator, drive prefix or control character
    /// on any supported platform. Used by `bb pull`.
    Portable,
    /// Reject only what is dangerous on the current host. Used by `bb sync`.
    Host,
}

/// Why a name was refused. `Display` gives the short reason shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeName(pub &'static str);

impl fmt::Display for UnsafeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Windows device names that cannot be used as file names (with or without an
/// extension).
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Check one name (a single path component, never a path).
pub fn check_component(name: &str, rules: Rules) -> Result<(), UnsafeName> {
    check_component_for(name, rules == Rules::Portable, cfg!(windows))
}

/// Testable core of [`check_component`]. `portable` = the strict cross-platform
/// rules; `windows` = this host applies Windows path semantics.
fn check_component_for(name: &str, portable: bool, windows: bool) -> Result<(), UnsafeName> {
    if name.is_empty() {
        return Err(UnsafeName("empty name"));
    }
    if name == "." || name == ".." {
        return Err(UnsafeName("'.' and '..' are not file names"));
    }
    if name.contains('\0') {
        return Err(UnsafeName("contains a NUL byte"));
    }
    if name.contains('/') {
        return Err(UnsafeName("contains a path separator"));
    }
    if portable || windows {
        if name.contains('\\') {
            return Err(UnsafeName("contains a path separator"));
        }
        if name.chars().any(|c| c.is_control()) {
            return Err(UnsafeName("contains a control character"));
        }
        let b = name.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return Err(UnsafeName("starts with a Windows drive prefix"));
        }
    }
    if windows {
        if name.contains(':') {
            return Err(UnsafeName("contains ':' (drive prefix or alternate data stream)"));
        }
        if name.chars().any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
            return Err(UnsafeName("contains a character Windows does not allow"));
        }
        if name.ends_with('.') || name.ends_with(' ') {
            return Err(UnsafeName("ends with a dot or space (Windows strips it)"));
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        if WINDOWS_RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(UnsafeName("is a reserved Windows device name"));
        }
    }
    Ok(())
}

/// Bidirectional-control characters (embeddings, overrides, isolates). They are
/// not control characters to `char::is_control`, but a name containing one can
/// render as a different name (`evil\u{202e}fdp.exe` shows as `evilexe.pdf`).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

/// One name as it may appear INSIDE AN ARCHIVE (`bb pull --zip`).
///
/// An archive is extracted somewhere else, often on Windows, so the host's own
/// rules are not enough. The structural checks of [`Rules::Portable`] still
/// REFUSE the name (separators, `.`/`..`, control characters, drive prefixes:
/// those are traversal, not cosmetics). Everything else that Windows or a
/// renderer would mangle is REWRITTEN instead, so the file is not lost:
///
/// - `: < > " | ? *` become `_` (a `:` is an NTFS alternate data stream);
/// - trailing dots and spaces become `_` (Windows silently strips them);
/// - a reserved device name (`CON`, `NUL`, `COM1`, ... with or without an
///   extension) gets a leading `_`;
/// - bidi overrides are removed.
///
/// Rewriting can make two names equal; the caller de-duplicates afterwards.
pub fn archive_component(name: &str) -> Result<String, UnsafeName> {
    check_component(name, Rules::Portable)?;
    let mut out: String = name
        .chars()
        .filter(|c| !is_bidi_control(*c))
        .map(|c| {
            if matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = out.trim_end_matches(['.', ' ']).len();
    if trimmed != out.len() {
        let pad = "_".repeat(out.len() - trimmed);
        out.truncate(trimmed);
        out.push_str(&pad);
    }
    let stem = out.split('.').next().unwrap_or(&out).trim_end();
    if WINDOWS_RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        out.insert(0, '_');
    }
    // The result must satisfy the strictest rule set; if some shape slipped
    // through the rewrite, refuse rather than emit it.
    check_component_for(&out, true, true)?;
    Ok(out)
}

/// Canonicalise the deepest EXISTING ancestor of `path` (resolving symlinks),
/// then re-append the not-yet-existing tail. The tail may only contain plain
/// components; anything else (`..`, a root) means the caller skipped validation.
fn canonicalize_deepest(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(_) => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| format!("{}: no existing ancestor", path.display()))?;
                tail.push(name);
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("{}: no existing ancestor", path.display()))?;
                if existing.as_os_str().is_empty() {
                    existing = Path::new(".");
                    break;
                }
            }
        }
    }
    let mut out = existing
        .canonicalize()
        .map_err(|e| format!("{}: cannot resolve: {e}", existing.display()))?;
    for c in tail.iter().rev() {
        out.push(c);
    }
    Ok(out)
}

/// Assert that `path`, with every symlink on its way resolved, stays inside
/// `root`. `path` need not exist yet (its deepest existing ancestor is resolved).
pub fn ensure_contained(root: &Path, path: &Path) -> Result<(), String> {
    let root_c = canonicalize_deepest(root)?;
    let path_c = canonicalize_deepest(path)?;
    if path_c.starts_with(&root_c) {
        Ok(())
    } else {
        Err(format!(
            "{} resolves to {}, outside {}",
            path.display(),
            path_c.display(),
            root_c.display()
        ))
    }
}

/// Refuse a symlink at `path` itself (a write there would go through it).
fn reject_symlink(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(format!("{} is a symlink; refusing to write through it", path.display()))
        }
        _ => Ok(()),
    }
}

/// `dir/name` for ONE server-provided name: the name is validated as a single
/// safe component, the target must not be a symlink, and its resolved location
/// must stay inside `dir`. The returned path is the plain join (not the
/// canonical form), so messages show what the user would expect.
pub fn safe_child(dir: &Path, name: &str, rules: Rules) -> Result<PathBuf, String> {
    check_component(name, rules).map_err(|e| format!("unsafe name {name:?} ({e})"))?;
    let target = dir.join(name);
    // Belt and braces: a validated name is exactly one normal component.
    debug_assert!(matches!(
        Path::new(name).components().collect::<Vec<_>>().as_slice(),
        [Component::Normal(_)]
    ));
    reject_symlink(&target)?;
    ensure_contained(dir, &target)?;
    Ok(target)
}

/// `base/rel` for a `/`-separated remote-relative path (what `bb sync` keeps as
/// map keys): every segment is validated, and the resolved location must stay
/// inside `base` (so a symlinked subdirectory cannot redirect the write).
pub fn safe_rel_join(base: &Path, rel: &str, rules: Rules) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("unsafe name \"\" (empty path)".to_string());
    }
    let mut target = base.to_path_buf();
    for seg in rel.split('/') {
        check_component(seg, rules).map_err(|e| format!("unsafe name {rel:?} ({e})"))?;
        target.push(seg);
    }
    reject_symlink(&target)?;
    ensure_contained(base, &target)?;
    Ok(target)
}

/// Default output path for `bb pull <id>` without `-o`: the server-provided
/// name, relative to the current directory. An explicit `-o` is the user's own
/// choice and never goes through here.
pub fn default_output_path(name: &str) -> Result<PathBuf, String> {
    check_component(name, Rules::Portable).map_err(|e| {
        format!("the server-provided file name {name:?} is unsafe ({e}); pass -o <path> to choose where to save it")
    })?;
    let path = PathBuf::from(name);
    reject_symlink(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-1733-{tag}-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn rejects_the_task_1733_fixture_names_portably() {
        for bad in [
            "../x",
            "../../x",
            "/etc/x",
            "/abs/path",
            "a/../../x",
            "a/b",
            "..\\x",
            "a\\..\\b",
            "C:\\x",
            "C:x",
            "z:",
            "",
            ".",
            "..",
            "a\0b",
            "line\nbreak",
            "esc\u{1b}[31m",
        ] {
            assert!(
                check_component(bad, Rules::Portable).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn accepts_ordinary_names_portably() {
        for ok in [
            "report.pdf",
            "rapport Q3 \u{00e9}\u{00e8}.pdf",
            ".gitignore",
            "...",
            "..hidden",
            "a b c",
            "2026-10-04T10:30:00.log",
            "name.with.dots.tar.gz",
            "\u{65e5}\u{672c}\u{8a9e}.txt",
            "(1)",
        ] {
            assert!(check_component(ok, Rules::Portable).is_ok(), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn host_rules_on_unix_hosts_only_refuse_what_escapes() {
        // Sync keeps working for odd-but-legal local names; the dangerous four stay refused.
        for (name, windows, ok) in [
            ("a\\b", false, true),
            ("tab\there", false, true),
            ("C:x", false, true),
            ("a\\b", true, false),
            ("..", false, false),
            ("a/b", false, false),
            ("", false, false),
            ("a\0b", false, false),
        ] {
            assert_eq!(
                check_component_for(name, false, windows).is_ok(),
                ok,
                "host rules for {name:?} (windows={windows})"
            );
        }
    }

    #[test]
    fn windows_rules_refuse_reserved_names_streams_and_trailing_dots() {
        for bad in [
            "CON",
            "con",
            "NUL.txt",
            "aux.tar.gz",
            "COM1",
            "lpt9.log",
            "file.txt:stream",
            "a:b",
            "name.",
            "name ",
            "a<b",
            "a|b",
            "q?",
            "star*",
        ] {
            assert!(
                check_component_for(bad, false, true).is_err(),
                "{bad:?} must be rejected on a Windows host"
            );
        }
        for ok in ["console.txt", "com10", "nul_1", "report.pdf"] {
            assert!(
                check_component_for(ok, false, true).is_ok(),
                "{ok:?} must be accepted on a Windows host"
            );
        }
    }

    #[test]
    fn safe_child_stays_inside_the_directory() {
        let dir = tmp("child");
        let p = safe_child(&dir, "ok.txt", Rules::Portable).unwrap();
        assert_eq!(p, dir.join("ok.txt"));
        for bad in ["../x", "/etc/x", "a/../../x", "..", "."] {
            assert!(safe_child(&dir, bad, Rules::Portable).is_err(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn safe_child_refuses_a_preexisting_symlink_even_to_a_harmless_target() {
        let dir = tmp("symlink-child");
        let outside = tmp("symlink-outside");
        std::os::unix::fs::symlink(&outside, dir.join("sub")).unwrap();
        std::os::unix::fs::symlink(outside.join("f"), dir.join("f")).unwrap();
        assert!(safe_child(&dir, "sub", Rules::Portable).is_err());
        assert!(safe_child(&dir, "f", Rules::Portable).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(unix)]
    #[test]
    fn safe_rel_join_refuses_a_symlinked_subdirectory_escape() {
        let base = tmp("rel-base");
        let outside = tmp("rel-outside");
        std::os::unix::fs::symlink(&outside, base.join("docs")).unwrap();
        // Name is innocent, but docs/ leads out of base.
        assert!(safe_rel_join(&base, "docs/report.pdf", Rules::Host).is_err());
        // A real subdirectory (and a not-yet-existing one) is fine.
        std::fs::create_dir_all(base.join("real")).unwrap();
        assert!(safe_rel_join(&base, "real/report.pdf", Rules::Host).is_ok());
        assert!(safe_rel_join(&base, "new/deeper/report.pdf", Rules::Host).is_ok());
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn safe_rel_join_validates_every_segment() {
        let base = tmp("rel-seg");
        for bad in ["../x", "a/../x", "a//b", "/abs", "a/./b", ""] {
            assert!(safe_rel_join(&base, bad, Rules::Host).is_err(), "{bad:?}");
        }
        assert_eq!(
            safe_rel_join(&base, "a/b/c.txt", Rules::Host).unwrap(),
            base.join("a").join("b").join("c.txt")
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_output_path_explains_the_escape_hatch() {
        assert_eq!(default_output_path("note.txt").unwrap(), PathBuf::from("note.txt"));
        let err = default_output_path("../../.ssh/authorized_keys").unwrap_err();
        assert!(err.contains("unsafe") && err.contains("-o <path>"), "{err}");
    }

    #[test]
    fn ensure_contained_catches_dotdot_even_when_unvalidated() {
        let dir = tmp("contained");
        std::fs::create_dir_all(dir.join("in")).unwrap();
        assert!(ensure_contained(&dir.join("in"), &dir.join("in").join("a").join("b")).is_ok());
        assert!(ensure_contained(&dir.join("in"), &dir.join("in").join("..").join("x")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_component_rewrites_what_windows_or_a_renderer_would_mangle() {
        for (input, want) in [
            ("plain.txt", "plain.txt"),
            ("Q3 \u{00e9}.pdf", "Q3 \u{00e9}.pdf"),
            ("CON", "_CON"),
            ("con.txt", "_con.txt"),
            ("NUL.tar.gz", "_NUL.tar.gz"),
            ("COM1", "_COM1"),
            ("LPT9.log", "_LPT9.log"),
            ("report.", "report_"),
            ("trail ", "trail_"),
            ("dots...", "dots___"),
            ("ab:c", "ab_c"),
            ("x.txt:stream", "x.txt_stream"),
            ("q?<>|*\".txt", "q______.txt"),
            ("evil\u{202e}fdp.exe", "evilfdp.exe"),
            ("iso\u{2066}late", "isolate"),
            ("console.txt", "console.txt"),
        ] {
            assert_eq!(archive_component(input).as_deref(), Ok(want), "{input:?}");
        }
    }

    #[test]
    fn archive_component_still_refuses_what_is_traversal() {
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "..\\x",
            "C:x",
            "C:\\x",
            "a\0b",
            "line\nbreak",
            "/abs",
        ] {
            assert!(archive_component(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn archive_component_output_always_passes_the_strictest_rules() {
        for input in ["CON", "a:b", "x. ", "\u{202e}", "..a", "COM1.", " ", "nul "] {
            if let Ok(out) = archive_component(input) {
                assert!(check_component_for(&out, true, true).is_ok(), "{input:?} -> {out:?}");
            }
        }
    }
}
