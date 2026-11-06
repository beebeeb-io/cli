//! Spec §9.2 (task 2333): the line formatter, as plain strings.

use super::line::*;
use super::units::*;
use crate::transfer_stats::*;

const GUUS: &str = "A House of Dynamite (2025) (2160p NF WEB-DL Hybrid H265 DV HDR DDP Atmos 5.1 English - HONE).mkv";
/// 17,480,000,000 B in 131 chunks of 128 MiB, as ciphertext (spec §6.3).
const TOTAL: u64 = 17_480_003_668;
/// 66 whole chunks of 134,217,756 B.
const DONE: u64 = 8_858_371_896;

fn bps_for_mbps(mbps: f64) -> f64 {
    mbps * 1_000_000.0 / 8.0
}

fn snap(done: u64, total: u64) -> Snapshot {
    Snapshot {
        direction: Direction::Upload,
        files_done: 0,
        files_total: 1,
        plan_final: true,
        bytes_done: done,
        bytes_total: total,
        bytes_transferred: done,
        chunks_done: 66,
        chunks_total: 131,
        chunks_already_stored: 0,
        current: Speed::BytesPerSec(bps_for_mbps(317.3)),
        avg_bytes_per_sec: Some(bps_for_mbps(309.9)),
        eta_secs: Some(217.4),
        in_flight: 1,
        limit: 1,
        peak: 1,
        oldest_in_flight_ms: Some(2100),
        direct: 66,
        api: 0,
        retries: 0,
        pools: vec![PoolCount {
            label: PoolLabel {
                id: Some(uuid::Uuid::parse_str("3e15382b-0000-4000-8000-000000000001").unwrap()),
                country: Some("Germany".into()),
                country_code: Some("DE".into()),
            },
            chunks: 66,
        }],
        active: vec![FileView {
            name: GUUS.into(),
            bytes_done: done,
            bytes_total: total,
            chunk_size: 134_217_728,
        }],
    }
}

fn folder_snap() -> Snapshot {
    let mut s = snap(124_000_000, 142_000_000);
    s.files_done = 2;
    s.files_total = 23;
    s.current = Speed::BytesPerSec(bps_for_mbps(112.4));
    s.eta_secs = Some(2.0);
    s
}

fn summary(bytes: u64, duration_ms: u64) -> Summary {
    Summary {
        direction: Direction::Upload,
        files_done: 1,
        files_total: 1,
        bytes_done: bytes,
        bytes_total: bytes,
        bytes_transferred: bytes,
        chunks_done: 131,
        chunks_total: 131,
        chunks_already_stored: 0,
        duration_ms,
        avg_mbps: Some(317.3),
        peak: 1,
        max_in_flight: 1,
        direct: 131,
        api: 0,
        retries: 0,
        pools: snap(0, 1).pools,
    }
}

#[test]
fn run_line_widths() {
    let s = snap(DONE, TOTAL);
    let rows = [
        (
            100,
            "  ━━━━━━━━━━━━━━━───────────────  50% 8.86 GB / 17.48 GB · 317.3 Mbps · eta 3m 38s",
        ),
        (
            80,
            "  ━━━━━━━━━━━━━──────────────  50% 8.86 GB / 17.48 GB · 317.3 Mbps · eta 3m 38s",
        ),
        (60, "   50% 8.86 GB / 17.48 GB · 317.3 Mbps · eta 3m 38s"),
        (45, "   50% 8.86 GB / 17.48 GB · eta 3m 38s"),
        (30, "   50% 8.86 GB / 17.48 GB"),
    ];
    for (columns, want) in rows {
        assert_eq!(run_line(&s, columns), want, "{columns} columns");
    }
    let start = run_line(&snap(0, TOTAL), 100);
    assert!(start.contains("  0% 0 B / 17.48 GB"), "{start}");
    assert!(run_line(&snap(TOTAL - 1, TOTAL), 100).contains(" 99% "));
    assert!(run_line(&snap(TOTAL, TOTAL), 100).contains("100% "));
}

#[test]
fn run_line_several_files() {
    let s = folder_snap();
    assert_eq!(
        run_line(&s, 100),
        "  ━━━━━━━━━━━━━━━━━━━━━━━━━━────  87% 124.0 MB / 142.0 MB · 2/23 files · 112.4 Mbps · eta 2s"
    );
    assert_eq!(
        run_line(&s, 80),
        "  ━━━━━━━━━━━━━━───  87% 124.0 MB / 142.0 MB · 2/23 files · 112.4 Mbps · eta 2s"
    );
    assert_eq!(run_line(&s, 60), "   87% 124.0 MB / 142.0 MB · 112.4 Mbps · eta 2s");
    let file = FileView {
        name: "interview-03.m4a".into(),
        bytes_done: 29_360_324,
        bytes_total: 48_000_336,
        chunk_size: 4_194_304,
    };
    assert_eq!(file_line(&file, false, 80), "    interview-03.m4a  29.4 MB / 48.0 MB");
    assert_eq!(
        file_line(&snap(DONE, TOTAL).active[0], true, 80),
        "    A House of Dynamite (2025) (2160p NF WEB-DL … Atmos 5.1 English - HONE).mkv"
    );
}

#[test]
fn run_line_without_a_final_plan() {
    let mut s = snap(1_240_000_000, 1_240_000_000);
    s.plan_final = false;
    s.files_done = 3;
    s.current = Speed::BytesPerSec(bps_for_mbps(412.0));
    assert_eq!(run_line(&s, 100), "  1.24 GB · 3 files · 412.0 Mbps");
    s.files_done = 1;
    assert_eq!(run_line(&s, 100), "  1.24 GB · 1 file · 412.0 Mbps");
}

#[test]
fn verbose_lines_drop_from_the_end() {
    let s = snap(DONE, TOTAL);
    assert_eq!(
        stats_line(&s, 100),
        "    chunks 66/131 · 1/1 in flight · avg 309.9 Mbps · oldest 2.1s"
    );
    assert_eq!(stats_line(&s, 60), "    chunks 66/131 · 1/1 in flight · avg 309.9 Mbps");
    assert_eq!(stats_line(&s, 40), "    chunks 66/131 · 1/1 in flight");
    // Guus (2026-11-06): the pool is shown by its storage country only, never an id, a city or a provider.
    assert_eq!(
        pool_line(&s, 100),
        "    pool Germany · direct 66 · api 0 · 0 retries · chunk 134.2 MB"
    );
    assert_eq!(pool_line(&s, 60), "    pool Germany · direct 66 · api 0 · 0 retries");
    assert_eq!(pool_line(&s, 40), "    pool Germany · direct 66 · api 0");
    let mut download = s.clone();
    download.pools[0].label.id = None;
    download.avg_bytes_per_sec = None;
    download.oldest_in_flight_ms = None;
    assert!(pool_line(&download, 100).starts_with("    pool Germany · direct 66"));
    assert_eq!(stats_line(&download, 100), "    chunks 66/131 · 1/1 in flight · avg —");
}

#[test]
fn speed_formats() {
    assert_eq!(crate::ui::human_speed_network(12_500_000.0), "100.0 Mbps");
    assert_eq!(crate::ui::human_speed_network(6_000.0), "<0.1 Mbps");
    assert_eq!(
        crate::ui::human_speed_network(150_000_000.0),
        "1200.0 Mbps",
        "bb speedtest's old 1.2 Gbps"
    );
    assert_eq!(crate::ui::human_speed_network(1_500_000_000.0), "12000.0 Mbps");
    assert_eq!(crate::ui::human_speed_network(0.0), "—");
    assert_eq!(crate::ui::human_speed_network(f64::NAN), "—");
    assert_eq!(speed(&Speed::NoRate), "— Mbps");
    assert_eq!(speed(&Speed::Idle), "— Mbps");
    assert_eq!(speed(&Speed::Stalled), "stalled");
    assert_eq!(speed(&Speed::BytesPerSec(12_500_000.0)), "100.0 Mbps");
    assert_eq!(mbps(12_500_000, 1000), "100.0 Mbps");
    assert_eq!(mbps(12_500_000, 0), "—");
}

#[test]
fn eta_formats() {
    let rows = [
        (0.2, "1s"),
        (59.0, "59s"),
        (59.3, "1m 00s"),
        (65.0, "1m 05s"),
        (3599.2, "1h 00m"),
        (3720.0, "1h 02m"),
        (360_000.0, ">99h"),
    ];
    for (secs, want) in rows {
        assert_eq!(eta(Some(secs)), want, "{secs} s");
    }
    assert_eq!(eta(None), "—");
}

#[test]
fn duration_formats() {
    let rows = [
        (4_240, "4.2s"),
        (59_940, "59.9s"),
        (59_960, "1m 00s"),
        (92_100, "1m 32s"),
        (119_600, "2m 00s"),
        (440_673, "7m 21s"),
        (3_599_700, "1h 00m 00s"),
        (3_723_000, "1h 02m 03s"),
    ];
    for (ms, want) in rows {
        assert_eq!(duration(ms), want, "{ms} ms");
    }
}

#[test]
fn name_fitting() {
    assert_eq!(fit_name(GUUS, 40), "A House of Dynamite (20…lish - HONE).mkv");
    assert_eq!(fit_name("short.txt", 40), "short.txt");
    let cjk = "写真".repeat(30); // 60 characters, 120 columns
    let fitted = fit_name(&cjk, 40);
    let width = unicode_width::UnicodeWidthStr::width(fitted.as_str());
    assert!(
        fitted.contains('…') && (37..=40).contains(&width),
        "{fitted}: {width} columns"
    );
    assert_eq!(bytes(0), "0 B");
}

#[test]
fn push_end_lines() {
    assert_eq!(
        push_file_line(GUUS, TOTAL, TOTAL, 440_673, 80),
        "  ✓ A House of Dynamite (20…lish - HONE).mkv  17.48 GB   317.3 Mbps   7m 21s"
    );
    let s = summary(TOTAL, 440_673);
    assert_eq!(
        summary_line("uploaded", 1, ("failed", 0), TOTAL, &s),
        "1 file uploaded · 17.48 GB · avg 317.3 Mbps · 7m 21s total"
    );
    assert_eq!(
        summary_line(
            "uploaded",
            22,
            ("failed", 1),
            140_200_000,
            &summary(140_200_000, 11_400)
        ),
        "22 files uploaded · 1 failed · 140.2 MB · avg 98.4 Mbps · 11.4s total"
    );
    // The name never touches the size: a 96-character name, an 8-column size.
    let line = push_file_line(GUUS, TOTAL, TOTAL, 440_673, 120);
    assert!(line.contains(".mkv  17.48 GB"), "{line}");
}

#[test]
fn verbose_summary_detail() {
    let mut s = summary(TOTAL, 440_673);
    s.chunks_done = 32;
    s.chunks_already_stored = 12;
    s.direct = 0;
    s.api = 20;
    assert_eq!(
        verbose_detail(&s),
        "    32 chunks (12 already on the server) · 1 at a time · direct 0 · api 20 · 0 retries · pool Germany"
    );
    s.peak = 4;
    assert!(verbose_detail(&s).contains(" · up to 4 at once · "));
    s.peak = 0;
    assert!(!verbose_detail(&s).contains("at once") && !verbose_detail(&s).contains("at a time"));
    s.chunks_done = 1;
    s.chunks_already_stored = 0;
    s.retries = 1;
    assert!(verbose_detail(&s).starts_with("    1 chunk · direct 0 · api 20 · 1 retry"));
    let mut nothing = summary(0, 5000);
    nothing.bytes_transferred = 0;
    nothing.avg_mbps = None;
    assert!(summary_line("uploaded", 0, ("failed", 0), 0, &nothing).contains("avg —"));
}

#[test]
fn chunk_lines() {
    let record = ChunkRecord {
        file_name: GUUS.into(),
        index: 65,
        chunk_count: 131,
        bytes: 134_217_756,
        route: Route::Direct,
        held: Held::Transferred,
        retries: 0,
        duration_ms: 3400,
    };
    assert_eq!(
        chunk_line(&record, false, 100),
        "    chunk 66/131 · 134.2 MB · direct · 3.4s · 315.8 Mbps · 0 retries"
    );
    let small = ChunkRecord {
        file_name: "interview-03.m4a".into(),
        index: 1,
        chunk_count: 3,
        bytes: 4_200_000,
        route: Route::Api,
        held: Held::Transferred,
        retries: 1,
        duration_ms: 300,
    };
    assert_eq!(
        chunk_line(&small, true, 100),
        "    interview-03.m4a · chunk 2/3 · 4.2 MB · api · 0.3s · 112.0 Mbps · 1 retry"
    );
    let held = ChunkRecord {
        index: 11,
        held: Held::AlreadyStored,
        ..record
    };
    assert_eq!(
        chunk_line(&held, false, 100),
        "    chunk 12/131 · 134.2 MB · already on the server"
    );
    let one = Tally {
        bytes: 1,
        chunks: 1,
        already_stored: 0,
    };
    assert_eq!(
        rollback_line("movie.mkv", one, Direction::Upload),
        "    movie.mkv: 1 chunk counted again after the upload restarted"
    );
}

#[test]
fn run_line_survives_tiny_widths() {
    let s = snap(DONE, TOTAL);
    for columns in [0, 1, 5, 9] {
        let line = run_line(&s, columns);
        assert!(line.contains("50%"), "{columns} columns: {line:?}");
        let _ = stats_line(&s, columns);
        let _ = pool_line(&s, columns);
        let _ = file_line(&s.active[0], false, columns);
        let _ = push_file_line(GUUS, TOTAL, TOTAL, 1, columns);
    }
}

#[test]
fn names_never_carry_control_characters() {
    let name = "a\nb\x1b[31mc\t.txt";
    let clean = "a?b?[31mc?.txt";
    let file = FileView {
        name: name.into(),
        bytes_done: 1,
        bytes_total: 2,
        chunk_size: 4_194_304,
    };
    let record = ChunkRecord {
        file_name: name.into(),
        index: 0,
        chunk_count: 1,
        bytes: 1,
        route: Route::Api,
        held: Held::Transferred,
        retries: 0,
        duration_ms: 1,
    };
    for line in [
        file_line(&file, false, 100),
        chunk_line(&record, true, 100),
        push_file_line(name, 1, 1, 1, 100),
        rollback_line(name, Tally::default(), Direction::Upload),
    ] {
        assert!(line.contains(clean), "{line:?}");
        assert!(!line.chars().any(char::is_control), "{line:?}");
    }
}

/// Guus (2026-11-06): the pool reads as its country; a country the client did not receive is not shown, never
/// guessed; two pools in one country read as one; the server's text is cleaned like a name (no terminal codes).
#[test]
fn pool_shows_only_a_received_country_cleaned() {
    let label = |id: Option<&str>, country: Option<&str>| PoolLabel {
        id: id.map(|i| uuid::Uuid::parse_str(i).unwrap()),
        country: country.map(str::to_string),
        country_code: None,
    };
    let mut s = snap(DONE, TOTAL);
    s.pools = vec![];
    assert_eq!(
        pool_line(&s, 100),
        "    direct 66 · api 0 · 0 retries · chunk 134.2 MB",
        "not known yet"
    );
    s.pools = vec![PoolCount {
        label: label(Some("3e15382b-0000-4000-8000-000000000001"), None),
        chunks: 3,
    }];
    assert_eq!(
        pool_line(&s, 100),
        "    direct 66 · api 0 · 0 retries · chunk 134.2 MB",
        "an id without a country shows nothing"
    );
    s.pools = vec![
        PoolCount {
            label: label(Some("3e15382b-0000-4000-8000-000000000001"), Some("Germany")),
            chunks: 3,
        },
        PoolCount {
            label: label(Some("4f26493c-0000-4000-8000-000000000002"), Some("Germany")),
            chunks: 2,
        },
        PoolCount {
            label: label(None, Some("Finland")),
            chunks: 1,
        },
    ];
    assert!(
        pool_line(&s, 100).starts_with("    pools Germany, Finland · direct 66"),
        "{}",
        pool_line(&s, 100)
    );
    s.pools = vec![PoolCount {
        label: label(None, Some("Ger\u{1b}[2J\nmany")),
        chunks: 1,
    }];
    let line = pool_line(&s, 100);
    assert!(!line.chars().any(char::is_control), "{line:?}");
    assert!(line.starts_with("    pool Ger?[2J?many · "), "{line:?}");
    let mut sum = summary(TOTAL, 440_673);
    sum.pools = s.pools.clone();
    assert!(!verbose_detail(&sum).chars().any(char::is_control));
}
