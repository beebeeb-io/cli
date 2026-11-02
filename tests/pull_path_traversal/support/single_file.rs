//! One behaviour group of the same integration suite.

use super::*;

/// Single-file `bb pull <id>` with no `-o`: the default output path is built
/// from the server-provided name and must be refused when unsafe.
#[test]
fn single_file_default_output_rejects_hostile_names() {
    let mut failures: Vec<String> = Vec::new();
    for (kind, source) in [
        ("../escape.txt", "request"),
        ("../escape.txt", "plain"),
        ("a/../../x", "request"),
        ("abs", "request"),
        ("abs", "plain"),
    ] {
        let placeholder = Scratch::new("single", "http://127.0.0.1:1");
        let name = if kind == "abs" {
            placeholder.root.join("abs-single.txt").display().to_string()
        } else {
            kind.to_string()
        };
        let mut fx = Fixture::new();
        let id = if source == "request" {
            fx.add_request_file(ROOT_ID, &name, PAYLOAD)
        } else {
            fx.add_plain_file(ROOT_ID, &name, PAYLOAD)
        };
        placeholder.point_at(&fx.serve());

        let before = placeholder.snapshot_outside_out();
        let out = placeholder.bb(&["pull", &id]);
        let after = placeholder.snapshot_outside_out();

        let label = format!("{kind}/{source}");
        let escaped: Vec<_> = after.difference(&before).cloned().collect();
        if !escaped.is_empty() {
            failures.push(format!(
                "{label}: wrote outside cwd or created a file: {escaped:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        if out.status.success() {
            failures.push(format!("{label}: expected a refusal, got success\n{}", describe(&out)));
            continue;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !(stderr.contains("unsafe") && stderr.contains("-o")) {
            failures.push(format!(
                "{label}: refusal must say 'unsafe' and point at -o\n{}",
                describe(&out)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// An explicit `-o <path>` is the user's own choice and is never second-guessed
/// (it may legitimately be absolute or contain `..`).
#[test]
fn explicit_output_path_is_honoured() {
    let mut fx = Fixture::new();
    let id = fx.add_request_file(ROOT_ID, "../would-be-unsafe.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("explicit-o", &url);
    let target = s.root.join("chosen.txt");

    let out = s.bb(&["pull", &id, "-o", target.to_str().unwrap()]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(std::fs::read(&target).unwrap(), BENIGN);
}

/// `bb sync` mirrors a remote folder into a local directory. A hostile remote
/// name (`../../escape.txt`, absolute, separators) must be skipped, never
/// joined onto the sync root; legitimate files in the same folder still sync.
/// (The sync variant is weaker than pull: names only reach it from a hostile
/// server, since sync decrypts with the master key alone.)
#[test]
fn sync_download_skips_hostile_remote_names() {
    let mut failures: Vec<String> = Vec::new();
    for kind in ["dotdot", "absolute", "separator"] {
        let placeholder = Scratch::new(&format!("sync-{kind}"), "http://127.0.0.1:1");
        // Sync root is <root>/cwd/local, so `../../escape.txt` lands in <root>.
        let local = placeholder.cwd.join("local");
        std::fs::create_dir_all(&local).unwrap();
        let name = match kind {
            "dotdot" => "../../escape.txt".to_string(),
            "absolute" => placeholder.root.join("abs-sync.txt").display().to_string(),
            _ => "dir/inner.txt".to_string(),
        };

        let mut fx = Fixture::new();
        let vault = fx.add_plain_folder("", "vault");
        fx.add_plain_file(&vault, &name, PAYLOAD);
        fx.add_plain_file(&vault, "ok.txt", BENIGN);
        placeholder.point_at(&fx.serve());

        let before = placeholder.snapshot_outside_out();
        let out = placeholder.bb(&["sync", local.to_str().unwrap(), "/vault", "--once"]);
        let after = placeholder.snapshot_outside_out();

        let new: Vec<_> = after
            .difference(&before)
            // `home/` is bb's own config dir (device.json); `cwd/local/` is the sync root.
            .filter(|p| !p.starts_with("cwd/local/") && !p.starts_with("home/"))
            .cloned()
            .collect();
        if !new.is_empty() {
            failures.push(format!(
                "{kind}: wrote outside the sync root: {new:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        let stray: Vec<String> = std::fs::read_dir(&local)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "ok.txt" && !n.starts_with(".bb"))
            .collect();
        if !stray.is_empty() {
            failures.push(format!(
                "{kind}: hostile entry inside the sync root: {stray:?}\n{}",
                describe(&out)
            ));
            continue;
        }
        match std::fs::read(local.join("ok.txt")) {
            Ok(b) if b == BENIGN => {}
            other => failures.push(format!("{kind}: benign file not synced: {other:?}\n{}", describe(&out))),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
