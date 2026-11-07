//! Spec §9.1 (task 2333): the transfer statistics model, on a fake clock.
//! Times are milliseconds; sizes are ciphertext bytes.

use super::*;

const MB: u64 = 1_000_000;

fn mbps(bytes_per_sec: f64) -> f64 {
    (bytes_per_sec * 8.0 / 1_000_000.0 * 10.0).round() / 10.0
}

fn rate(s: &Snapshot) -> f64 {
    match s.current {
        Speed::BytesPerSec(r) => r,
        ref other => panic!("expected a rate, got {other:?}"),
    }
}

/// An upload run with one planned file of `chunks` chunks of `chunk_bytes`, the plan sealed.
fn one_file(chunks: u32, chunk_bytes: u64) -> (TransferStats, FileSlot) {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(chunks as u64 * chunk_bytes, chunks);
    t.seal_plan();
    t.set_limit(1);
    let f = t.begin_file("movie.mkv", chunks as u64 * chunk_bytes, chunks, chunk_bytes);
    (t, f)
}

/// One chunk in flight from `start` to `end`, confirmed as transferred.
fn chunk(t: &mut TransferStats, f: FileSlot, index: u32, bytes: u64, start: u64, end: u64) -> ChunkRecord {
    t.chunk_started(f, index, Route::Api, start);
    let r = t.chunk_done(f, index, bytes, Held::Transferred, end);
    t.chunk_settled(f, index, end);
    r
}

fn assert_invariants(t: &TransferStats, now: u64) {
    let s = t.snapshot(now);
    assert!(
        s.chunks_done <= s.chunks_total,
        "I1 chunks {} > {}",
        s.chunks_done,
        s.chunks_total
    );
    assert!(
        s.bytes_done <= s.bytes_total,
        "I1 bytes {} > {}",
        s.bytes_done,
        s.bytes_total
    );
    assert!(s.peak >= s.in_flight, "I2");
    assert!(s.chunks_already_stored <= s.chunks_done, "I3");
}

#[test]
fn current_rate_one_stream() {
    let (mut t, f) = one_file(10, 12_500_000);
    for i in 0..3 {
        chunk(&mut t, f, i, 12_500_000, i as u64 * 1000, (i as u64 + 1) * 1000);
    }
    let s = t.snapshot(3000);
    assert_eq!(mbps(rate(&s)), 100.0);
    assert_eq!(mbps(s.avg_bytes_per_sec.unwrap()), 100.0);
}

#[test]
fn current_rate_uses_the_newest_ten_seconds() {
    let (mut t, f) = one_file(25, 2_500_000);
    for i in 0..15u64 {
        chunk(&mut t, f, i as u32, 1_250_000, i * 1000, (i + 1) * 1000);
    }
    for i in 15..25u64 {
        chunk(&mut t, f, i as u32, 2_500_000, i * 1000, (i + 1) * 1000);
    }
    let s = t.snapshot(25_000);
    assert_eq!(mbps(rate(&s)), 20.0);
    assert_eq!(mbps(s.avg_bytes_per_sec.unwrap()), 14.0, "43,750,000 B over 25 s");
}

#[test]
fn current_rate_parallel_streams() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(8 * 12_500_000, 8);
    t.seal_plan();
    t.set_limit(4);
    let f = t.begin_file("a", 8 * 12_500_000, 8, 12_500_000);
    for (round, base) in [(0u32, 0u64), (4, 2000)] {
        for i in round..round + 4 {
            t.chunk_started(f, i, Route::Direct, base);
        }
        for i in round..round + 4 {
            t.chunk_done(f, i, 12_500_000, Held::Transferred, base + 2000);
            t.chunk_settled(f, i, base + 2000);
        }
    }
    let s = t.snapshot(4000);
    assert_eq!(mbps(rate(&s)), 200.0);
    assert_eq!(s.peak, 4);
}

#[test]
fn stall_after_three_chunk_times() {
    for (dur, before, after) in [(10_000u64, 29_000u64, 31_000u64), (20_000, 59_000, 61_000)] {
        let (mut t, f) = one_file(3, MB);
        chunk(&mut t, f, 0, MB, 0, dur);
        t.chunk_started(f, 1, Route::Api, dur);
        assert_ne!(
            t.snapshot(dur + before).current,
            Speed::Stalled,
            "{dur} ms chunk, +{before}"
        );
        assert_eq!(
            t.snapshot(dur + after).current,
            Speed::Stalled,
            "{dur} ms chunk, +{after}"
        );
    }
}

#[test]
fn idle_between_files() {
    let (mut t, f) = one_file(3, MB);
    chunk(&mut t, f, 0, MB, 0, 1000);
    let s = t.snapshot(1000 + 31_000);
    assert_eq!(s.current, Speed::Idle);
    assert_eq!(s.eta_secs, None);
}

#[test]
fn no_rate_before_the_first_confirmation() {
    let (mut t, f) = one_file(3, MB);
    t.chunk_started(f, 0, Route::Api, 0);
    let s = t.snapshot(200_000);
    assert_eq!(s.current, Speed::NoRate);
    assert_eq!(s.eta_secs, None);
    assert_eq!(s.avg_bytes_per_sec, None);
    assert_eq!(s.in_flight, 1);
}

#[test]
fn eta_needs_three_seconds_of_window() {
    let (mut t, f) = one_file(10, MB);
    chunk(&mut t, f, 0, MB, 0, 2900);
    assert_eq!(t.snapshot(2900).eta_secs, None, "2.9 s of window");
    let (mut t, f) = one_file(10, MB);
    chunk(&mut t, f, 0, MB, 0, 3000);
    assert!(t.snapshot(3000).eta_secs.is_some(), "3.0 s of window");
}

#[test]
fn eta_hidden_until_the_plan_is_final() {
    let mut t = TransferStats::new(Direction::Download, 0);
    t.plan_file(10 * MB, 10);
    let f = t.begin_file("a", 10 * MB, 10, MB);
    chunk(&mut t, f, 0, MB, 0, 3000);
    assert_eq!(t.snapshot(3000).eta_secs, None, "plan not final");
    t.seal_plan();
    assert!(t.snapshot(3000).eta_secs.is_some(), "plan sealed");
}

/// Two 100,000,000 B chunks of 8 s each (12.5 MB/s), a third in flight, 100,000,000 B left.
fn eight_second_chunks() -> TransferStats {
    let (mut t, f) = one_file(3, 100 * MB);
    chunk(&mut t, f, 0, 100 * MB, 0, 8000);
    chunk(&mut t, f, 1, 100 * MB, 8000, 16_000);
    t.chunk_started(f, 2, Route::Api, 16_000);
    t
}

#[test]
fn eta_counts_down_and_never_reaches_zero() {
    let t = eight_second_chunks();
    assert_eq!(t.snapshot(16_000 + 3000).eta_secs, Some(5.0));
    assert_eq!(t.snapshot(16_000 + 7500).eta_secs, Some(1.0), "floor");
    assert_eq!(
        t.snapshot(16_000 + 11_000).eta_secs,
        Some(1.0),
        "floor: on time until +12 s"
    );
}

#[test]
fn eta_rises_when_the_next_chunk_is_late() {
    let t = eight_second_chunks();
    let late = t.snapshot(16_000 + 13_000).eta_secs.unwrap();
    assert!((late - 14.5).abs() < 1e-6, "200,000,000 B over 29 s: {late}");
    let later = t.snapshot(16_000 + 20_000).eta_secs.unwrap();
    assert!((later - 18.0).abs() < 1e-6, "200,000,000 B over 36 s: {later}");
    let s = t.snapshot(16_000 + 31_000);
    assert_eq!(s.current, Speed::Stalled);
    assert_eq!(s.eta_secs, None);
}

#[test]
fn current_speed_falls_while_late() {
    let t = eight_second_chunks();
    assert_eq!(mbps(rate(&t.snapshot(16_000))), 100.0, "on time");
    assert_eq!(mbps(rate(&t.snapshot(16_000 + 13_000))), 55.2, "late");
}

#[test]
fn already_stored_counts_as_done_not_as_transferred() {
    let (mut t, f) = one_file(12, MB);
    for i in 0..10 {
        t.chunk_started(f, i, Route::Direct, 0);
        let r = t.chunk_done(f, i, MB, Held::AlreadyStored, 10);
        t.chunk_settled(f, i, 10);
        assert_eq!(r.held, Held::AlreadyStored);
    }
    chunk(&mut t, f, 10, MB, 1000, 2000);
    chunk(&mut t, f, 11, MB, 2000, 3000);
    let s = t.snapshot(3000);
    assert_eq!(s.chunks_done, 12);
    assert_eq!(s.chunks_already_stored, 10);
    assert_eq!(s.bytes_transferred, 2 * MB);
    assert_eq!(s.bytes_done, 12 * MB);
    assert_eq!(
        rate(&s),
        1_000_000.0,
        "2 MB over 2 s: the held chunks are not in the rate"
    );
}

#[test]
fn rollback_never_double_counts() {
    let (mut t, f) = one_file(5, MB);
    let mut now = 0;
    for i in 0..3 {
        chunk(&mut t, f, i, MB, now, now + 100);
        now += 100;
        assert_invariants(&t, now);
    }
    t.file_done(f, false);
    t.rollback(Tally {
        bytes: 3 * MB,
        chunks: 3,
        already_stored: 0,
    });
    assert_invariants(&t, now);
    let retry = t.begin_file("movie.mkv", 5 * MB, 5, MB);
    for i in 0..5 {
        chunk(&mut t, retry, i, MB, now, now + 100);
        now += 100;
        assert_invariants(&t, now);
    }
    t.file_done(retry, true);
    let s = t.snapshot(now);
    assert_eq!(s.chunks_done, 5);
    assert_eq!(s.chunks_total, 5);
    assert_eq!(s.bytes_done, 5 * MB);
    assert_eq!(s.bytes_transferred, 8 * MB, "speed counts the re-sent chunks");
    assert_eq!(s.files_done, 1);
}

#[test]
fn rollback_saturates() {
    let (mut t, f) = one_file(5, MB);
    chunk(&mut t, f, 0, MB, 0, 100);
    t.rollback(Tally {
        bytes: 9 * MB,
        chunks: 9,
        already_stored: 9,
    });
    let s = t.snapshot(100);
    assert_eq!((s.bytes_done, s.chunks_done, s.chunks_already_stored), (0, 0, 0));
}

#[test]
fn in_flight_gauge_up_and_down() {
    let (mut t, f) = one_file(4, MB);
    let mut seen = Vec::new();
    t.chunk_started(f, 0, Route::Api, 0);
    seen.push(t.snapshot(0).in_flight);
    t.chunk_started(f, 1, Route::Api, 0);
    seen.push(t.snapshot(0).in_flight);
    t.chunk_settled(f, 0, 1);
    seen.push(t.snapshot(1).in_flight);
    t.chunk_started(f, 2, Route::Api, 2);
    seen.push(t.snapshot(2).in_flight);
    t.chunk_settled(f, 1, 3);
    seen.push(t.snapshot(3).in_flight);
    t.chunk_settled(f, 2, 4);
    seen.push(t.snapshot(4).in_flight);
    assert_eq!(seen, [1, 2, 1, 2, 1, 0]);
    assert_eq!(t.snapshot(4).peak, 2);
    t.chunk_settled(f, 2, 5);
    assert_eq!(t.snapshot(5).in_flight, 0, "a second settle never goes negative");
}

#[test]
fn retries_count_per_chunk_and_run() {
    let (mut t, f) = one_file(2, MB);
    t.chunk_started(f, 0, Route::Api, 0);
    t.chunk_retried(f, 0);
    t.chunk_retried(f, 0);
    let r0 = t.chunk_done(f, 0, MB, Held::Transferred, 100);
    t.chunk_settled(f, 0, 100);
    t.chunk_started(f, 1, Route::Direct, 100);
    t.chunk_retried(f, 1);
    let r1 = t.chunk_done(f, 1, MB, Held::Transferred, 200);
    t.chunk_settled(f, 1, 200);
    assert_eq!((r0.retries, r1.retries), (2, 1));
    assert_eq!(
        (r0.index, r0.chunk_count, r0.duration_ms, r0.route),
        (0, 2, 100, Route::Api)
    );
    assert_eq!(r1.route, Route::Direct);
    let s = t.snapshot(200);
    assert_eq!(s.retries, 3);
    assert_eq!((s.direct, s.api), (1, 1));
}

#[test]
fn a_skipped_file_leaves_the_plan() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(10 * MB, 10);
    t.plan_file(4 * MB, 4);
    t.next_file(4 * MB, 4);
    t.end_file(FileEnd::NotSent);
    t.seal_plan();
    let s = t.snapshot(0);
    assert_eq!((s.files_total, s.bytes_total, s.chunks_total), (1, 10 * MB, 10));
}

#[test]
fn pools_are_listed_once_with_chunk_counts() {
    let pool = |country: &str| PoolLabel {
        country: Some(country.to_string()),
        country_code: None,
    };
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(6 * MB, 6);
    t.seal_plan();
    let a = t.begin_file("a", 3 * MB, 3, MB);
    t.pool(a, pool("Germany"));
    for i in 0..3 {
        chunk(&mut t, a, i, MB, 0, 10);
    }
    let b = t.begin_file("b", 2 * MB, 2, MB);
    for i in 0..2 {
        chunk(&mut t, b, i, MB, 10, 20);
    }
    // A resumed upload learns its pool at completion, after its chunks.
    t.pool(b, pool("Finland"));
    // Another pool in the same country is the same place to a user: one entry.
    let c = t.begin_file("c", MB, 1, MB);
    t.pool(c, pool("Germany"));
    chunk(&mut t, c, 0, MB, 20, 30);
    let s = t.snapshot(30);
    let got: Vec<(Option<String>, u32)> = s.pools.iter().map(|p| (p.label.country.clone(), p.chunks)).collect();
    assert_eq!(got, [(Some("Germany".into()), 4), (Some("Finland".into()), 2)]);
}

#[test]
fn summary_with_zero_duration() {
    let (mut t, f) = one_file(1, MB);
    t.chunk_started(f, 0, Route::Api, 0);
    t.chunk_done(f, 0, MB, Held::Transferred, 0);
    t.chunk_settled(f, 0, 0);
    let at_start = t.summary(0);
    assert_eq!(at_start.duration_ms, 0);
    assert_eq!(at_start.avg_mbps, None);
    assert_eq!(t.snapshot(0).avg_bytes_per_sec, None);
    let (t, _) = one_file(1, MB);
    assert_eq!(t.summary(5000).avg_mbps, None, "nothing transferred");
    let json = t.summary(0).to_json().to_string();
    assert!(!json.contains("NaN") && !json.contains("inf"), "{json}");
}

#[test]
fn summary_json_has_the_contract_fields() {
    let (mut t, f) = one_file(2, 12_500_000);
    t.pool(
        f,
        PoolLabel {
            country: Some("Germany".into()),
            country_code: Some("DE".into()),
        },
    );
    chunk(&mut t, f, 0, 12_500_000, 0, 1000);
    chunk(&mut t, f, 1, 12_500_000, 1000, 2000);
    t.file_done(f, true);
    let j = t.summary(2000).to_json();
    let keys: Vec<&str> = j.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "direction",
        "files_done",
        "files_total",
        "bytes_done",
        "bytes_total",
        "bytes_transferred",
        "chunks_done",
        "chunks_total",
        "chunks_already_stored",
        "duration_ms",
        "avg_mbps",
        "peak_chunks_in_flight",
        "max_chunks_in_flight",
        "routes",
        "retries",
        "pools",
    ];
    want.sort_unstable();
    let mut keys = keys;
    keys.sort_unstable();
    assert_eq!(keys, want);
    assert_eq!(j["direction"], "upload");
    assert_eq!(j["avg_mbps"], 100.0);
    assert_eq!(j["routes"], serde_json::json!({"direct": 0, "api": 2}));
    assert_eq!(
        j["pools"],
        serde_json::json!([{"country": "Germany", "country_code": "DE", "chunks": 2}])
    );
    assert_eq!(j["max_chunks_in_flight"], 1);
}

// ── purity guard (spec §4.1) ────────────────────────────────────────────────

const FORBIDDEN: [&str; 11] = [
    "std::io",
    "Instant",
    "std::thread",
    "tokio",
    "indicatif",
    "crossterm",
    "colored",
    "println!",
    "eprintln!",
    "print!",
    "eprint!",
];

/// The production part of a source file: whole-line comments and every
/// `#[cfg(test)]` item removed, the way `tests/no_city_copy.rs` does.
fn production_text(source: &str) -> String {
    let mut out = String::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("#[cfg(test)]") {
            let mut item = lines.next().unwrap_or("");
            while item.trim_start().starts_with("#[") {
                item = lines.next().unwrap_or("");
            }
            let t = item.trim_start();
            let is_mod = t.starts_with("mod ") || t.starts_with("pub(crate) mod ");
            if is_mod && item.trim_end().ends_with('{') {
                for inner in lines.by_ref() {
                    if inner == "}" {
                        break;
                    }
                }
            }
            continue;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn hits(text: &str) -> Vec<&'static str> {
    FORBIDDEN.iter().copied().filter(|n| text.contains(n)).collect()
}

#[test]
fn transfer_stats_is_pure() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/transfer_stats");
    let mut scanned = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        // Test-only files hold the needles and the planted line.
        if path.is_dir() || name == "tests.rs" || !name.ends_with(".rs") {
            continue;
        }
        let text = production_text(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(hits(&text), Vec::<&str>::new(), "{name} is not pure");
        scanned.push(name);
    }
    scanned.sort();
    assert_eq!(scanned, ["files_at_once.rs", "mod.rs", "pool.rs", "window.rs"]);
    assert_eq!(
        hits("fn f() { println!(\"x\"); }"),
        ["println!"],
        "the scanner can fail"
    );
}

/// A file that failed for good keeps its plan (the run ends short of its total), but what it will never send is
/// not waiting to be sent: the ETA leaves it out (review finding, task 2333).
#[test]
fn eta_leaves_out_what_a_failed_file_will_not_send() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(300 * MB, 3);
    t.plan_file(1000 * MB, 10);
    t.seal_plan();
    t.set_limit(1);
    let f = t.begin_file("a", 300 * MB, 3, 100 * MB);
    chunk(&mut t, f, 0, 100 * MB, 0, 8000);
    chunk(&mut t, f, 1, 100 * MB, 8000, 16_000);
    t.chunk_started(f, 2, Route::Api, 16_000);
    // 1,100 MB left at 12.5 MB/s, 3 s after the newest chunk.
    assert_eq!(t.snapshot(19_000).eta_secs, Some(85.0));
    t.give_up(1000 * MB);
    assert_eq!(t.snapshot(19_000).eta_secs, Some(5.0), "only the running file is left");
    let s = t.snapshot(19_000);
    assert_eq!((s.bytes_done, s.bytes_total), (200 * MB, 1300 * MB), "the plan stays");
}

/// When everything left is given up, nothing is waiting to be sent: the ETA hides instead of reading `1s` (review
/// finding, task 2333; e.g. while a later file's conflict lookup runs).
#[test]
fn eta_hides_when_everything_left_is_given_up() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(300 * MB, 3);
    t.plan_file(1000 * MB, 10);
    t.seal_plan();
    t.set_limit(1);
    let f = t.begin_file("a", 300 * MB, 3, 100 * MB);
    chunk(&mut t, f, 0, 100 * MB, 0, 8000);
    chunk(&mut t, f, 1, 100 * MB, 8000, 16_000);
    chunk(&mut t, f, 2, 100 * MB, 16_000, 24_000);
    t.file_done(f, true);
    t.give_up(1000 * MB);
    let s = t.snapshot(27_000);
    assert!(matches!(s.current, Speed::BytesPerSec(_)), "premise: a rate exists");
    assert_eq!(s.eta_secs, None, "nothing is waiting to be sent");
}

/// Review P2 (task 2333): the command plans a file from its size; the driver reads the size again when the file
/// begins, after the conflict lookup and any prompt. A file that changed in between moves its share of the totals to
/// what the driver read, so done never passes total (I1). A second attempt at the same size changes nothing.
#[test]
fn a_file_that_begins_at_another_size_moves_its_share() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(100, 1);
    t.plan_file(50, 1);
    t.seal_plan();
    t.set_limit(1);
    t.next_file(100, 1);
    t.begin_file("grew.log", 300, 3, 100);
    let s = t.snapshot(0);
    assert_eq!(
        (s.files_total, s.bytes_total, s.chunks_total),
        (2, 350, 4),
        "the share follows the new size"
    );
    let f = t.begin_file("grew.log", 300, 3, 100);
    let s = t.snapshot(0);
    assert_eq!(
        (s.bytes_total, s.chunks_total),
        (350, 4),
        "a second attempt counts the file once"
    );
    for i in 0..3 {
        chunk(&mut t, f, i, 100, i as u64 * 10, i as u64 * 10 + 10);
    }
    t.file_done(f, true);
    t.end_file(FileEnd::Uploaded);
    let s = t.summary(30);
    assert_eq!((s.bytes_done, s.chunks_done), (300, 3));
    assert_eq!((s.bytes_total, s.chunks_total), (350, 4));
}

/// A file that is not sent takes its share out of the run; a failed one keeps it in the totals and gives up only
/// what it did not send (its share as the driver read it).
#[test]
fn a_file_that_ends_unsent_or_failed_settles_its_share() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(100, 1);
    t.plan_file(200, 2);
    t.seal_plan();
    t.set_limit(1);
    t.next_file(100, 1);
    t.end_file(FileEnd::NotSent);
    let s = t.snapshot(0);
    assert_eq!(
        (s.files_total, s.bytes_total, s.chunks_total),
        (1, 200, 2),
        "the skipped file left the run"
    );

    t.next_file(200, 2);
    let f = t.begin_file("b", 300, 3, 100);
    chunk(&mut t, f, 0, 100, 0, 10);
    t.file_done(f, false);
    t.end_file(FileEnd::Failed);
    let s = t.snapshot(10);
    assert_eq!(
        (s.files_total, s.bytes_total, s.chunks_total),
        (1, 300, 3),
        "the failed file keeps its share"
    );
    assert_eq!(t.given_up_bytes, 200, "what it did not send is given up");
}

/// A buffered body's length is known only once it arrived (a legacy file's JSON chunks are longer than the file row
/// says): the file's share of the plan follows it, as `begin_file` follows what a driver read, so done never passes
/// total (I1).
#[test]
fn a_buffered_body_longer_than_its_plan_grows_the_plan() {
    let mut t = TransferStats::new(Direction::Download, 0);
    t.plan_file(10 * MB, 2);
    t.plan_file(5 * MB, 1);
    t.seal_plan();
    t.next_file(10 * MB, 2);
    let f = t.begin_file("legacy.bin", 10 * MB, 2, 0);
    t.body_done(f, 2, 12 * MB, Route::Api, 1000);
    let s = t.snapshot(1000);
    assert_eq!((s.bytes_done, s.bytes_total), (12 * MB, 17 * MB));
    assert_eq!((s.chunks_done, s.chunks_total), (2, 3));
    // A body no longer than the plan leaves it alone.
    t.next_file(5 * MB, 1);
    let g = t.begin_file("b.bin", 5 * MB, 1, 0);
    t.body_done(g, 1, 4 * MB, Route::Api, 2000);
    assert_eq!(t.snapshot(2000).bytes_total, 17 * MB);
}

/// `--parallel` (spec §7): two files in flight at once, each under its own key. Each begins at the size the driver
/// read, and one that fails gives up only what IT did not send, whatever the other sent meanwhile.
#[test]
fn files_at_once_keep_their_own_shares() {
    let mut t = TransferStats::new(Direction::Upload, 0);
    t.plan_file(300 * MB, 3);
    t.plan_file(300 * MB, 3);
    t.seal_plan();
    t.set_limit(4);
    t.next_file_as(0, 300 * MB, 3);
    t.next_file_as(1, 300 * MB, 3);
    let a = t.begin_file_as(0, "a", 300 * MB, 3, 100 * MB);
    let b = t.begin_file_as(1, "b", 200 * MB, 2, 100 * MB);
    assert_eq!(
        t.snapshot(0).bytes_total,
        500 * MB,
        "b's share follows what the driver read"
    );
    chunk(&mut t, a, 0, 100 * MB, 0, 8000);
    chunk(&mut t, b, 0, 100 * MB, 0, 8000);
    chunk(&mut t, b, 1, 100 * MB, 8000, 16_000);
    t.file_done(b, true);
    t.end_file_as(1, FileEnd::Uploaded);
    t.chunk_started(a, 1, Route::Api, 16_000);
    t.end_file_as(0, FileEnd::Failed);
    // a sent 100 of its 300 MB: 200 MB given up; b's 200 MB do not count as a's.
    let s = t.snapshot(19_000);
    assert_eq!((s.bytes_done, s.bytes_total), (300 * MB, 500 * MB), "the plan stays");
    assert_eq!(
        s.eta_secs, None,
        "nothing is waiting to be sent: a gave up exactly what it did not send"
    );
    assert_eq!(t.given_up_bytes, 200 * MB);
}
