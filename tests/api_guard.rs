//! The guard that keeps a test run off production (safety finding, 2026-10-25).
//!
//! The CLI's API URL comes from `--api`, the config file, or the built-in default `https://api.beebeeb.io`.
//! `BB_API_BASE` is NOT read by the CLI, so a test that runs the binary with a scratch HOME and names no
//! server points it at production. A harness therefore sets `BB_REFUSE_NON_LOOPBACK_API=1` on every run of
//! the binary, and `config::load_config` exits 2 for a URL that is not on this machine before anything is sent.
//!
//! These tests also point every proxy variable at a closed local port, so that even a regression of the guard
//! cannot put a request on the network: the worst it can do is fail to connect to 127.0.0.1:9.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch_home(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("api-guard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".config")).unwrap();
    dir
}

/// `bb <args>` in a scratch HOME with NO config file, behind a proxy that goes nowhere.
fn bb(home: &Path, args: &[&str], guard: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("BB_NO_UPDATE", "1")
        .env("NO_COLOR", "1")
        .env_remove("BB_TRANSPORT")
        .stdin(Stdio::null());
    for var in [
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "ALL_PROXY",
        "https_proxy",
        "http_proxy",
        "all_proxy",
    ] {
        cmd.env(var, "http://127.0.0.1:9");
    }
    cmd.env_remove("NO_PROXY").env_remove("no_proxy");
    match guard {
        Some(value) => {
            cmd.env("BB_REFUSE_NON_LOOPBACK_API", value);
        }
        None => {
            cmd.env_remove("BB_REFUSE_NON_LOOPBACK_API");
        }
    }
    cmd.output().expect("run bb")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_run_that_names_no_server_stops_before_calling_the_default() {
    let home = scratch_home("default");
    let out = bb(&home, &["ls"], Some("1"));
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("Refusing to contact api.beebeeb.io"), "{err}");
    assert!(err.contains("BB_REFUSE_NON_LOOPBACK_API"), "{err}");
    assert!(err.contains("--api"), "it says how to name a local server: {err}");
}

#[test]
fn a_run_that_names_another_public_server_is_refused_too() {
    let home = scratch_home("named");
    let out = bb(&home, &["--api", "https://api.example.org", "ls"], Some("1"));
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("Refusing to contact api.example.org"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_run_that_names_a_server_on_this_machine_is_not_refused() {
    let home = scratch_home("local");
    // Nothing listens on port 9: the run fails to connect, which is NOT the guard.
    let out = bb(&home, &["--api", "http://127.0.0.1:9", "ls"], Some("1"));
    assert!(!stderr(&out).contains("Refusing to contact"), "{}", stderr(&out));
}

#[test]
fn without_the_variable_nothing_changes_for_a_user() {
    let home = scratch_home("off");
    // A user who names a public server is not refused; the proxy sends the request nowhere.
    let out = bb(&home, &["--api", "https://api.example.org", "ls"], None);
    assert!(!stderr(&out).contains("Refusing to contact"), "{}", stderr(&out));
    // "0" and an empty value mean off as well.
    for value in ["0", "", "  "] {
        let out = bb(&home, &["--api", "https://api.example.org", "ls"], Some(value));
        assert!(
            !stderr(&out).contains("Refusing to contact"),
            "{value:?}: {}",
            stderr(&out)
        );
    }
}

// ───────────────────────── the scan: every spawn of `bb` is on the guard ─────────────────────────
//
// A test that runs the binary without BB_REFUSE_NON_LOOPBACK_API would point it at production the day its scratch
// config is missing. The scan is fail-closed: it reads the code of every file under `tests/` (comments removed,
// strings kept), finds each `Command::new(env!("CARGO_BIN_EXE_bb"))`, and requires `.env("BB_REFUSE_NON_LOOPBACK_API", ..)`
// between that call and the point where its function runs the command (`.output(`, `.spawn(`, `.status(`; the end of
// the function when it only builds the command). Any other use of `CARGO_BIN_EXE_bb` (an alias, a variable) is an
// offender too, because the scan could not see where it ends up.

/// `source` with comments removed (line, doc and nested block comments). String, raw string and char literals are kept
/// as they are, so a `//` inside a URL does not hide the rest of its line.
fn without_comments(source: &str) -> String {
    let b = source.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(b' ');
        } else if c == b'r' && (i == 0 || !ident(b[i - 1])) && matches!(b.get(i + 1), Some(b'"' | b'#')) {
            // r"..." / r#"..."#: copy through the closing quote with the same number of hashes
            let mut j = i + 1;
            let mut hashes = 0;
            while b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                j += 1;
                loop {
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                out.extend_from_slice(&b[i..j.min(b.len())]);
                i = j;
            } else {
                out.push(c);
                i += 1;
            }
        } else if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            out.extend_from_slice(&b[start..i]);
        } else if c == b'\'' {
            // a char literal ('x', '\n', '\u{..}') or a lifetime ('a): only a literal is copied through
            let literal_end = if b.get(i + 1) == Some(&b'\\') {
                b[i + 2..].iter().position(|&x| x == b'\'').map(|p| i + 2 + p + 1)
            } else if b.get(i + 2) == Some(&b'\'') {
                Some(i + 3)
            } else {
                None
            };
            match literal_end {
                Some(end) => {
                    out.extend_from_slice(&b[i..end.min(b.len())]);
                    i = end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `code` with every run of whitespace removed (so `Command :: new (\n env!(..))` compares equal to the plain form).
fn squeezed(code: &str) -> String {
    code.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The byte range of the body of the innermost `fn` that contains `at`, in `code`.
fn enclosing_fn_body(code: &str, at: usize) -> Option<(usize, usize)> {
    let b = code.as_bytes();
    let mut best: Option<(usize, usize)> = None;
    let mut from = 0;
    while let Some(rel) = code[from..].find("fn ") {
        let kw = from + rel;
        from = kw + 3;
        if kw > 0 && (b[kw - 1].is_ascii_alphanumeric() || b[kw - 1] == b'_') {
            continue;
        }
        // the first `{` after the signature (a `;` first means a declaration without a body)
        let Some(open_rel) = code[kw..].find(['{', ';']) else {
            continue;
        };
        let open = kw + open_rel;
        if b[open] == b';' {
            continue;
        }
        let mut depth = 0;
        let mut close = None;
        for (j, &c) in b.iter().enumerate().skip(open) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { continue };
        if open < at && at < close && best.is_none_or(|(o, _)| o < open) {
            best = Some((open, close));
        }
    }
    best
}

/// What is wrong with the spawns of `bb` in one test file's source; empty when every one is on the guard.
fn unguarded_spawns(source: &str) -> Vec<String> {
    // Any mention of the cargo-provided binary path, however it is spelled (`CARGO_BIN_EXE_` + a name built with
    // `concat!` is still a mention of this prefix).
    const TOKEN: &str = "CARGO_BIN_EXE_";
    const SPAWN: &str = "Command::new(env!(\"CARGO_BIN_EXE_bb\"))";
    // The guard, with the value the binary reads as "on".
    const GUARD: &str = ".env(\"BB_REFUSE_NON_LOOPBACK_API\",\"1\")";
    // What takes the guard away again, whenever it comes after the guard and before the run.
    const UNGUARDS: [&str; 3] = [".env_clear()", ".env_remove(\"BB_REFUSE_NON_LOOPBACK_API\"", ".envs("];
    let code = without_comments(source);
    let mut problems = Vec::new();
    // every `Command::new(` and what it is given, whitespace-insensitively
    let mut spawns = 0;
    let mut from = 0;
    while let Some(rel) = code[from..].find("Command") {
        let start = from + rel;
        from = start + "Command".len();
        let rest = &code[start..];
        let after_new = squeezed(&rest.chars().take(80).collect::<String>());
        if !after_new.starts_with("Command::new(") {
            continue;
        }
        // the call's own argument list
        let open = start + rest.find('(').unwrap_or(0);
        let mut depth = 0;
        let mut end = open;
        for (j, c) in code[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + j + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let call = squeezed(&code[start..end]);
        if call.contains(TOKEN) {
            if call != SPAWN {
                problems.push(format!("a spawn of bb that is not the plain `{SPAWN}`: `{call}`"));
                continue;
            }
            spawns += 1;
            let line = code[..start].matches('\n').count() + 1;
            // A spawn outside any fn body (a macro_rules! body, a static, a const) cannot be followed to its run.
            let Some((_, body_end)) = enclosing_fn_body(&code, end) else {
                problems.push(format!(
                    "line {line} (in the code without comments): `{SPAWN}` is outside any fn body (a macro or an item): the scan cannot see where it runs"
                ));
                continue;
            };
            let tail = &code[end..body_end];
            let run_at = [".output(", ".spawn(", ".status("]
                .iter()
                .filter_map(|m| tail.find(m))
                .min()
                .unwrap_or(tail.len());
            let before_run = squeezed(&tail[..run_at]);
            match before_run.rfind(GUARD) {
                None => problems.push(format!(
                    "line {line} (in the code without comments): `{SPAWN}` is run without .env(\"BB_REFUSE_NON_LOOPBACK_API\", \"1\")"
                )),
                Some(guard_at) => {
                    for ungard in UNGUARDS {
                        if before_run[guard_at + GUARD.len()..].contains(ungard) {
                            problems.push(format!(
                                "line {line} (in the code without comments): `{ungard}` after the guard takes it away again before the run"
                            ));
                        }
                    }
                }
            }
        }
    }
    // any other mention of the token is a path the scan cannot follow
    let mentions = code.matches(TOKEN).count();
    if mentions != spawns {
        problems.push(format!(
            "{mentions} uses of {TOKEN} but {spawns} plain spawns: an alias or a split spelling hides where bb is run"
        ));
    }
    problems
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// The two files the scan does not read, by PATH relative to `tests/` (a file of the same name elsewhere is read): this
/// file's own helper sets the variable conditionally and its fixtures spell the spawns out; the real-server test is
/// #[ignore]d and takes its server from the environment on purpose.
fn exempt(relative_path: &str) -> bool {
    ["api_guard.rs", "direct_real_server.rs"].contains(&relative_path)
}

#[test]
fn the_exemptions_are_two_paths_not_two_names() {
    assert!(exempt("api_guard.rs") && exempt("direct_real_server.rs"));
    assert!(!exempt("common/api_guard.rs") && !exempt("sub/direct_real_server.rs") && !exempt("zero_byte_files.rs"));
}

#[test]
fn every_test_that_runs_the_binary_sets_the_guard() {
    // This file's own helper sets the variable conditionally (and its fixtures spell the spawns out); the real-server test
    // is #[ignore]d and takes its server from the environment on purpose.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    rust_files(&dir, &mut files);
    let mut offenders = Vec::new();
    let (mut scanned, mut spawns) = (0, 0);
    for path in files {
        let relative = path.strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
        if exempt(&relative) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        if !text.contains("CARGO_BIN_EXE_") {
            continue;
        }
        scanned += 1;
        spawns += without_comments(&text).matches("CARGO_BIN_EXE_").count();
        for problem in unguarded_spawns(&text) {
            offenders.push(format!("{}: {problem}", path.strip_prefix(&dir).unwrap().display()));
        }
    }
    assert!(scanned >= 20, "the scan found only {scanned} files that run the binary");
    assert!(spawns >= 26, "the scan found only {spawns} spawns");
    assert!(
        offenders.is_empty(),
        "set BB_REFUSE_NON_LOOPBACK_API=1 on every run of the binary: {offenders:#?}"
    );
}

// ── the scan can go red: each of these, written by a future test, must be refused ──

const GUARDED: &str = r#"
fn run(home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bb"))
        .args(["ls"])
        .env("HOME", home)
        .env("BB_REFUSE_NON_LOOPBACK_API", "1")
        .output()
        .unwrap()
}
"#;

#[test]
fn the_scan_accepts_a_guarded_spawn_in_every_shape_the_tests_use() {
    assert!(unguarded_spawns(GUARDED).is_empty());
    // built in one statement, run in another
    let split_statements = r#"
fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bb"));
    cmd.env("HOME", home);
    cmd.env("BB_REFUSE_NON_LOOPBACK_API", "1");
    cmd
}
fn run() { let out = command(&h).output().unwrap(); }
"#;
    assert!(unguarded_spawns(split_statements).is_empty());
    // the call itself split across lines, guard in a later statement before the run
    let split_call = "fn f() { let mut c = Command\n  ::new(\n env!(\"CARGO_BIN_EXE_bb\") );\n c.env(\"BB_REFUSE_NON_LOOPBACK_API\", \"1\"); c.output().unwrap(); }";
    assert!(
        unguarded_spawns(split_call).is_empty(),
        "{:?}",
        unguarded_spawns(split_call)
    );
    // a `//` inside a string does not hide the rest of the line
    let url = r#"fn f() { Command::new(env!("CARGO_BIN_EXE_bb")).env("X", "http://x").env("BB_REFUSE_NON_LOOPBACK_API", "1").output(); }"#;
    assert!(unguarded_spawns(url).is_empty());
}

#[test]
fn the_scan_refuses_what_a_careless_test_could_write() {
    let unguarded = r#"fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).args(["ls"]).output().unwrap(); }"#;
    assert_eq!(unguarded_spawns(unguarded).len(), 1, "no guard at all");
    // an alias of the binary path: the scan cannot see where it is run
    let aliased = r#"fn run() { let bin = env!("CARGO_BIN_EXE_bb"); Command::new(bin).output().unwrap(); }"#;
    assert!(!unguarded_spawns(aliased).is_empty(), "an alias");
    // the call split across lines, no guard
    let split = "fn run() {\n Command::new(\n  env!(\"CARGO_BIN_EXE_bb\")\n ).output().unwrap(); }";
    assert_eq!(unguarded_spawns(split).len(), 1, "a split call is still a spawn");
    // the variable named in a comment of any kind does not count as the guard
    let line_comment = "fn run() {\n // .env(\"BB_REFUSE_NON_LOOPBACK_API\", \"1\")\n Command::new(env!(\"CARGO_BIN_EXE_bb\")).output().unwrap(); }";
    assert_eq!(unguarded_spawns(line_comment).len(), 1, "a line comment");
    let doc_comment =
        "/// sets BB_REFUSE_NON_LOOPBACK_API\nfn run() { Command::new(env!(\"CARGO_BIN_EXE_bb\")).output().unwrap(); }";
    assert_eq!(unguarded_spawns(doc_comment).len(), 1, "a doc comment");
    let block_comment = "fn run() { /* .env(\"BB_REFUSE_NON_LOOPBACK_API\", \"1\") */ Command::new(env!(\"CARGO_BIN_EXE_bb\")).output().unwrap(); }";
    assert_eq!(unguarded_spawns(block_comment).len(), 1, "a block comment");
    // the guard set in ANOTHER function, or set after the command ran, or only removed
    let elsewhere = r#"fn guard(c: &mut Command) { c.env("BB_REFUSE_NON_LOOPBACK_API", "1"); }
fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).output().unwrap(); }"#;
    assert_eq!(unguarded_spawns(elsewhere).len(), 1, "another function");
    let after = r#"fn run() { let mut c = Command::new(env!("CARGO_BIN_EXE_bb")); c.output().unwrap(); c.env("BB_REFUSE_NON_LOOPBACK_API", "1"); }"#;
    assert_eq!(unguarded_spawns(after).len(), 1, "after the run");
    let removed = r#"fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).env_remove("BB_REFUSE_NON_LOOPBACK_API").output().unwrap(); }"#;
    assert_eq!(unguarded_spawns(removed).len(), 1, "env_remove is not the guard");
    // a command that is only built here, with the guard set in a LATER function: not the same command's guard
    let built_only = r#"fn build() -> Command { Command::new(env!("CARGO_BIN_EXE_bb")) }
fn later(c: &mut Command) { c.env("BB_REFUSE_NON_LOOPBACK_API", "1"); }"#;
    assert_eq!(unguarded_spawns(built_only).len(), 1, "a guard in a later function");
    // the guard set, then taken away again before the run (env_clear wipes it; env_remove / envs can too)
    let cleared = r#"fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).env("BB_REFUSE_NON_LOOPBACK_API", "1").env_clear().output().unwrap(); }"#;
    assert_eq!(unguarded_spawns(cleared).len(), 1, "env_clear after the guard");
    let removed_after = r#"fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).env("BB_REFUSE_NON_LOOPBACK_API", "1").env_remove("BB_REFUSE_NON_LOOPBACK_API").output().unwrap(); }"#;
    assert_eq!(
        unguarded_spawns(removed_after).len(),
        1,
        "env_remove of the guard after it"
    );
    let envs_after = r#"fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).env("BB_REFUSE_NON_LOOPBACK_API", "1").envs(std::env::vars()).output().unwrap(); }"#;
    assert_eq!(unguarded_spawns(envs_after).len(), 1, "envs after the guard");
    // a guard with a value the binary reads as off
    for off in ["0", "", "no"] {
        let src = format!(
            r#"fn run() {{ Command::new(env!("CARGO_BIN_EXE_bb")).env("BB_REFUSE_NON_LOOPBACK_API", "{off}").output().unwrap(); }}"#
        );
        assert_eq!(unguarded_spawns(&src).len(), 1, "the value {off:?} is not the guard");
    }
    // a spawn the scan cannot follow: inside a macro, or with a name built at compile time
    let in_macro = r#"macro_rules! run_bb { () => { Command::new(env!("CARGO_BIN_EXE_bb")).output().unwrap() }; }"#;
    assert!(!unguarded_spawns(in_macro).is_empty(), "a macro body");
    let built_name = r#"fn run() { Command::new(env!(concat!("CARGO_BIN_EXE_", "bb"))).env("BB_REFUSE_NON_LOOPBACK_API", "1").output().unwrap(); }"#;
    assert!(!unguarded_spawns(built_name).is_empty(), "a name built with concat!");
    // clearing BEFORE the guard is how the two sites that were wrong are written now
    let clear_then_guard = r#"fn run() { Command::new(env!("CARGO_BIN_EXE_bb")).env_clear().env("HOME", h).env("BB_REFUSE_NON_LOOPBACK_API", "1").output().unwrap(); }"#;
    assert!(
        unguarded_spawns(clear_then_guard).is_empty(),
        "env_clear first, then the guard"
    );
    // one guarded and one unguarded spawn in one file: the second is found
    let two = format!("{GUARDED}\nfn other() {{ Command::new(env!(\"CARGO_BIN_EXE_bb\")).output().unwrap(); }}");
    assert_eq!(unguarded_spawns(&two).len(), 1, "one of two");
}

#[test]
fn the_scan_reads_nested_directories() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("api-guard-nested-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("common/deeper")).unwrap();
    std::fs::write(dir.join("top.rs"), "// a").unwrap();
    std::fs::write(dir.join("common/helper.rs"), "// b").unwrap();
    std::fs::write(dir.join("common/deeper/more.rs"), "// c").unwrap();
    std::fs::write(dir.join("common/notes.txt"), "not rust").unwrap();
    let mut found = Vec::new();
    rust_files(&dir, &mut found);
    let mut names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["helper.rs", "more.rs", "top.rs"]);
    let _ = std::fs::remove_dir_all(&dir);
}
