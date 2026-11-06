//! Task 2333, plan Task 4: the `LiveProgress` sink (spec §6.1, §6.3, §6.7, §6.8).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::sink::*;
use crate::transfer_stats::{Direction, Held, PoolLabel, Route, Tally};
use crate::upload::{ChunkProgress, Stream};

const GUUS: &str = "A House of Dynamite (2025) (2160p NF WEB-DL Hybrid H265 DV HDR DDP Atmos 5.1 English - HONE).mkv";
const TOTAL: u64 = 17_480_003_668;
const CHUNK: u64 = 134_217_756;
/// 134,217,756 B every 3,384 ms is 317.3 Mbps; 66 chunks leave an ETA of 3m 38s.
const CHUNK_MS: u64 = 3384;

/// Spec §6.3's scenario: Guus's file, 66 of 131 chunks confirmed back to back, the clock at the last one.
fn guus_at_66(level: Level) -> (LiveProgress, Arc<Mutex<Vec<String>>>) {
    let clock = Arc::new(AtomicU64::new(0));
    let (p, record) = LiveProgress::for_test(Direction::Upload, level, 80, Arc::clone(&clock));
    p.plan_file(TOTAL, 131);
    p.seal_plan();
    p.set_limit(1);
    let f = p.begin_file_chunks(GUUS, TOTAL, 131, 134_217_728);
    f.pool(PoolLabel {
        id: Some(uuid::Uuid::parse_str("3e15382b-0000-4000-8000-000000000001").unwrap()),
        country: Some("Germany".into()),
        country_code: Some("DE".into()),
    });
    for i in 0..66u32 {
        clock.store(i as u64 * CHUNK_MS, Ordering::SeqCst);
        f.chunk_started(i, Route::Direct);
        clock.store((i as u64 + 1) * CHUNK_MS, Ordering::SeqCst);
        f.chunk_done(i, CHUNK, Held::Transferred);
        f.chunk_settled(i);
    }
    drop(f); // still uploading: never finished, the file stays active
    (p, record)
}

#[test]
fn default_level_renders_run_and_file_lines() {
    let (p, _) = guus_at_66(Level::Default);
    assert_eq!(
        p.render_now(),
        [
            "  ━━━━━━━━━━━━━──────────────  50% 8.86 GB / 17.48 GB · 317.3 Mbps · eta 3m 38s",
            "    A House of Dynamite (2025) (2160p NF WEB-DL … Atmos 5.1 English - HONE).mkv",
        ]
    );
}

#[test]
fn verbose_level_adds_stats_pool_and_chunk_lines() {
    let (p, record) = guus_at_66(Level::Verbose);
    let lines = p.render_now();
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(lines[1], "    chunks 66/131 · 0/1 in flight · avg 317.3 Mbps");
    assert_eq!(
        lines[2],
        "    pool 3e15382b · Germany · direct 66 · api 0 · 0 retries · chunk 134.2 MB"
    );
    let record = record.lock().unwrap();
    let chunks: Vec<&String> = record.iter().filter(|l| l.starts_with("err:    chunk ")).collect();
    assert_eq!(chunks.len(), 66);
    assert_eq!(
        chunks[65],
        "err:    chunk 66/131 · 134.2 MB · direct · 3.4s · 317.3 Mbps · 0 retries"
    );
}

#[test]
fn note_and_suspend_go_around_the_live_lines() {
    let (p, record) = guus_at_66(Level::Default);
    record.lock().unwrap().clear();
    p.render_now();
    p.note("  resuming x", Stream::Stdout);
    p.suspend(&mut || p.note("  inside", Stream::Stderr));
    p.render_now();
    let record = record.lock().unwrap();
    let kinds: Vec<&str> = record.iter().map(|l| &l[..l.find(':').unwrap()]).collect();
    assert_eq!(kinds, ["live", "live", "out", "err", "live", "live"], "{record:?}");
    assert_eq!(record[2], "out:  resuming x");
}

#[test]
fn plain_draw_prints_chunk_lines_only_when_verbose() {
    for (level, want) in [(Level::Default, 0), (Level::Verbose, 1)] {
        let (p, record) = LiveProgress::for_draw(
            Direction::Upload,
            ViewOptions {
                level,
                draw: Draw::Plain,
            },
        );
        p.plan_file(10, 1);
        p.seal_plan();
        let f = p.begin_file_chunks("a.txt", 10, 1, 10);
        f.chunk_started(0, Route::Api);
        f.chunk_done(0, 10, Held::Transferred);
        f.chunk_settled(0);
        let record = record.lock().unwrap();
        let chunk_lines = record.iter().filter(|l| l.starts_with("err:    chunk 1/1")).count();
        assert_eq!(chunk_lines, want, "{record:?}");
        assert!(
            !record.iter().any(|l| l.starts_with("live:")),
            "no live lines off a terminal"
        );
    }
}

#[test]
fn off_draw_records_nothing() {
    let (p, record) = LiveProgress::for_draw(
        Direction::Upload,
        ViewOptions {
            level: Level::Verbose,
            draw: Draw::Off,
        },
    );
    p.plan_file(10, 1);
    let f = p.begin_file_chunks("a.txt", 10, 1, 10);
    f.chunk_started(0, Route::Api);
    f.chunk_done(0, 10, Held::Transferred);
    f.chunk_settled(0);
    p.rollback_tally(
        "a.txt",
        Tally {
            bytes: 10,
            chunks: 1,
            already_stored: 0,
        },
    );
    assert!(record.lock().unwrap().is_empty(), "{:?}", record.lock().unwrap());
    // The drivers' notices still print, as they do today.
    p.note("  a.txt: the interrupted upload expired on the server", Stream::Stderr);
    assert_eq!(record.lock().unwrap().len(), 1);
    assert_eq!(p.summary().chunks_done, 0, "the model ran: done, then rolled back");
}

#[test]
fn draw_mode_table() {
    use Draw::*;
    // (rich, quiet_or_json, stdout tty, stderr tty) → draw
    let rows = [
        (true, false, true, true, Terminal),
        (true, false, true, false, Plain),
        (true, false, false, true, Plain),
        (true, false, false, false, Plain),
        (false, true, true, true, Off),
        (false, true, false, false, Off),
    ];
    for (rich, quiet_or_json, out_tty, err_tty, want) in rows {
        for verbose in [false, true] {
            let o = ViewOptions::decide(rich, quiet_or_json, out_tty, err_tty, verbose);
            assert_eq!(o.draw, want, "{rich} {quiet_or_json} {out_tty} {err_tty}");
            assert_eq!(o.level, if verbose { Level::Verbose } else { Level::Default });
        }
    }
}

#[test]
fn suspend_while_ticking_never_deadlocks() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let p = Arc::new(LiveProgress::for_ticker_test(Duration::from_millis(1)));
        p.plan_file(1_000_000, 1000);
        p.seal_plan();
        let events = {
            let p = Arc::clone(&p);
            std::thread::spawn(move || {
                let f = p.begin_file_chunks("b", 1_000_000, 1000, 1000);
                for i in 0..1000 {
                    f.chunk_started(i, Route::Api);
                    f.chunk_done(i, 1000, Held::Transferred);
                    f.chunk_settled(i);
                }
            })
        };
        p.suspend(&mut || {
            // Hold the suspension long enough that the 1 ms ticker tries to draw inside it.
            std::thread::sleep(Duration::from_millis(100));
            for _ in 0..1000 {
                p.unplan_file(0, 0);
                p.note("  inside the prompt", Stream::Stderr);
            }
        });
        events.join().unwrap();
        p.finish_all();
        done_tx.send(()).unwrap();
    });
    assert!(
        done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        "deadlock: suspend + ticker + events did not finish in 5 s"
    );
}

#[test]
fn ticker_stops_in_finish_all() {
    let p = LiveProgress::for_ticker_test(Duration::from_millis(1));
    assert!(p.ticker_running());
    p.finish_all();
    assert!(!p.ticker_running());
    p.finish_all(); // idempotent
    let _ = p.summary(); // calls finish_all again
    assert!(!p.ticker_running());
}

/// Spec §9.1 `concurrent_events_commute` (and `bar_progress_rollback_race`, moved
/// here): confirmations and rollbacks from many threads through the sink's lock
/// commute. Seeded first, so a rollback never meets an empty count and saturates.
#[test]
fn concurrent_events_commute() {
    const ITERATIONS: usize = 300;
    const INC_THREADS: u32 = 8;
    const INCS_PER_THREAD: u32 = 200;
    const ROLLBACK_THREADS: u32 = 4;
    const ROLLBACKS_PER_THREAD: u32 = 100;
    let seed = ROLLBACK_THREADS * ROLLBACKS_PER_THREAD;
    for iteration in 0..ITERATIONS {
        let (p, _) = LiveProgress::for_draw(
            Direction::Upload,
            ViewOptions {
                level: Level::Default,
                draw: Draw::Off,
            },
        );
        let chunks = seed + INC_THREADS * INCS_PER_THREAD;
        p.plan_file(chunks as u64 * 3, chunks);
        p.seal_plan();
        let seeded = p.begin_file_chunks("seed", seed as u64 * 3, seed, 3);
        for i in 0..seed {
            seeded.chunk_done(i, 3, Held::Transferred);
        }
        std::thread::scope(|scope| {
            for t in 0..INC_THREADS {
                let p = &p;
                scope.spawn(move || {
                    let f = p.begin_file_chunks("race.bin", INCS_PER_THREAD as u64 * 3, INCS_PER_THREAD, 3);
                    for i in 0..INCS_PER_THREAD {
                        f.chunk_started(t * 1000 + i, Route::Api);
                        f.chunk_done(t * 1000 + i, 3, Held::Transferred);
                        f.chunk_settled(t * 1000 + i);
                    }
                    f.finish(true);
                });
            }
            for _ in 0..ROLLBACK_THREADS {
                let p = &p;
                scope.spawn(move || {
                    for _ in 0..ROLLBACKS_PER_THREAD {
                        p.rollback_tally(
                            "seed",
                            Tally {
                                bytes: 3,
                                chunks: 1,
                                already_stored: 0,
                            },
                        );
                    }
                });
            }
        });
        let s = p.summary();
        let confirmed = seed + INC_THREADS * INCS_PER_THREAD;
        assert_eq!(s.chunks_done, confirmed - seed, "iteration {iteration}: lost update");
        assert_eq!(s.bytes_done, (confirmed - seed) as u64 * 3, "iteration {iteration}");
        assert!(s.peak <= INC_THREADS, "peak {}", s.peak);
    }
}
