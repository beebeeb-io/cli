//! Guard (task 2050, Guus 2026-10-19: "Describe data locations by country, not
//! city"): no city name may appear in what `bb` shows a user.
//!
//! The production source (every `src/**/*.rs` without its `#[cfg(test)]` items,
//! with comment lines removed) and the README are scanned for Falkenstein,
//! Helsinki, Nuremberg, Nürnberg and Ede, matched as whole words,
//! case-insensitively (`ede` the place, not "needed"). Comments and the inline
//! test modules are excluded on purpose: they hold fixtures that prove the
//! absence of a city in the output.
//!
//! The behavioural half (the real binary against a server full of cities
//! prints none) lives in `tests/region_choice.rs`. This half catches a literal
//! that no test happens to execute.
//!
//! The scanner proves it can go red: `scanner_finds_a_city_in_user_facing_text`
//! feeds it a string that must be flagged, and ignores one that must not.

use std::fs;
use std::path::{Path, PathBuf};

const BANNED: [&str; 5] = ["falkenstein", "helsinki", "nuremberg", "n\u{fc}rnberg", "ede"];

/// Whole-word city matches in `text`: `(line number, word)`.
fn cities_in(text: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        for word in lower.split(|c: char| !c.is_alphanumeric()) {
            if BANNED.contains(&word) {
                hits.push((i + 1, word.to_string()));
            }
        }
    }
    hits
}

/// The user-facing part of a Rust source file: whole-line comments and every
/// `#[cfg(test)]` item removed. An inline `#[cfg(test)] mod x { ... }` runs to
/// the first `}` at the start of a line (rustfmt's closing brace).
fn production_text(source: &str) -> String {
    let mut out = String::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("#[cfg(test)]") {
            out.push('\n');
            // Further attributes, then the item itself.
            let mut item = lines.next().unwrap_or("");
            while item.trim_start().starts_with("#[") {
                out.push('\n');
                item = lines.next().unwrap_or("");
            }
            out.push('\n');
            if item.trim_start().starts_with("mod ") || item.trim_start().starts_with("pub(crate) mod ") {
                if item.trim_end().ends_with('{') {
                    for inner in lines.by_ref() {
                        out.push('\n');
                        if inner == "}" {
                            break;
                        }
                    }
                }
            }
            continue;
        }
        if line.trim_start().starts_with("//") {
            // Keep the line count: a blank line stands in for the comment.
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn scanner_finds_a_city_in_user_facing_text() {
    let src = "fn f() {\n    // Falkenstein in a comment is fine\n    println!(\"Stored in Falkenstein\");\n}\n\
               #[cfg(test)]\nmod tests {\n    const X: &str = \"Helsinki\";\n}\n\
               fn g() { let _ = \"Nuremberg\"; }\n";
    let hits = cities_in(&production_text(src));
    assert_eq!(
        hits,
        vec![(3, "falkenstein".to_string()), (9, "nuremberg".to_string())],
        "a test module is skipped, code after it is still scanned: {hits:?}"
    );
    assert!(cities_in("we needed it").is_empty(), "`ede` inside a word is not the place");
    assert_eq!(cities_in("N\u{fc}rnberg, Ede").len(), 2);
}

#[test]
fn no_city_in_the_production_source() {
    let mut files = Vec::new();
    rust_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(files.len() > 20, "found only {} source files", files.len());
    let mut found = Vec::new();
    for file in &files {
        let source = fs::read_to_string(file).unwrap();
        for (line, word) in cities_in(&production_text(&source)) {
            found.push(format!("{}:{line}: {word}", file.display()));
        }
    }
    assert!(
        found.is_empty(),
        "city names in user-facing source (storage is described by country):\n{}",
        found.join("\n")
    );
}

#[test]
fn no_city_in_the_readme() {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).unwrap();
    assert!(readme.contains("bb region"), "README no longer mentions bb region");
    let hits = cities_in(&readme);
    assert!(hits.is_empty(), "city names in README.md: {hits:?}");
}
