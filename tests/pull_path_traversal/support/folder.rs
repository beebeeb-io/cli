//! One behaviour group of the same integration suite.

use super::*;

/// Control: a folder pull of ordinary request uploads still works (including a
/// nested real subfolder and a non-ASCII name), so the refusals below are not
/// a broken fixture.
#[test]
fn folder_pull_happy_path_still_works() {
    let mut fx = Fixture::new();
    fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
    fx.add_request_file(ROOT_ID, "rapport Q3 \u{00e9}\u{00e8}.pdf", b"unicode name\n");
    let sub = fx.add_request_folder(ROOT_ID, "sub");
    fx.add_request_file(&sub, "inner.txt", b"nested\n");
    fx.add_plain_file(ROOT_ID, "plain.txt", b"plain body\n");
    let url = fx.serve();
    let s = Scratch::new("happy", &url);

    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(std::fs::read(s.out_dir().join("ok.txt")).unwrap(), BENIGN);
    assert_eq!(
        std::fs::read(s.out_dir().join("rapport Q3 \u{00e9}\u{00e8}.pdf")).unwrap(),
        b"unicode name\n"
    );
    assert_eq!(std::fs::read(s.out_dir().join("sub/inner.txt")).unwrap(), b"nested\n");
    assert_eq!(std::fs::read(s.out_dir().join("plain.txt")).unwrap(), b"plain body\n");
}

/// Task 1943: a file whose `name_encrypted` is plaintext carries no
/// authenticated chunk-format pin, so its content is never read. In a folder
/// pull it is skipped with a warning (like an unsafe name), nothing is written
/// for it, and its sibling still pulls; the run exits 3 (skipped items).
#[test]
fn folder_pull_skips_a_file_without_authenticated_metadata() {
    let mut fx = Fixture::new();
    fx.add_plaintext_named_file(ROOT_ID, "no-pin.txt", b"never read\n");
    fx.add_plain_file(ROOT_ID, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("no-pin", &url);

    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(!s.out_dir().join("no-pin.txt").exists(), "{}", describe(&out));
    assert_eq!(std::fs::read(s.out_dir().join("ok.txt")).unwrap(), BENIGN);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no-pin.txt") && err.contains("refusing to guess"),
        "{}",
        describe(&out)
    );
}

/// Malicious FILE names, from a request upload (attacker-chosen content key)
/// and from a plaintext-name row. Each case: the hostile entry sits first, a
/// benign sibling second. Required: nothing appears outside `out/`, nothing
/// hostile lands inside `out/` either, the benign sibling is still pulled, and
/// the run exits 3 (finished with skipped items) naming the unsafe name.
#[test]
fn folder_pull_rejects_hostile_file_names() {
    // Scratch layout: <root>/cwd/out is the output dir, so `../../x` lands in
    // <root>, `a/../../x` lands in <root>/cwd, and an absolute path is built
    // per-case from the scratch root.
    type NameFn = Box<dyn Fn(&Path) -> String>;
    let cases: Vec<(&str, NameFn)> = vec![
        ("dotdot_dotdot", Box::new(|_| "../../escape.txt".to_string())),
        ("dotdot_one", Box::new(|_| "../escape.txt".to_string())),
        ("nested_dotdot", Box::new(|_| "a/../../x".to_string())),
        (
            "absolute",
            Box::new(|root| root.join("abs-escape.txt").display().to_string()),
        ),
        ("subdir_separator", Box::new(|_| "dir/file.txt".to_string())),
        ("backslash_dotdot", Box::new(|_| "..\\x".to_string())),
        ("windows_drive", Box::new(|_| "C:\\x".to_string())),
        ("windows_drive_relative", Box::new(|_| "C:x".to_string())),
        ("single_dot", Box::new(|_| ".".to_string())),
        ("double_dot", Box::new(|_| "..".to_string())),
        ("empty", Box::new(|_| String::new())),
        ("nul_byte", Box::new(|_| "a\0b".to_string())),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (kind, mk_name) in &cases {
        for source in ["request", "plain"] {
            let label = format!("{kind}/{source}");
            // The absolute-path case needs the scratch root, which needs the
            // scratch dir, which needs the server URL: create the scratch first
            // against a placeholder URL, then repoint its config at the mock.
            let placeholder = Scratch::new(&format!("{kind}-{source}"), "http://127.0.0.1:1");
            let name = mk_name(&placeholder.root);

            let mut fx = Fixture::new();
            if source == "request" {
                fx.add_request_file(ROOT_ID, &name, PAYLOAD);
            } else {
                fx.add_plain_file(ROOT_ID, &name, PAYLOAD);
            }
            fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
            placeholder.point_at(&fx.serve());

            let before = placeholder.snapshot_outside_out();
            let out = placeholder.bb(&["pull", ROOT_ID, "-o", "out"]);
            let after = placeholder.snapshot_outside_out();

            let escaped: Vec<_> = after.difference(&before).cloned().collect();
            if !escaped.is_empty() {
                failures.push(format!("{label}: wrote OUTSIDE out/: {escaped:?}\n{}", describe(&out)));
                continue;
            }
            let entries = placeholder.out_entries();
            let stray: Vec<_> = entries.iter().filter(|e| e.as_str() != "ok.txt").cloned().collect();
            if !stray.is_empty() {
                failures.push(format!(
                    "{label}: hostile entry created inside out/: {stray:?}\n{}",
                    describe(&out)
                ));
                continue;
            }
            match std::fs::read(placeholder.out_dir().join("ok.txt")) {
                Ok(b) if b == BENIGN => {}
                other => {
                    failures.push(format!(
                        "{label}: benign sibling not pulled: {other:?}\n{}",
                        describe(&out)
                    ));
                    continue;
                }
            }
            if out.status.code() != Some(3) {
                failures.push(format!(
                    "{label}: expected exit 3, got {:?}\n{}",
                    out.status.code(),
                    describe(&out)
                ));
                continue;
            }
            if !String::from_utf8_lossy(&out.stderr).contains("unsafe") {
                failures.push(format!(
                    "{label}: stderr does not name the unsafe name\n{}",
                    describe(&out)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n\n{}",
        failures.len(),
        cases.len() * 2,
        failures.join("\n\n")
    );
}

/// A hostile FOLDER name (`..`, `../..`, absolute) must not be descended into:
/// its children would otherwise be written into the parent directory.
#[test]
fn folder_pull_rejects_hostile_folder_names() {
    let mut failures: Vec<String> = Vec::new();
    for (kind, name) in [("dotdot", ".."), ("dotdot_two", "../.."), ("slash", "x/y")] {
        let mut fx = Fixture::new();
        let evil = fx.add_request_folder(ROOT_ID, name);
        fx.add_request_file(&evil, "inner.txt", PAYLOAD);
        fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
        let url = fx.serve();
        let s = Scratch::new(&format!("folder-{kind}"), &url);

        let before = s.snapshot_outside_out();
        let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);
        let after = s.snapshot_outside_out();

        let escaped: Vec<_> = after.difference(&before).cloned().collect();
        if !escaped.is_empty() {
            failures.push(format!("{kind}: wrote OUTSIDE out/: {escaped:?}\n{}", describe(&out)));
            continue;
        }
        let stray: Vec<_> = s.out_entries().into_iter().filter(|e| e != "ok.txt").collect();
        if !stray.is_empty() {
            failures.push(format!(
                "{kind}: hostile entry inside out/: {stray:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        if out.status.code() != Some(3) {
            failures.push(format!(
                "{kind}: expected exit 3, got {:?}\n{}",
                out.status.code(),
                describe(&out)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A pre-existing symlink inside the output dir must not be followed out of it,
/// even though the folder name itself (`sub`) is perfectly innocent.
#[cfg(unix)]
#[test]
fn folder_pull_does_not_follow_preexisting_symlink_out_of_target() {
    let mut fx = Fixture::new();
    let sub = fx.add_request_folder(ROOT_ID, "sub");
    fx.add_request_file(&sub, "inner.txt", PAYLOAD);
    fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("symlink-dir", &url);

    let outside = s.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(s.out_dir()).unwrap();
    std::os::unix::fs::symlink(&outside, s.out_dir().join("sub")).unwrap();

    let before = s.snapshot_outside_out();
    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);
    let after = s.snapshot_outside_out();

    let escaped: Vec<_> = after.difference(&before).cloned().collect();
    assert!(
        escaped.is_empty(),
        "followed the symlink out of out/: {escaped:?}\n{}",
        describe(&out)
    );
    assert!(!outside.join("inner.txt").exists(), "{}", describe(&out));
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert_eq!(std::fs::read(s.out_dir().join("ok.txt")).unwrap(), BENIGN);
}

/// Same, for a symlink sitting where a FILE is about to be written: the write
/// must not go through it to its target.
#[cfg(unix)]
#[test]
fn folder_pull_does_not_write_through_preexisting_file_symlink() {
    let mut fx = Fixture::new();
    fx.add_request_file(ROOT_ID, "link.txt", PAYLOAD);
    fx.add_request_file(ROOT_ID, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("symlink-file", &url);

    let victim = s.root.join("victim.txt");
    std::fs::write(&victim, LOCAL).unwrap();
    std::fs::create_dir_all(s.out_dir()).unwrap();
    std::os::unix::fs::symlink(&victim, s.out_dir().join("link.txt")).unwrap();

    // --force must not turn a symlink into a write-through either.
    let out = s.bb(&["pull", ROOT_ID, "-o", "out", "--force"]);

    assert_eq!(
        std::fs::read(&victim).unwrap(),
        LOCAL,
        "the symlink target was overwritten\n{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
}

/// `guard_existing_output` must also fire on the request-key branch of a folder
/// pull: an attacker-named file must not silently replace a local file of the
/// same name (without --force); with --force it is the user's explicit choice.
#[test]
fn folder_pull_request_branch_refuses_to_overwrite_without_force() {
    let mut fx = Fixture::new();
    fx.add_request_file(ROOT_ID, "note.txt", PAYLOAD);
    let url = fx.serve();
    let s = Scratch::new("guard", &url);
    std::fs::create_dir_all(s.out_dir()).unwrap();
    std::fs::write(s.out_dir().join("note.txt"), LOCAL).unwrap();

    let out = s.bb(&["pull", ROOT_ID, "-o", "out"]);
    assert_eq!(
        std::fs::read(s.out_dir().join("note.txt")).unwrap(),
        LOCAL,
        "existing local file was overwritten without --force\n{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--force"),
        "{}",
        describe(&out)
    );

    let forced = s.bb(&["pull", ROOT_ID, "-o", "out", "--force"]);
    assert!(forced.status.success(), "{}", describe(&forced));
    assert_eq!(std::fs::read(s.out_dir().join("note.txt")).unwrap(), PAYLOAD);
}
