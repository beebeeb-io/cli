use colored::Colorize;
use std::time::Instant;

use crate::{api::ApiClient, colors, ui};

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

pub async fn run() -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    if ui::is_json() {
        return run_json(&api).await;
    }

    let w = 58;
    println!("{}", ui::box_header("SPEEDTEST", w));

    // Get user's preferred region
    let region_info = api.get_my_region().await.ok();
    let region = region_info
        .as_ref()
        .and_then(|r| r.get("preferred_region"))
        .and_then(|v| v.as_str())
        .map(|s| capitalise(s))
        .unwrap_or_else(|| "Europe".to_string());
    println!(
        "{}",
        ui::box_line(&format!("{:<12}{}", "region".custom_color(colors::INK_DIM), region,), w,)
    );
    println!(
        "{}",
        ui::box_line(
            &format!("{:<12}{}", "endpoint".custom_color(colors::INK_DIM), "api.beebeeb.io"),
            w,
        )
    );
    println!("{}", ui::box_footer(w));
    println!();

    // -- Phase 1: Latency --
    println!("  {}", "NETWORK".custom_color(colors::AMBER));
    let mut latencies = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let _ = api.ping_health().await;
        latencies.push(start.elapsed().as_millis() as u64);
    }
    let min_lat = *latencies.iter().min().unwrap_or(&0);
    let max_lat = *latencies.iter().max().unwrap_or(&0);
    let avg_lat = latencies.iter().sum::<u64>() / latencies.len().max(1) as u64;
    println!(
        "  {:<12}{} {}",
        "latency".custom_color(colors::INK_DIM),
        format!("{}ms", avg_lat).custom_color(colors::GREEN_OK),
        format!("avg ({} / {} / {} min/avg/max)", min_lat, avg_lat, max_lat).custom_color(colors::INK_DIM),
    );

    // -- Phase 2: Upload --
    let blob = vec![42u8; 10_000_000]; // 10 MB
    let mut up_speeds = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        match api.speedtest_upload(&blob).await {
            Ok(()) => {
                let elapsed = start.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    up_speeds.push(blob.len() as f64 / elapsed);
                }
            }
            Err(e) => {
                println!(
                    "  {:<12}{}",
                    "upload".custom_color(colors::INK_DIM),
                    format!("endpoint unavailable: {e}").custom_color(colors::RED_ERR),
                );
                break;
            }
        }
    }
    let avg_up = if up_speeds.is_empty() {
        0.0
    } else {
        up_speeds.iter().sum::<f64>() / up_speeds.len() as f64
    };
    if !up_speeds.is_empty() {
        println!(
            "  {:<12}{}  {}",
            "upload".custom_color(colors::INK_DIM),
            ui::human_speed_network(avg_up).custom_color(colors::GREEN_OK).bold(),
            ui::speed_bar(avg_up * 8.0, 1_000_000_000.0, 30),
        );
    }

    // -- Phase 3: Download --
    let mut dl_speeds = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        match api.speedtest_download(10_000_000).await {
            Ok(data) => {
                let elapsed = start.elapsed().as_secs_f64();
                if elapsed > 0.0 {
                    dl_speeds.push(data.len() as f64 / elapsed);
                }
            }
            Err(e) => {
                println!(
                    "  {:<12}{}",
                    "download".custom_color(colors::INK_DIM),
                    format!("endpoint unavailable: {e}").custom_color(colors::RED_ERR),
                );
                break;
            }
        }
    }
    let avg_dl = if dl_speeds.is_empty() {
        0.0
    } else {
        dl_speeds.iter().sum::<f64>() / dl_speeds.len() as f64
    };
    if !dl_speeds.is_empty() {
        println!(
            "  {:<12}{}  {}",
            "download".custom_color(colors::INK_DIM),
            ui::human_speed_network(avg_dl).custom_color(colors::GREEN_OK).bold(),
            ui::speed_bar(avg_dl * 8.0, 1_000_000_000.0, 38),
        );
    }
    if !up_speeds.is_empty() || !dl_speeds.is_empty() {
        println!(
            "  {}",
            "             tested with 10 MB blob \u{00D7} 3 runs".custom_color(colors::INK_DIM),
        );
    }
    println!();

    // -- Phase 4: Crypto --
    println!("  {}", "CRYPTO".custom_color(colors::AMBER));
    let mk = crate::commands::push::load_master_key()?;
    let fk = beebeeb_core::kdf::derive_file_key(&mk, b"speedtest-bench");

    let bench_size = 64_000_000usize; // 64 MB
    let bench_buf = vec![42u8; bench_size];

    let start = Instant::now();
    let enc_blob = beebeeb_core::encrypt::encrypt_chunk(&fk, &bench_buf).map_err(|e| format!("encrypt bench: {e}"))?;
    let enc_elapsed = start.elapsed().as_secs_f64();
    let enc_speed = bench_size as f64 / enc_elapsed;

    let start = Instant::now();
    let _ = beebeeb_core::encrypt::decrypt_chunk(&fk, &enc_blob).map_err(|e| format!("decrypt bench: {e}"))?;
    let dec_elapsed = start.elapsed().as_secs_f64();
    let dec_speed = bench_size as f64 / dec_elapsed;

    // Detect hw accel hint
    let hw_hint = if cfg!(target_arch = "aarch64") && cfg!(target_os = "macos") {
        "Apple Silicon hw accel"
    } else if cfg!(target_arch = "x86_64") {
        "AES-NI"
    } else {
        "software"
    };

    println!(
        "  {:<12}{}  {}",
        "encrypt".custom_color(colors::INK_DIM),
        ui::human_speed(enc_speed).custom_color(colors::GREEN_OK).bold(),
        format!("AES-256-GCM \u{00B7} {hw_hint}").custom_color(colors::INK_DIM),
    );
    println!(
        "  {:<12}{}  {}",
        "decrypt".custom_color(colors::INK_DIM),
        ui::human_speed(dec_speed).custom_color(colors::GREEN_OK).bold(),
        format!("AES-256-GCM \u{00B7} {hw_hint}").custom_color(colors::INK_DIM),
    );

    let start = Instant::now();
    let _ = beebeeb_core::kdf::derive_master_key("speedtest-bench-password", b"speedtest-salt16");
    let kd_elapsed = start.elapsed();
    println!(
        "  {:<12}{}  {}",
        "key derive".custom_color(colors::INK_DIM),
        format!("{}ms", kd_elapsed.as_millis()).custom_color(colors::PATH),
        "Argon2id".custom_color(colors::INK_DIM),
    );
    println!();

    // -- Phase 5: Effective throughput --
    println!("  {}", "EFFECTIVE THROUGHPUT".custom_color(colors::AMBER),);
    let eff_push = if avg_up > 0.0 { avg_up.min(enc_speed) } else { enc_speed };
    let eff_pull = if avg_dl > 0.0 { avg_dl.min(dec_speed) } else { dec_speed };
    println!(
        "  {:<12}{}  {}",
        "push".custom_color(colors::INK_DIM),
        ui::human_speed(eff_push).custom_color(colors::AMBER).bold(),
        "(encrypt + upload)".custom_color(colors::INK_DIM),
    );
    println!(
        "  {:<12}{}  {}",
        "pull".custom_color(colors::INK_DIM),
        ui::human_speed(eff_pull).custom_color(colors::AMBER).bold(),
        "(download + decrypt)".custom_color(colors::INK_DIM),
    );
    println!();

    // -- Verdict --
    let eff_mbps = eff_push * 8.0 / 1_000_000.0;
    let (verdict, color) = ui::speed_verdict(eff_mbps);
    let time_up = 1_000_000_000.0 / eff_push;
    let time_dl = 1_000_000_000.0 / eff_pull;
    println!(
        "  verdict: {} \u{2014} 1 GB takes ~{:.0}s to upload, ~{:.0}s to download",
        verdict.custom_color(color).bold(),
        time_up,
        time_dl,
    );

    let bottleneck = if avg_up > 0.0 && enc_speed > avg_up * 2.0 {
        format!("network upload (crypto is {:.0}\u{00D7} faster)", enc_speed / avg_up,)
    } else if avg_up > 0.0 && avg_up > enc_speed * 2.0 {
        format!("encryption (network is {:.0}\u{00D7} faster)", avg_up / enc_speed,)
    } else if avg_up > 0.0 {
        "balanced (network \u{2248} crypto)".to_string()
    } else {
        "crypto only (speedtest endpoint unavailable)".to_string()
    };
    println!("  bottleneck: {}", bottleneck.custom_color(colors::INK_DIM),);

    Ok(())
}

async fn run_json(api: &ApiClient) -> Result<(), String> {
    // Region
    let region_info = api.get_my_region().await.ok();
    let region = region_info
        .as_ref()
        .and_then(|r| r.get("preferred_region"))
        .and_then(|v| v.as_str())
        .map(|s| capitalise(s))
        .unwrap_or_else(|| "Europe".to_string());

    // Latency
    let mut latencies = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let _ = api.ping_health().await;
        latencies.push(start.elapsed().as_millis() as u64);
    }
    let min_lat = *latencies.iter().min().unwrap_or(&0);
    let max_lat = *latencies.iter().max().unwrap_or(&0);
    let avg_lat = latencies.iter().sum::<u64>() / latencies.len().max(1) as u64;

    // Upload
    let blob = vec![42u8; 10_000_000];
    let mut up_speeds = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        if api.speedtest_upload(&blob).await.is_ok() {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                up_speeds.push(blob.len() as f64 / elapsed);
            }
        }
    }
    let avg_up = if up_speeds.is_empty() {
        0.0
    } else {
        up_speeds.iter().sum::<f64>() / up_speeds.len() as f64
    };

    // Download
    let mut dl_speeds = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        if let Ok(data) = api.speedtest_download(10_000_000).await {
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed > 0.0 {
                dl_speeds.push(data.len() as f64 / elapsed);
            }
        }
    }
    let avg_dl = if dl_speeds.is_empty() {
        0.0
    } else {
        dl_speeds.iter().sum::<f64>() / dl_speeds.len() as f64
    };

    // Crypto
    let mk = crate::commands::push::load_master_key()?;
    let fk = beebeeb_core::kdf::derive_file_key(&mk, b"speedtest-bench");

    let bench_size = 64_000_000usize;
    let bench_buf = vec![42u8; bench_size];

    let start = Instant::now();
    let enc_blob = beebeeb_core::encrypt::encrypt_chunk(&fk, &bench_buf).map_err(|e| format!("encrypt bench: {e}"))?;
    let enc_elapsed = start.elapsed().as_secs_f64();
    let enc_speed = bench_size as f64 / enc_elapsed;

    let start = Instant::now();
    let _ = beebeeb_core::encrypt::decrypt_chunk(&fk, &enc_blob).map_err(|e| format!("decrypt bench: {e}"))?;
    let dec_elapsed = start.elapsed().as_secs_f64();
    let dec_speed = bench_size as f64 / dec_elapsed;

    let start = Instant::now();
    let _ = beebeeb_core::kdf::derive_master_key("speedtest-bench-password", b"speedtest-salt16");
    let kd_elapsed = start.elapsed();

    // Effective throughput
    let eff_push = if avg_up > 0.0 { avg_up.min(enc_speed) } else { enc_speed };
    let eff_pull = if avg_dl > 0.0 { avg_dl.min(dec_speed) } else { dec_speed };
    let eff_mbps = eff_push / 1_000_000.0;
    let (verdict, _) = ui::speed_verdict(eff_mbps);

    let json = serde_json::json!({
        "region": region,
        "latency": {
            "min_ms": min_lat,
            "avg_ms": avg_lat,
            "max_ms": max_lat,
            "samples": latencies,
        },
        "upload_bps": avg_up as u64,
        "download_bps": avg_dl as u64,
        "crypto": {
            "encrypt_bps": enc_speed as u64,
            "decrypt_bps": dec_speed as u64,
            "key_derive_ms": kd_elapsed.as_millis() as u64,
        },
        "effective": {
            "push_bps": eff_push as u64,
            "pull_bps": eff_pull as u64,
        },
        "verdict": verdict,
    });
    println!("{}", serde_json::to_string_pretty(&json).unwrap());

    Ok(())
}
