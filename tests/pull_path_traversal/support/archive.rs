//! One behaviour group of the same integration suite.

use super::*;

/// `bb pull --zip` builds archive entry paths from the same server-provided
/// names. A `../..` or absolute entry is a zip-slip for whoever extracts the
/// archive later, so unsafe names must be left out (and the run flagged).
#[test]
fn zip_pull_leaves_hostile_names_out_of_the_archive() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_plain_file(&vault, "../../zipslip-marker.txt", PAYLOAD);
    fx.add_plain_file(&vault, "/abs/zipslip-abs-marker.txt", PAYLOAD);
    fx.add_plain_file(&vault, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    let zip_bytes = std::fs::read(s.cwd.join("out.zip")).unwrap_or_default();
    let has = |needle: &str| zip_bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(
        has("vault/ok.txt"),
        "benign entry missing from the archive\n{}",
        describe(&out)
    );
    assert!(
        !has("zipslip-marker") && !has("zipslip-abs-marker"),
        "hostile entry names ended up in the archive\n{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
}

/// Task 1760: `bb pull --zip` on a folder holding a file-request upload used
/// to die with `zip failed: decryption failed` (the zip path only knew the
/// master-key derivation). The entry must carry the DECRYPTED name + content.
#[test]
fn zip_pull_decrypts_request_uploads() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file(&vault, "from-stranger.txt", b"sent through a file request\n");
    let sub = fx.add_request_folder(&vault, "inbox");
    fx.add_request_file(&sub, "nested \u{00e9}.txt", b"nested request upload\n");
    fx.add_plain_file(&vault, "mine.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip-req", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    assert!(out.status.success(), "{}", describe(&out));
    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(
        entries.get("vault/from-stranger.txt").map(Vec::as_slice),
        Some(&b"sent through a file request\n"[..]),
        "request upload missing or wrong in archive: {:?}\n{}",
        entries.keys().collect::<Vec<_>>(),
        describe(&out)
    );
    assert_eq!(
        entries.get("vault/inbox/nested \u{00e9}.txt").map(Vec::as_slice),
        Some(&b"nested request upload\n"[..])
    );
    assert_eq!(entries.get("vault/mine.txt").map(Vec::as_slice), Some(BENIGN));
    assert_eq!(
        entries.len(),
        3,
        "unexpected entries: {:?}",
        entries.keys().collect::<Vec<_>>()
    );
}

/// Task 1760 x 1733: the request-key branch must not reopen the zip-slip hole.
/// A request upload whose DECRYPTED name is `../..` is left out and flagged.
#[test]
fn zip_pull_keeps_safe_path_checks_on_request_names() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file(&vault, "../../zipslip-req-marker.txt", PAYLOAD);
    fx.add_request_file(&vault, "good.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip-req-evil", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(
        entries.get("vault/good.txt").map(Vec::as_slice),
        Some(BENIGN),
        "{}",
        describe(&out)
    );
    assert!(
        entries.keys().all(|k| !k.contains("zipslip")),
        "hostile request name reached the archive: {:?}",
        entries.keys().collect::<Vec<_>>()
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
}

// ---------------------------------------------------------------------------
// Task 1760 round 2
// ---------------------------------------------------------------------------

/// P1-b: a multi-chunk request upload whose last chunk is shorter than the rest
/// (every real upload whose size is not a chunk multiple). The chunk size is
/// 100_000: not a power of two, so blind probing would burn its whole budget
/// before reaching it. The server's `X-Chunk-Size` header is authoritative, and
/// the zip path has to use it.
#[test]
fn zip_pull_decrypts_multichunk_request_upload_using_the_chunk_size_header() {
    let body = uneven_body(350_001);
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file_chunked(&vault, "big.bin", &body, 100_000, true);
    fx.add_request_file(&vault, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip-multichunk-header", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    assert!(out.status.success(), "{}", describe(&out));
    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(
        entries.get("vault/big.bin").map(Vec::as_slice),
        Some(body.as_slice()),
        "multi-chunk upload wrong or missing: {:?}\n{}",
        entries.keys().collect::<Vec<_>>(),
        describe(&out)
    );
    assert_eq!(entries.get("vault/ok.txt").map(Vec::as_slice), Some(BENIGN));
}

/// P1-b, legacy responses: no `X-Chunk-Size`; a power-of-two chunk size is
/// recovered by AEAD-validated probing (task 1759's frame-size logic).
#[test]
fn zip_pull_decrypts_multichunk_request_upload_without_the_header() {
    let body = uneven_body(2_500);
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file_chunked(&vault, "big.bin", &body, 1024, false);
    let url = fx.serve();
    let s = Scratch::new("zip-multichunk-probe", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    assert!(out.status.success(), "{}", describe(&out));
    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(entries.get("vault/big.bin").map(Vec::as_slice), Some(body.as_slice()));
}

/// P1-c: an uploader controls the names, so two uploads called `scan.pdf` (or
/// `A.txt` / `a.txt`, which collide on macOS and Windows) must not abort the
/// archive or overwrite each other on extraction. Later ones get `name (2).ext`,
/// per directory, and no two entries may differ only by case.
#[test]
fn zip_pull_dedupes_duplicate_and_case_colliding_names() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file(&vault, "scan.pdf", b"first scan\n");
    fx.add_request_file(&vault, "scan.pdf", b"second scan\n");
    fx.add_request_file(&vault, "A.txt", b"upper\n");
    fx.add_request_file(&vault, "a.txt", b"lower\n");
    let first = fx.add_request_folder(&vault, "inbox");
    fx.add_request_file(&first, "scan.pdf", b"in inbox\n");
    let second = fx.add_request_folder(&vault, "inbox");
    fx.add_request_file(&second, "note.txt", b"in the second inbox\n");
    let url = fx.serve();
    let s = Scratch::new("zip-dupes", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    assert!(out.status.success(), "{}", describe(&out));
    let names = zip_names(&s.cwd.join("out.zip"));
    assert_eq!(
        names,
        vec![
            "vault/A.txt",
            "vault/a (2).txt",
            "vault/inbox (2)/note.txt",
            "vault/inbox/scan.pdf",
            "vault/scan (2).pdf",
            "vault/scan.pdf",
        ],
        "{}",
        describe(&out)
    );
    let lower: BTreeSet<String> = names.iter().map(|n| n.to_lowercase()).collect();
    assert_eq!(lower.len(), names.len(), "case-insensitive collision in {names:?}");
    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(entries["vault/scan.pdf"], b"first scan\n");
    assert_eq!(entries["vault/scan (2).pdf"], b"second scan\n");
    assert_eq!(entries["vault/a (2).txt"], b"lower\n");
}

/// P2-d: a file that cannot be decrypted is left out and flagged (exit 3), the
/// rest of the archive is still written.
#[test]
fn zip_pull_skips_and_flags_an_undecryptable_item() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_corrupt_file(&vault, "broken.bin");
    fx.add_request_file(&vault, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip-corrupt", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(
        entries.keys().cloned().collect::<Vec<_>>(),
        vec!["vault/ok.txt".to_string()],
        "{}",
        describe(&out)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("broken.bin"),
        "the skipped item is not named\n{}",
        describe(&out)
    );
}

/// P2-d: `--force` over a good archive must not leave a truncated zip behind
/// when the run produces nothing (here: the only item cannot be decrypted).
#[test]
fn zip_force_failure_keeps_the_existing_archive() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_corrupt_file(&vault, "broken.bin");
    let url = fx.serve();
    let s = Scratch::new("zip-force-keeps", &url);
    std::fs::write(s.cwd.join("out.zip"), b"PRECIOUS EXISTING ARCHIVE").unwrap();

    let out = s.bb(&["pull", "vault", "--zip", "--force", "-o", "out.zip"]);

    assert!(!out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(s.cwd.join("out.zip")).unwrap(),
        b"PRECIOUS EXISTING ARCHIVE",
        "the existing archive was clobbered\n{}",
        describe(&out)
    );
    let leftovers: Vec<_> = std::fs::read_dir(&s.cwd)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "out.zip")
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

/// P2-d: a download that fails after the archive is under way (404 on the
/// second file) must also leave the existing archive alone and clean up.
#[test]
fn zip_force_mid_run_download_failure_keeps_the_existing_archive() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file(&vault, "ok.txt", BENIGN);
    fx.add_plain_row(&vault, "vanished.bin");
    let url = fx.serve();
    let s = Scratch::new("zip-force-mid", &url);
    std::fs::write(s.cwd.join("out.zip"), b"PRECIOUS EXISTING ARCHIVE").unwrap();

    let out = s.bb(&["pull", "vault", "--zip", "--force", "-o", "out.zip"]);

    assert!(!out.status.success(), "{}", describe(&out));
    assert_eq!(
        std::fs::read(s.cwd.join("out.zip")).unwrap(),
        b"PRECIOUS EXISTING ARCHIVE",
        "{}",
        describe(&out)
    );
    let leftovers: Vec<_> = std::fs::read_dir(&s.cwd)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "out.zip")
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

/// P2-g: when the request keys cannot be loaded, say so ONCE (not once per
/// file, not never); the request uploads are left out and flagged.
#[test]
fn zip_pull_warns_once_when_request_keys_cannot_be_loaded() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file(&vault, "one.txt", b"1\n");
    fx.add_request_file(&vault, "two.txt", b"2\n");
    fx.add_plain_file(&vault, "mine.txt", BENIGN);
    fx.fail_requests_endpoint();
    let url = fx.serve();
    let s = Scratch::new("zip-keys-fail", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("could not load your file-request keys").count(),
        1,
        "{}",
        describe(&out)
    );
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let entries = read_zip(&s.cwd.join("out.zip"));
    assert_eq!(
        entries.keys().cloned().collect::<Vec<_>>(),
        vec!["vault/mine.txt".to_string()]
    );
}

/// P2-h: names that are fine on this host but break on Windows (device names,
/// trailing dot/space, `:` streams, reserved characters) or spoof a file type
/// (bidi override) are made safe in the archive, not passed through. Nothing is
/// lost: every file is still in the archive, just under a safe name.
#[test]
fn zip_pull_makes_windows_unsafe_names_safe() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    for (i, name) in [
        "CON.txt",
        "nul",
        "report.",
        "trail ",
        "ab:c.txt",
        "evil\u{202e}fdp.exe",
        "q?.txt",
        "ads.txt:stream",
    ]
    .iter()
    .enumerate()
    {
        fx.add_request_file(&vault, name, format!("body {i}\n").as_bytes());
    }
    fx.add_request_file(&vault, "ok.txt", BENIGN);
    let url = fx.serve();
    let s = Scratch::new("zip-winnames", &url);

    let out = s.bb(&["pull", "vault", "--zip", "-o", "out.zip"]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        zip_names(&s.cwd.join("out.zip")),
        vec![
            "vault/_CON.txt",
            "vault/_nul",
            "vault/ab_c.txt",
            "vault/ads.txt_stream",
            "vault/evilfdp.exe",
            "vault/ok.txt",
            "vault/q_.txt",
            "vault/report_",
            "vault/trail_",
        ],
        "{}",
        describe(&out)
    );
}

/// P2-i: a hostile server can list a folder inside itself. The walk must stop,
/// flag the cycle, and still produce the archive for everything else.
#[test]
fn zip_pull_survives_a_folder_cycle() {
    let mut fx = Fixture::new();
    let vault = fx.add_plain_folder("", "vault");
    fx.add_request_file(&vault, "ok.txt", BENIGN);
    let sub = fx.add_plain_folder(&vault, "sub");
    fx.add_request_file(&sub, "inner.txt", b"inner\n");
    fx.link_child(&sub, &vault);
    let url = fx.serve();
    let s = Scratch::new("zip-cycle", &url);

    let out = s
        .bb_timeout(&["pull", "vault", "--zip", "-o", "out.zip"], 20)
        .expect("bb pull --zip did not terminate on a folder cycle");

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert_eq!(
        zip_names(&s.cwd.join("out.zip")),
        vec!["vault/ok.txt", "vault/sub/inner.txt"],
        "{}",
        describe(&out)
    );
}
