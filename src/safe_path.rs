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
//! The rules live in `beebeeb_core::path_safety` (spec §16.2), so the CLI,
//! desktop and web share one answer; this module re-exports them under the
//! names the CLI call sites already use, and keeps the CLI's fixture tests.
//!
//! Windows also resolves a name through a folder's 8.3 short names (task 2408):
//! the three helpers above refuse such a name, and a write by a name built
//! anywhere else calls [`short_name_alias`] (or [`refuse_short_name_alias`])
//! first, or writes through [`TempSibling`].

pub use beebeeb_core::path_safety::{
    Rules, TempSibling, archive_component, check_component, default_output_path, safe_child, safe_rel_join,
    short_name_alias,
};

/// Refuse a write (a create or a replace) to `path` whose last component
/// reaches a DIFFERENT existing entry through another of its names: on Windows
/// an 8.3 short name, `REPORT~1.PDF` for `Report for 2026.pdf` (task 2408).
/// Replacing it would replace that other file. The folder part is the
/// caller's choice and is not looked at. The refusal names both entries;
/// other hosts answer without touching the disk.
pub fn refuse_short_name_alias(path: &std::path::Path) -> Result<(), String> {
    let Some(name) = path.file_name() else {
        return Ok(());
    };
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => std::path::Path::new("."),
    };
    match short_name_alias(dir, std::path::Path::new(name)) {
        Ok(None) => Ok(()),
        Ok(Some(conflict)) => Err(conflict.to_string()),
        Err(e) => Err(format!(
            "{}: cannot check the name against the existing entries ({e})",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beebeeb_core::path_safety::{check_component_for, ensure_contained};
    use std::path::PathBuf;

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
            "..hidden",
            "a b c",
            "2026-10-04T10:30:00.log",
            "name.with.dots.tar.gz",
            "\u{65e5}\u{672c}\u{8a9e}.txt",
            "(1)",
        ] {
            assert!(check_component(ok, Rules::Portable).is_ok(), "{ok:?} must be accepted");
        }
        // core v0.2.8: Rules::Portable is host-independent; check_component never passes windows=true,
        // so `...` (ends in a dot) is accepted everywhere.  The write-path guard inside safe_child
        // calls check_component_to_write, which does apply Windows' trailing-dot rule, refusing `...`
        // on Windows while allowing it on Unix.
        assert!(check_component("...", Rules::Portable).is_ok());
        let dir = tmp("dots");
        if cfg!(windows) {
            assert!(
                safe_child(&dir, "...", Rules::Portable).is_err(),
                "... must be refused on Windows"
            );
        } else {
            assert!(
                safe_child(&dir, "...", Rules::Portable).is_ok(),
                "... must be allowed on Unix"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
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
