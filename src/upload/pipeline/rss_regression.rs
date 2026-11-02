use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::body::Body;
use axum::extract::Path;
use axum::routing::{post, put};
use axum::{Json, Router};
use beebeeb_core::kdf::MasterKey;
use futures_util::StreamExt;
use serde_json::json;
use uuid::Uuid;

use super::{NoopProgress, UploadSpec, stream_encrypt_upload};
use crate::api::ApiClient;

/// Minimal mock of the V2 `/api/v1/uploads/*` endpoints. The chunk handler
/// drains the body as a stream and discards it, so the mock holds at most one
/// frame — the measured peak reflects the CLIENT pipeline, not server-side
/// buffering. `init` echoes back a session id (= file id here) plus the
/// client-sent chunk plan so the driver frames the upload from the response.
async fn spawn_mock() -> String {
    let app = Router::new()
        .route(
            "/api/v1/uploads/init",
            post(|Json(body): Json<serde_json::Value>| async move {
                let file_id = body
                    .get("file_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                Json(json!({
                    "upload_session_id": Uuid::new_v4().to_string(),
                    "file_id": file_id,
                    "chunk_size_bytes": body.get("chunk_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                    "chunk_count": body.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0),
                }))
            }),
        )
        .route(
            "/api/v1/uploads/:session/chunks/:idx",
            put(|_p: Path<(String, u32)>, body: Body| async move {
                let mut stream = body.into_data_stream();
                let mut n: usize = 0;
                while let Some(frame) = stream.next().await {
                    n += frame.map(|b| b.len()).unwrap_or(0);
                }
                Json(json!({ "index": 0, "size": n, "skipped": false }))
            }),
        )
        .route(
            "/api/v1/uploads/:session/complete",
            post(|_p: Path<String>| async {
                Json(json!({ "id": Uuid::new_v4().to_string(), "size_bytes": 0, "chunk_count": 0 }))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn write_temp_file(name: &str, size: u64) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("bb-rss-{}-{name}", std::process::id()));
    let mut f = std::fs::File::create(&path).unwrap();
    let buf = vec![0xABu8; 1024 * 1024]; // 1 MiB, reused
    let mut written = 0u64;
    while written < size {
        let take = ((size - written) as usize).min(buf.len());
        f.write_all(&buf[..take]).unwrap();
        written += take as u64;
    }
    f.flush().unwrap();
    path
}

/// Peak-heap regression guard (task 0666): a conc=4 upload of four 64 MiB
/// files (4 MiB chunks under the N=32 ladder) must keep peak heap BOUNDED —
/// roughly constant w.r.t. tree size and well under the 2 GiB budget —
/// proving the streaming pipeline never buffers whole files. A regression
/// (losing the streaming/`Bytes` reuse, or buffering a file) inflates the
/// peak and trips the bound. Runs the REAL `stream_encrypt_upload` against an
/// in-process mock so the measurement covers the actual path.
///
/// `#[ignore]`d because the tracking allocator is PROCESS-GLOBAL: under the
/// default parallel `cargo test`, other tests' concurrent allocations
/// pollute the peak (measured ~87 MiB in-suite vs ~48 MiB isolated). Run it
/// isolated for an accurate measurement:
///   `cargo test upload_peak_heap_is_bounded_at_conc4 -- --ignored --test-threads=1`
#[tokio::test]
#[ignore = "process-global peak-alloc measurement; run isolated: -- --ignored --test-threads=1"]
async fn upload_peak_heap_is_bounded_at_conc4() {
    const FILE_SIZE: u64 = 64 * 1024 * 1024;
    const CONC: u32 = 4;

    let base_url = spawn_mock().await;
    let api = ApiClient::new_for_test(base_url);
    let master_key = Arc::new(MasterKey::from_bytes([7u8; 32]));

    // Create temp files BEFORE measuring so their creation isn't counted.
    let paths: Vec<_> = (0..CONC)
        .map(|i| write_temp_file(&format!("{i}.bin"), FILE_SIZE))
        .collect();

    let baseline = crate::test_alloc::live();
    crate::test_alloc::reset_peak();

    let progress = NoopProgress;
    let futures = paths.iter().map(|path| {
        let spec = UploadSpec {
            path: path.clone(),
            file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
            file_id: Uuid::new_v4(),
            base_version_number: None,
            replace_pin: None,
            replace_file_key: None,
            parent_id: None,
            concurrency: CONC,
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        stream_encrypt_upload(&api, master_key.clone(), spec, &progress)
    });
    let results = futures_util::future::join_all(futures).await;

    let peak_increase = crate::test_alloc::peak().saturating_sub(baseline);

    for p in &paths {
        let _ = std::fs::remove_file(p);
    }

    let mib = peak_increase as f64 / (1024.0 * 1024.0);
    eprintln!(
        "[RSS] conc={CONC} x {} MiB files (4 MiB chunks): peak heap increase = {mib:.1} MiB",
        FILE_SIZE / (1024 * 1024),
    );

    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "upload {i} failed: {r:?}");
    }

    // CONC × FILE_SIZE = 256 MiB of plaintext crosses the pipeline; a
    // file-proportional regression (buffering a whole file — the old ~8×
    // path) would push peak toward/over that. MEASURED isolated steady
    // state: ~46–50 MiB (logged above) — peak ≈ 1/5th of the data in flight,
    // i.e. constant-memory streaming. Bound at 80 MiB: ~1.6× over the
    // measured peak, below the ~110 MiB a single buffered 64 MiB file would
    // cause and the 256 MiB whole-tree line, and far under the 2 GiB budget.
    // A multiplicative blowup (lost streaming, a channel buffering whole
    // files, lost Bytes reuse) trips it. (A subtle single-extra-chunk change
    // — e.g. CHANNEL_CAP 1→2, ~+16 MiB — is near this 4 MiB-chunk config's
    // resolution; catching that reliably would need larger chunks.)
    const BOUND: usize = 80 * 1024 * 1024;
    assert!(
        peak_increase < BOUND,
        "peak heap increase {mib:.1} MiB exceeded {} MiB — upload is not constant-memory",
        BOUND / (1024 * 1024)
    );
}
