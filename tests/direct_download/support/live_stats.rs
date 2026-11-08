//! Task 2333 (spec §6.5, §7, §9.4 N3, §9.5): what `bb pull` prints about its transfer, over both routes of the
//! same mocks: the `transfer` object of `--json`, the end lines with readable times, the folder pull's summary,
//! `--zip` without a carriage-return line, and what `-v` must never show.

use super::*;

/// The claims guard's provider names (`scripts/claims-guard/policy.conf`, `PROVIDER_NAMES`), lowercase: a copy of
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

/// The fixture's ciphertext: what a download moves and counts.
fn encrypted_len(env: &Env) -> u64 {
    env.mock.fx.frames.iter().map(|f| f.len() as u64).sum()
}

/// `bb pull <id> -o out.bin` with `extra` before the id; the output as text.
fn pull_text(env: &Env, extra: &[&str]) -> (Output, String, String) {
    let id = env.mock.fx.file_id.clone();
    let mut args: Vec<&str> = vec!["pull"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&[id.as_str(), "-o", "out.bin"]);
    let out = bb(env, &args, &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out, stdout, stderr)
}

/// Spec §9.5: `bb --json pull` adds one `transfer` object; each chunk counts once, on the route it came by.
#[test]
fn pull_json_reports_transfer_stats() {
    for (discovery, route, limit) in [(Discovery::Offer, "direct", 4), (Discovery::Nothing, "api", 1)] {
        let env = setup(discovery, Grants::Normal, &[], &[]);
        let (out, json) = pull(&env, &[], &[]);
        assert!(out.status.success(), "{}", describe(&out));
        assert_identical(&env, &out);
        let t = &json["transfer"];
        let chunks = env.mock.fx.frames.len() as u64;
        assert_eq!(t["direction"], "download", "{}", describe(&out));
        assert_eq!(t["files_done"], 1);
        assert_eq!(t["files_total"], 1);
        assert_eq!(t["chunks_done"], chunks);
        assert_eq!(t["chunks_total"], chunks);
        assert_eq!(t["chunks_already_stored"], 0);
        assert_eq!(t["bytes_done"], encrypted_len(&env));
        assert_eq!(t["bytes_total"], encrypted_len(&env));
        assert_eq!(t["bytes_transferred"], encrypted_len(&env));
        assert_eq!(t["routes"][route], chunks, "{route}: {t}");
        assert_eq!(t["max_chunks_in_flight"], limit, "{t}");
        assert!(t["peak_chunks_in_flight"].as_u64().unwrap() <= limit, "{t}");
        assert_eq!(
            t["pools"],
            json!([{ "country": "Germany", "country_code": "DE", "chunks": chunks }])
        );
    }
}

/// N3 (spec §9.4): `bb -v pull` and `bb --json pull` name the storage country only: never the city, the datacenter
/// code, the pool id, the storage host or bucket, a signed link, the session token, the master key or a provider.
#[test]
fn verbose_pull_prints_no_secret_or_provider() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, stdout, stderr) = pull_text(&env, &["-v"]);
    assert!(out.status.success(), "{}", describe(&out));
    assert_clean(&env, &out);
    let (json_out, _) = pull(&env, &["--force"], &[]);
    assert!(json_out.status.success(), "{}", describe(&json_out));
    assert_clean(&env, &json_out);
    let storage = env.mock.storage_addr.to_string();
    let mut needles = vec![
        CITY,
        DATACENTER,
        POOL_ID,
        &POOL_ID[..8],
        storage.as_str(),
        BUCKET,
        SIG,
        "test-session-token",
        env.mock.fx.key_b64.as_str(),
    ];
    needles.extend(PROVIDER_NAMES);
    let all = format!("{stdout}{stderr}{}", describe(&json_out));
    assert_absent(&all, &needles);
    // What it does show: one line per chunk (stderr is not a terminal), and the country in the detail line.
    let chunks = env.mock.fx.frames.len();
    let chunk_lines = stderr.lines().filter(|l| l.trim_start().starts_with("chunk ")).count();
    assert_eq!(chunk_lines, chunks, "{stderr}");
    assert!(stderr.contains(&format!("chunk 1/{chunks} · ")), "{stderr}");
    assert!(stdout.contains("· pool Germany"), "{stdout}");
}

/// N3's needles: what no output of a download may carry.
fn n3_needles<'a>(env: &'a Env, storage: &'a str) -> Vec<&'a str> {
    let mut needles = vec![
        CITY,
        DATACENTER,
        POOL_ID,
        &POOL_ID[..8],
        storage,
        BUCKET,
        SIG,
        "test-session-token",
        env.mock.fx.key_b64.as_str(),
    ];
    needles.extend(PROVIDER_NAMES);
    needles
}

/// Review P2 (#142), N3 for `bb sync`'s download direction: `bb sync -v --once` and `bb --json sync --once` over the
/// direct route name the storage country of the file's row only (N3's list). The upload direction:
/// `tests/transfer_stats_output.rs` `verbose_sync_upload_prints_no_secret_or_provider`.
#[test]
fn verbose_sync_download_prints_no_secret_or_provider() {
    for flags in [["sync", "-v"], ["--json", "sync"]] {
        let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
        let dir = env.cwd.join("syncdir");
        std::fs::create_dir_all(&dir).unwrap();
        let mut args = vec!["--api", env.api_url.as_str()];
        args.extend(flags);
        args.extend(["--once", dir.to_str().unwrap(), "/SyncUp"]);
        let out = bb(&env, &args, &[]);
        assert!(out.status.success(), "{flags:?}: {}", describe(&out));
        assert_eq!(
            std::fs::read(dir.join("multi.bin")).unwrap_or_default(),
            env.mock.fx.plaintext
        );
        assert_clean(&env, &out);
        let storage = env.mock.storage_addr.to_string();
        assert_absent(&describe(&out), &n3_needles(&env, &storage));
        let stdout = String::from_utf8_lossy(&out.stdout);
        if flags[0] == "--json" {
            let json: Value = serde_json::from_str(&stdout).unwrap_or(Value::Null);
            let chunks = env.mock.fx.frames.len();
            assert_eq!(
                json["transfer_download"]["pools"],
                json!([{ "country": "Germany", "country_code": "DE", "chunks": chunks }]),
                "{}",
                describe(&out)
            );
        } else {
            assert!(stdout.contains("pool Germany"), "{}", describe(&out));
        }
    }
}

/// Review P2 (#142), N3 for the other pull forms under `-v`: a folder, `--zip`, and a single file whose direct
/// route is refused, so the download moves to the API stream.
#[test]
fn verbose_folder_zip_and_fallback_pulls_print_no_secret_or_provider() {
    let folder = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let zip = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let fallback = setup(Discovery::Offer, Grants::DirectUnavailable, &[], &[]);
    let id = fallback.mock.fx.file_id.clone();
    for (env, args) in [
        (&folder, vec!["pull", SYNC_FOLDER_ID, "-o", "pulled", "-v"]),
        (&zip, vec!["pull", "--zip", "/SyncUp", "-o", "sync.zip", "-v"]),
        (&fallback, vec!["pull", id.as_str(), "-o", "out.bin", "-v"]),
    ] {
        let out = bb(env, &args, &[]);
        assert!(out.status.success(), "{args:?}: {}", describe(&out));
        assert_clean(env, &out);
        let storage = env.mock.storage_addr.to_string();
        assert_absent(&describe(&out), &n3_needles(env, &storage));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("chunk 1/"),
            "-v printed its chunk lines: {}",
            describe(&out)
        );
    }
    assert_eq!(
        std::fs::read(fallback.cwd.join("out.bin")).unwrap_or_default(),
        fallback.mock.fx.plaintext
    );
    assert_eq!(
        fallback.mock.log.lock().unwrap().api_download_hits,
        1,
        "premise: the API stream served it"
    );
}

/// The negative check above can fail: it flags a planted city and provider name.
#[test]
fn the_pull_absence_check_can_fail() {
    let planted = format!("pool {CITY} · hetzner · {DATACENTER}");
    for needle in [CITY, "hetzner", DATACENTER] {
        let caught = std::panic::catch_unwind(|| assert_absent(&planted, &[needle]));
        assert!(caught.is_err(), "{needle} is flagged");
    }
}

/// Spec §6.5: the end lines read `{time} · streamed` in the §5 duration form; no raw `ms` token is left.
#[test]
fn pull_end_lines_have_no_raw_ms() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let (out, stdout, stderr) = pull_text(&env, &[]);
    assert!(out.status.success(), "{}", describe(&out));
    let time = regex::Regex::new(r"^    (\d+\.\ds|\d+m \d\ds|\d+h \d\dm \d\ds) · streamed$").unwrap();
    assert_eq!(stdout.lines().filter(|l| time.is_match(l)).count(), 1, "{stdout}");
    let file = regex::Regex::new(r"^  ✓ multi\.bin  21 KB  (\d+\.\d|<0\.1) Mbps$").unwrap();
    assert_eq!(stdout.lines().filter(|l| file.is_match(l)).count(), 1, "{stdout}");
    let raw_ms = regex::Regex::new(r"\b\d+ms\b").unwrap();
    assert!(
        !raw_ms.is_match(&stdout) && !raw_ms.is_match(&stderr),
        "{stdout}{stderr}"
    );
    assert!(!stderr.contains('\r'), "no live line on a pipe: {stderr:?}");
}

/// Spec §6.5: a folder pull ends with a summary line; each file keeps its two lines, in the new units.
#[test]
fn pull_folder_prints_a_summary() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let out = bb(&env, &["pull", SYNC_FOLDER_ID, "-o", "pulled"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{}", describe(&out));
    let summary = regex::Regex::new(r"^  ✓ 1 file pulled · 21 KB · avg .+ Mbps · .+ total$").unwrap();
    assert_eq!(stdout.lines().filter(|l| summary.is_match(l)).count(), 1, "{stdout}");
    let time = regex::Regex::new(r"^      (\d+\.\ds|\d+m \d\ds) · streamed$").unwrap();
    assert_eq!(stdout.lines().filter(|l| time.is_match(l)).count(), 1, "{stdout}");
    assert!(!regex::Regex::new(r"\b\d+ms\b").unwrap().is_match(&stdout), "{stdout}");
}

/// Spec §3.1: `--zip` draws no carriage-return line of its own; on a pipe its end lines use the §5 durations, and
/// `--json` carries the `transfer` object.
#[test]
fn pull_zip_has_no_carriage_return_line() {
    let env = setup(Discovery::Offer, Grants::Normal, &[], &[]);
    let out = bb(&env, &["pull", "--zip", "/SyncUp", "-o", "sync.zip"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{}", describe(&out));
    assert!(!stderr.contains('\r') && !stdout.contains('\r'), "{}", describe(&out));
    let detail =
        regex::Regex::new(r"^    1 file · (\d+\.\ds|\d+m \d\ds) download · (\d+\.\ds|\d+m \d\ds) zip$").unwrap();
    assert_eq!(stdout.lines().filter(|l| detail.is_match(l)).count(), 1, "{stdout}");
    assert!(!regex::Regex::new(r"\b\d+ms\b").unwrap().is_match(&stdout), "{stdout}");

    let out = bb(&env, &["--json", "pull", "--zip", "/SyncUp", "-o", "sync2.zip"], &[]);
    assert!(out.status.success(), "{}", describe(&out));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let t = &json["transfer"];
    assert_eq!(t["direction"], "download", "{}", describe(&out));
    assert_eq!(t["files_done"], 1, "{t}");
    assert_eq!(t["chunks_done"], env.mock.fx.frames.len() as u64, "{t}");
}
