//! Task 2333 (spec §6, §9.4 N2a, §9.5): what `bb push` prints about its transfer, through the real binary against
//! the in-process mock of `support/upload_mock.rs`: the end lines on a pipe, `--verbose`, `--quiet`, the usage
//! error of both together, a failed or grown file in a folder, and what must never be shown.

#[path = "support/upload_mock.rs"]
pub mod upload_mock;

use std::sync::atomic::Ordering;
use std::time::Duration;

use upload_mock::*;

/// The claims guard's provider names (`scripts/claims-guard/policy.conf`, `PROVIDER_NAMES`): a copy of
/// `src/transfer_stats/pool.rs`'s, because `tests/` cannot reach a `cfg(test)` item of the binary.
const PROVIDER_NAMES: [&str; 8] = [
    "hetzner",
    "leaseweb",
    "ovhcloud",
    "scaleway",
    "contabo",
    "vultr",
    "digitalocean",
    "your-objectstorage.com",
];

/// Panics naming the first needle found in `haystack` (compared lowercase).
fn assert_absent(haystack: &str, needles: &[&str]) {
    let hay = haystack.to_lowercase();
    for needle in needles {
        assert!(
            !hay.contains(&needle.to_lowercase()),
            "must never be shown: {needle:?} found in {haystack:?}"
        );
    }
}

#[test]
fn absence_check_can_fail() {
    let planted = "pool hetzner · http://objects.storage.test/bucket-test/x?X-Amz-Signature=abc";
    let caught = std::panic::catch_unwind(|| assert_absent(planted, &["X-Amz-Signature"]));
    assert!(caught.is_err(), "the check flags the signature");
    let caught = std::panic::catch_unwind(|| assert_absent(planted, &PROVIDER_NAMES));
    assert!(caught.is_err(), "the check flags a provider name");
}

/// Spec §9.5: on a pipe the end lines print (the only performance record a log gets), with the fixed units, and
/// no live line is drawn.
#[tokio::test(flavor = "multi_thread")]
async fn push_summary_on_a_pipe() {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "push", "folder2/a.txt"]).await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let summary = regex::Regex::new(r"^  ✓ 1 file uploaded · .+ · avg .+ Mbps · .+ total$").unwrap();
    let lines: Vec<&str> = stdout.lines().filter(|l| summary.is_match(l)).collect();
    assert_eq!(lines.len(), 1, "one summary line: {stdout}");
    assert!(
        stdout.contains("  1 file · 5 B · encrypting with AES-256-GCM"),
        "{stdout}"
    );
    let end_line = regex::Regex::new(r"(?m)^  ✓ a\.txt {2,}33 B {2,}\S+ Mbps {2,}\S+s$").unwrap();
    assert!(
        end_line.is_match(&stdout),
        "the file's end line, its name apart from its size: {stdout}"
    );
    let ms = regex::Regex::new(r"\d+ms\b").unwrap();
    assert!(!ms.is_match(&stdout), "no raw milliseconds: {stdout}");
    assert!(!stdout.contains("MB/s") && !stdout.contains("1 files"), "{stdout}");
    assert!(
        !stderr.contains('━') && !stderr.contains('%'),
        "no live line off a terminal: {stderr}"
    );
}

/// Spec §9.5: `-v` on a pipe prints each chunk's line on stderr, and the detail line under the summary.
#[tokio::test(flavor = "multi_thread")]
async fn push_verbose_on_a_pipe() {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "push", "-v", "folder2/a.txt"]).await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let chunk = regex::Regex::new(r"(?m)^    chunk 1/1 · 33 B · api · .+ · 0 retries$").unwrap();
    assert!(chunk.is_match(&stderr), "the chunk line on stderr: {stderr}");
    assert!(
        stdout.contains("\n    1 chunk · 1 at a time · direct 0 · api 1 · 0 retries"),
        "the detail line under the summary: {stdout}"
    );
}

/// Spec §6.1: `--quiet` keeps what it prints today: the id, alone.
#[tokio::test(flavor = "multi_thread")]
async fn push_quiet_prints_only_the_id() {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--quiet", "push", "folder2/a.txt"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(out.status.success(), "{stdout}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    assert!(uuid::Uuid::parse_str(lines[0].trim()).is_ok(), "{stdout}");
}

/// Spec §6.2: `--verbose` with `--quiet` is a usage error in either order, before anything is sent: on `bb push`,
/// `bb pull` and `bb sync` (task 2333 PR 2).
#[tokio::test(flavor = "multi_thread")]
async fn quiet_and_verbose_is_a_usage_error_in_both_orders() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let cases: [&[&str]; 6] = [
        &["--api", api.as_str(), "--quiet", "push", "-v", "folder2/a.txt"],
        &["--api", api.as_str(), "push", "-v", "--quiet", "folder2/a.txt"],
        &["--api", api.as_str(), "--quiet", "pull", "-v", "3e15382b"],
        &["--api", api.as_str(), "pull", "-v", "--quiet", "3e15382b"],
        &["--api", api.as_str(), "--quiet", "sync", "-v", "--once", "folder2"],
        &["--api", api.as_str(), "sync", "-v", "--quiet", "--once", "folder2"],
    ];
    for args in cases {
        let out = bb(&home, args).await;
        let (_stdout, stderr) = text(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("--verbose cannot be combined with --quiet"), "{stderr}");
    }
    assert!(mock.inits.lock().unwrap().is_empty(), "nothing was uploaded");

    // `--json` wins over `--quiet` (main's output mode), and `-v` has no effect with `--json`: no conflict.
    let out = bb(
        &home,
        &["--api", &api, "--json", "--quiet", "push", "-v", "folder2/a.txt"],
    )
    .await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(serde_json::from_str::<Value>(&stdout).is_ok(), "{stdout}");
}

/// N2a (spec §9.4; Guus and the lead, 2026-11-06): `bb push -v` and `bb --json push` name the pool by its country
/// only, and never show the pool id, the datacenter code, the city, the session token, the master key or a provider
/// name.
#[tokio::test(flavor = "multi_thread")]
async fn verbose_push_prints_no_secret_or_provider() {
    let mock = Mock {
        init_extra: Some(json!({
            "storage_pool_id": "3e15382b-0000-4000-8000-000000000001",
            "region": "zz9",
            "storage_location": {"city": "Testville", "country": "Germany", "country_code": "DE"},
        })),
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    const TOKEN: &str = "tok-2333-session-secret";
    // Base64 of the bytes 1..=32: a throwaway key the mock never checks.
    const KEY: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";
    let home = scratch_home_with(&api, TOKEN, KEY);
    make_folder2(&home);

    for args in [
        vec!["--api", api.as_str(), "push", "-v", "folder2/a.txt"],
        vec!["--api", api.as_str(), "--json", "push", "folder2/z.txt"],
    ] {
        let out = bb(&home, &args).await;
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "{args:?}: {stdout}{stderr}");
        let shown = format!("{stdout}\n{stderr}");
        if args.contains(&"--json") {
            let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
            assert_eq!(
                v["transfer"]["pools"],
                json!([{"country": "Germany", "country_code": "DE", "chunks": 1}]),
                "{v}"
            );
        } else {
            assert!(stdout.contains("· pool Germany"), "the country is shown: {shown}");
        }
        // The pool id has no meaning to a user and is never shown, nor put in --json (lead, 2026-11-06).
        assert_absent(&shown, &["3e15382b", "zz9", "Testville", TOKEN, KEY]);
        assert_absent(&shown, &PROVIDER_NAMES);
    }
}

/// Review Focus 1: a folder whose file fails ends short of its plan and says so: done < total, `1 failed`, a
/// non-zero exit, and the failure mark.
#[tokio::test(flavor = "multi_thread")]
async fn push_folder_with_a_failed_file_ends_short_and_says_failed() {
    let mock = Mock {
        reject_empty: true,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb(&home, &["--api", &api, "--json", "push", "folder2"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(!out.status.success(), "{stdout}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let t = &v["transfer"];
    assert_eq!(v["total_failed"], 1, "{v}");
    assert_eq!(
        (t["files_done"].clone(), t["files_total"].clone()),
        (json!(2), json!(3)),
        "{t}"
    );
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(2), json!(3)),
        "{t}"
    );
    assert!(
        t["bytes_done"].as_u64().unwrap() < t["bytes_total"].as_u64().unwrap(),
        "{t}"
    );

    let out = bb(&home, &["--api", &api, "push", "folder2"]).await;
    let (stdout, _stderr) = text(&out);
    assert!(!out.status.success());
    assert!(stdout.contains("  ! 2 files uploaded · 1 failed · "), "{stdout}");
}

/// Review Focus 1, the other way: a file that grows after the push planned it (before its own upload starts) is
/// uploaded at its new size, and the totals follow it: done never passes total, nothing panics.
#[tokio::test(flavor = "multi_thread")]
async fn push_folder_with_a_file_that_grew_counts_its_new_size() {
    let mock = Mock::default();
    mock.hold_first_init.store(true, Ordering::SeqCst);
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let folder = make_folder2(&home);

    let run = {
        let (home, api) = (home.clone(), api.clone());
        // One file at a time (`--parallel 1`): the second file's upload starts only after the held first init.
        tokio::spawn(async move { bb(&home, &["--api", &api, "--json", "push", "--parallel", "1", "folder2"]).await })
    };
    // The first file's init is held: the plan is made. Grow the second file past one chunk, then let it go.
    let waited = std::time::Instant::now();
    while mock.inits.lock().unwrap().is_empty() && waited.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let held = !mock.inits.lock().unwrap().is_empty();
    std::fs::write(folder.join("a.txt"), vec![b'a'; 5 * 1024 * 1024]).unwrap();
    mock.hold_first_init.store(false, Ordering::SeqCst);
    let out = run.await.unwrap();
    assert!(held, "premise: the first init arrived and was held");

    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let t = &v["transfer"];
    assert_eq!(t["bytes_done"], t["bytes_total"], "{t}");
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(4), json!(4)),
        "{t}"
    );
    assert_eq!(t["bytes_done"], v["total_bytes"], "{v}");
}

/// Review P2: a file that changes after the push planned it, during its own conflict lookup (where a prompt can wait
/// for minutes), is uploaded at its new size, and the totals follow it: done never passes total, nothing panics.
#[tokio::test(flavor = "multi_thread")]
async fn push_file_that_changes_during_its_conflict_lookup_counts_its_new_size() {
    let mock = Mock::default();
    mock.hold_conflict_checks.store(true, Ordering::SeqCst);
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let folder = make_folder2(&home);

    let run = {
        let (home, api) = (home.clone(), api.clone());
        tokio::spawn(async move { bb(&home, &["--api", &api, "--json", "push", "folder2/a.txt"]).await })
    };
    let waited = std::time::Instant::now();
    while *mock.conflict_checks.lock().unwrap() == 0 && waited.elapsed() < Duration::from_secs(20) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let held = *mock.conflict_checks.lock().unwrap() > 0;
    std::fs::write(folder.join("a.txt"), vec![b'a'; 5 * 1024 * 1024]).unwrap();
    mock.hold_conflict_checks.store(false, Ordering::SeqCst);
    let out = run.await.unwrap();
    assert!(held, "premise: the conflict lookup arrived and was held");

    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let v: Value = serde_json::from_str(&stdout).expect("stdout is one JSON document");
    let t = &v["transfer"];
    assert_eq!(t["bytes_done"], t["bytes_total"], "{t}");
    assert_eq!(
        (t["chunks_done"].clone(), t["chunks_total"].clone()),
        (json!(2), json!(2)),
        "{t}"
    );
    assert_eq!(t["bytes_done"], v["total_bytes"], "{v}");
}

/// Review P3: `bb push -v 2>log` with stdout coloured (a terminal; here forced): the per-chunk lines in the log carry
/// no colour codes.
#[tokio::test(flavor = "multi_thread")]
async fn verbose_chunk_lines_on_a_redirected_stderr_carry_no_colour() {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    let out = bb_coloured(&home, &["--api", &api, "push", "-v", "folder2/a.txt"]).await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains("\u{1b}["), "premise: stdout is coloured: {stdout:?}");
    let chunk_lines: Vec<&str> = stderr.lines().filter(|l| l.contains("chunk 1/1")).collect();
    assert_eq!(chunk_lines.len(), 1, "{stderr:?}");
    assert!(
        !chunk_lines[0].contains('\u{1b}'),
        "no colour codes in the log: {:?}",
        chunk_lines[0]
    );
}

/// P7 (spec §9.3) and §9.5 `sync_json_has_both_directions`: `bb --json sync --once --concurrency 2` uploads six
/// files two at a time, one chunk each: the most chunks in flight is 2, the limit 2, and both directions'
/// `transfer` objects are there (the download one with nothing moved).
#[tokio::test(flavor = "multi_thread")]
async fn sync_peak_is_at_most_the_concurrency() {
    let mock = Mock {
        chunk_delay_ms: 50,
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    for i in 0..6 {
        std::fs::write(local.join(format!("f{i}.txt")), format!("file {i}")).unwrap();
    }

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "--json",
            "sync",
            "--once",
            "--concurrency",
            "2",
            local.to_str().unwrap(),
            "/SyncUp",
        ],
    )
    .await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let up = &json["transfer_upload"];
    assert_eq!(up["direction"], "upload", "{json}");
    assert_eq!(up["files_done"], 6, "{up}");
    assert_eq!(up["chunks_done"], 6, "{up}");
    assert_eq!(up["chunks_total"], 6, "{up}");
    assert_eq!(up["bytes_done"], up["bytes_total"], "{up}");
    assert_eq!(up["max_chunks_in_flight"], 2, "{up}");
    assert_eq!(
        up["peak_chunks_in_flight"], 2,
        "two files' chunks overlapped, never more: {up}"
    );
    let down = &json["transfer_download"];
    assert_eq!(down["direction"], "download", "{json}");
    assert_eq!(down["chunks_done"], 0, "{down}");
    assert_eq!(down["avg_mbps"], Value::Null, "{down}");
}

/// Review P3 (#142): sync's limit is at most the files pending. One file at the default `--concurrency 4` can have
/// one chunk in flight, and `max_chunks_in_flight` says 1, not 4.
#[tokio::test(flavor = "multi_thread")]
async fn sync_limit_is_at_most_the_files_pending() {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("only.txt"), b"one file").unwrap();

    let args = [
        "--api",
        &api,
        "--json",
        "sync",
        "--once",
        local.to_str().unwrap(),
        "/SyncUp",
    ];
    let out = bb(&home, &args).await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let json: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    let up = &json["transfer_upload"];
    assert_eq!(up["files_done"], 1, "{up}");
    assert_eq!(up["max_chunks_in_flight"], 1, "{up}");
    assert_eq!(up["peak_chunks_in_flight"], 1, "{up}");
}

/// Spec §6.5: on a pipe the sync line reads Mbps and a §5 duration; `-v` adds the `↑` detail line.
#[tokio::test(flavor = "multi_thread")]
async fn sync_end_line_on_a_pipe() {
    let api = start_mock(Mock::default()).await;
    let home = scratch_home(&api);
    let local = home.join("syncdir");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("a.txt"), b"hello").unwrap();

    let out = bb(
        &home,
        &[
            "--api",
            &api,
            "sync",
            "-v",
            "--once",
            local.to_str().unwrap(),
            "/SyncUp",
        ],
    )
    .await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "{stdout}{stderr}");
    let line = regex::Regex::new(r"^  ✓ synced · 1 ↑ · \d+ B · avg .+ Mbps · \d+\.\ds$").unwrap();
    assert_eq!(stdout.lines().filter(|l| line.is_match(l)).count(), 1, "{stdout}");
    let detail = regex::Regex::new(r"^    ↑ 1 chunk · 1 at a time · direct 0 · api 1 · 0 retries").unwrap();
    assert_eq!(stdout.lines().filter(|l| detail.is_match(l)).count(), 1, "{stdout}");
    assert!(stderr.contains("chunk 1/1 · "), "-v's chunk line on stderr: {stderr}");
    assert!(
        !regex::Regex::new(r"MB/s|\b\d+ms\b").unwrap().is_match(&stdout),
        "{stdout}"
    );
}

/// Review P2 (#142), N2a for `bb sync`'s upload direction (spec §9.4): `bb sync -v --once` and `bb --json sync
/// --once` name the storage country of the init answer only, never its pool id, datacenter code or city, the session
/// token, the master key or a provider. (The download direction runs over `tests/direct_download/`'s mocks, which
/// serve no upload: `verbose_sync_download_prints_no_secret_or_provider`.)
#[tokio::test(flavor = "multi_thread")]
async fn verbose_sync_upload_prints_no_secret_or_provider() {
    let mock = Mock {
        init_extra: Some(json!({
            "storage_pool_id": "3e15382b-0000-4000-8000-000000000001",
            "region": "zz9",
            "storage_location": {"city": "Testville", "country": "Germany", "country_code": "DE"},
        })),
        ..Default::default()
    };
    let api = start_mock(mock.clone()).await;
    const TOKEN: &str = "tok-2333-session-secret";
    // Base64 of the bytes 1..=32: a throwaway key the mock never checks.
    const KEY: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";
    let home = scratch_home_with(&api, TOKEN, KEY);

    for (flags, dir) in [(["sync", "-v"], "syncdir-v"), (["--json", "sync"], "syncdir-json")] {
        let local = home.join(dir);
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(local.join("a.txt"), b"hello").unwrap();
        let mut args = vec!["--api", api.as_str()];
        args.extend(flags);
        args.extend(["--once", local.to_str().unwrap(), "/SyncUp"]);
        let out = bb(&home, &args).await;
        let (stdout, stderr) = text(&out);
        assert!(out.status.success(), "{args:?}: {stdout}{stderr}");
        let shown = format!("{stdout}\n{stderr}");
        if flags[0] == "--json" {
            let v: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
            assert_eq!(
                v["transfer_upload"]["pools"],
                json!([{"country": "Germany", "country_code": "DE", "chunks": 1}]),
                "{v}"
            );
        } else {
            assert!(stdout.contains("pool Germany"), "the country is shown: {shown}");
        }
        assert_absent(&shown, &["3e15382b", "zz9", "Testville", TOKEN, KEY]);
        assert_absent(&shown, &PROVIDER_NAMES);
    }
}
