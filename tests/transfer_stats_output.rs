//! Task 2333 (spec §6, §9.4 N2a, §9.5): what `bb push` prints about its transfer, through the real binary against
//! the in-process mock of `support/upload_mock.rs`: the end lines on a pipe, `--verbose`, `--quiet`, the usage
//! error of both together, a failed or grown file in a folder, and what must never be shown.

#[path = "support/upload_mock.rs"]
mod upload_mock;

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

/// Spec §6.2: `--verbose` with `--quiet` is a usage error in either order, before anything is sent.
#[tokio::test(flavor = "multi_thread")]
async fn quiet_and_verbose_is_a_usage_error_in_both_orders() {
    let mock = Mock::default();
    let api = start_mock(mock.clone()).await;
    let home = scratch_home(&api);
    make_folder2(&home);

    for args in [
        ["--api", api.as_str(), "--quiet", "push", "-v", "folder2/a.txt"],
        ["--api", api.as_str(), "push", "-v", "--quiet", "folder2/a.txt"],
    ] {
        let out = bb(&home, &args).await;
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

/// N2a (spec §9.4; Guus 2026-11-06): `bb push -v` shows the pool as its country, and `bb --json push` carries the
/// pool's id and country; neither shows the datacenter code, the city, the session token, the master key or a
/// provider name, and the verbose lines show no pool id.
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
                v["transfer"]["pools"][0]["id"], "3e15382b-0000-4000-8000-000000000001",
                "{v}"
            );
            assert_eq!(v["transfer"]["pools"][0]["country"], "Germany", "{v}");
        } else {
            assert!(stdout.contains("· pool Germany"), "the country is shown: {shown}");
            assert_absent(&shown, &["3e15382b"]);
        }
        assert_absent(&shown, &["zz9", "Testville", TOKEN, KEY]);
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
        tokio::spawn(async move { bb(&home, &["--api", &api, "--json", "push", "folder2"]).await })
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
