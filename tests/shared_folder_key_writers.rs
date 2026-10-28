//! Contract 2237 "format l7" (lead, 2026-10-28), sections 1 and 2, as they apply to the CLI.
//!
//! The contract binds every client request that inserts or replaces a `shared_folder_keys` row: for a custody
//! folder it must carry `format: "l7"` (section 1), and a 409 `upgrade_required` answer means "the owner has to
//! upgrade this folder", never a retry in the legacy format (section 2).
//!
//! The CLI has no such request. It READS the folder key inventory (`GET /shared-folders/{id}/keys`, and the
//! legacy invite's `GET /shares/invites/{id}/folder-keys` for a folder that never had custody) and adopts an
//! owner wrap on `files.owner_wrapped_file_key` (`POST /files/{id}/owner-key`, a column of `files`, not a key
//! row). Nothing in `src/` publishes, upgrades, re-wraps or links folder keys.
//!
//! That is a claim about the whole source tree, so it is pinned here: every line of `src/` that names a route
//! family which can write key rows must be one of the reads below. A future CLI writer fails this test on
//! purpose; its author then owes the contract (send `format: "l7"` for custody folders, map 409
//! `upgrade_required` to an honest error, add a test for both) before adding the line to the list.

use std::path::{Path, PathBuf};

/// Route families whose server handlers insert or replace `shared_folder_keys` rows: invite create and
/// `.../folder-keys` publish, `PUT /shared-folders/...` (custody register, rewrap, key publish), folder links,
/// and the public folder (guest) routes.
const WRITER_ROUTE_FAMILIES: &[&str] = &[
    "shares/invites",
    "shared-folders",
    "folder-links",
    "folder_links",
    "api/v1/f/",
    // Wire field names of the write bodies (quoted, so reader method names such as `get_shared_folder_keys`
    // do not match).
    "\"encrypted_folder_key\"",
    "\"folder_keys\"",
];

/// The only lines allowed to name one of those families. Each carries its HTTP method, so turning a read into
/// a write changes the line and fails the test.
const SANCTIONED: &[(&str, &str)] = &[
    ("owned_keys.rs", r#"api.get_json("/api/v1/shares/invites/sent")"#),
    (
        "owned_keys.rs",
        r#".get_json(&format!("/api/v1/shares/invites/{invite_id}/folder-keys"))"#,
    ),
    ("api.rs", r#".get(self.url(&format!("/api/v1/shared-folders/{id}")))"#),
    (
        "api.rs",
        r#".get(self.url(&format!("/api/v1/shared-folders/{id}/keys")))"#,
    ),
    // Encrypted search shards for a shared folder: not a key row.
    (
        "shared_folder_index.rs",
        r#"&format!("/api/v1/shared-folders/{folder_id}/search-index/shards")"#,
    ),
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn mentions_family(code: &str) -> Option<&'static str> {
    WRITER_ROUTE_FAMILIES
        .iter()
        .copied()
        .find(|family| code.contains(family))
}

/// Code lines of `src/` naming a writer route family, as `(file name, line number, trimmed line)`.
fn mentions(root: &Path) -> Vec<(String, usize, String)> {
    let mut files = Vec::new();
    sources(root, &mut files);
    assert!(
        files.len() > 20,
        "scanned {} files; the walk found no source tree",
        files.len()
    );
    let mut found = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        for (n, line) in std::fs::read_to_string(&file).unwrap().lines().enumerate() {
            let code = line.trim();
            if code.starts_with("//") {
                continue;
            }
            if mentions_family(code).is_some() {
                found.push((name.clone(), n + 1, code.to_string()));
            }
        }
    }
    found
}

fn unsanctioned(found: &[(String, usize, String)]) -> Vec<String> {
    found
        .iter()
        .filter(|(file, _, code)| {
            !SANCTIONED
                .iter()
                .any(|(f, fragment)| f == file && code.contains(fragment))
        })
        .map(|(file, n, code)| format!("{file}:{n}: {code}"))
        .collect()
}

#[test]
fn the_cli_publishes_no_shared_folder_keys() {
    let found = mentions(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    // The instrument must see the reads it sanctions, or a silent scan would pass for the wrong reason.
    for (file, fragment) in SANCTIONED {
        assert!(
            found.iter().any(|(f, _, code)| f == file && code.contains(fragment)),
            "sanctioned line not found in src/{file}: {fragment}"
        );
    }
    let strangers = unsanctioned(&found);
    assert!(
        strangers.is_empty(),
        "new code names a route family that can write shared_folder_keys rows. Contract 2237 format-l7: send \
         `format: \"l7\"` for custody folders, treat 409 `upgrade_required` as \"the owner must upgrade this \
         folder\" (never a legacy retry), test both, then list the line in SANCTIONED:\n{}",
        strangers.join("\n")
    );
    eprintln!("scanned writer-route mentions: {} (all sanctioned reads)", found.len());
}

#[test]
fn a_write_to_a_key_route_is_not_sanctioned() {
    // The check itself must be able to go red: a PUT to the inventory route and a new publish route.
    let found = vec![
        (
            "api.rs".to_string(),
            1,
            r#".put(self.url(&format!("/api/v1/shared-folders/{id}/keys")))"#.to_string(),
        ),
        (
            "api.rs".to_string(),
            2,
            r#".post(self.url(&format!("/api/v1/shares/invites/{id}/folder-keys")))"#.to_string(),
        ),
        (
            "sync.rs".to_string(),
            3,
            r#"json!({"format": "l7", "folder_keys": keys})"#.to_string(),
        ),
    ];
    assert_eq!(unsanctioned(&found).len(), 3);
    // ...and the reader method names that tripped the first draft are not writer routes.
    assert!(mentions_family("api.get_shared_folder_keys(id, cursor).await?").is_none());
    assert!(mentions_family(r#"json!({"folder_keys": keys})"#).is_some());
}
