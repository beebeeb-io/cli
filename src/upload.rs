//! Streaming, constant-memory chunk upload — the one upload driver shared by
//! `bb sync` (parallel, file-level) and `bb push` (sequential).
//!
//! ## Why this exists
//!
//! The previous path (`std::fs::read` the whole file → encrypt *every* chunk
//! into a `Vec` → sequential PUT) peaked at ~2× file size in RAM per file, and
//! under `bb sync`'s `buffer_unordered(concurrency)` that became ~`8×` the
//! largest file. It also showed no progress. This module replaces it with a
//! look-ahead pipeline whose peak memory is bounded by the chunk size, not the
//! file size.
//!
//! ## Pipeline
//!
//! ```text
//!   producer (spawn_blocking, owns ChunkEncryptor)
//!     → next_chunk()  [AES off the async reactor]
//!     → bounded mpsc (cap 1) ── backpressure ──>  consumer (async)
//!                                                   → PUT /files/{id}/chunks/{i}
//!                                                   → progress.chunk_confirmed()
//! ```
//!
//! The bounded channel gives natural look-ahead: while the consumer PUTs chunk
//! N, the producer has already encrypted chunk N+1 (buffered) and is encrypting
//! N+2. Peak ≈ `~4 × chunk_size` per file (read buffer + channel-held ciphertext
//! + in-flight blocking chunk + PUT body), **never** file-size-proportional.
//!
//! ## Key handling
//!
//! The master key is used only synchronously on the async side (to encrypt the
//! name, build the encryptor, and derive the thumbnail key). The
//! `FileKey`-owning [`ChunkEncryptor`](beebeeb_core::chunk_stream::ChunkEncryptor)
//! is the only thing moved into the blocking closure — raw `MasterKey` bytes are
//! never copied (it is held behind an `Arc`).
//!
//! ## Cancellation
//!
//! `spawn_blocking` cannot be aborted mid-chunk, so a shared `AtomicBool`
//! shutdown flag is checked at the top of every producer iteration and at the
//! top of every consumer iteration. On any early exit the consumer **always**
//! `rx.close()`s before awaiting the producer; closing the receiver wakes a
//! producer parked on the cap-1 channel, so the pipeline can never deadlock.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;

use beebeeb_core::chunk_stream::{ChunkEncryptor, EncryptedChunk};
use beebeeb_core::kdf::MasterKey;
use beebeeb_types::ChunkProfile;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use uuid::Uuid;

use crate::api::ApiClient;

/// Sentinel error returned when an upload is cancelled (Ctrl-C / shutdown
/// flag). Callers compare against this to count "remaining" rather than
/// reporting a failure. Mirrors the existing `__rate_limited__` convention.
pub const INTERRUPTED: &str = "__interrupted__";

/// Look-ahead channel depth. `1` keeps peak memory tight while still
/// overlapping "encrypt N+1" with "PUT N"; bump to `2` for a touch more
/// overlap at the cost of one extra chunk resident.
const CHANNEL_CAP: usize = 1;

/// Thumbnails are best-effort. To honour the constant-memory promise we never
/// read more than this much of a file just to make one.
const MAX_THUMBNAIL_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Per-chunk AEAD overhead on the wire: `nonce(12) + tag(16)`.
const CHUNK_OVERHEAD: u64 = 28;

// ── Public surface ──────────────────────────────────────────────────────────

/// Everything needed to upload one file.
pub struct UploadSpec {
    /// Local path to read from.
    pub path: PathBuf,
    /// Display + name-encryption filename (may be suffixed for keep-both).
    pub file_name: String,
    /// The file UUID. A fresh random id for a new file, or the existing id for
    /// a server-side version replace (`bb push --replace`).
    pub file_id: Uuid,
    /// Destination folder, or `None` for the vault root.
    pub parent_id: Option<Uuid>,
    /// Number of files uploaded in parallel by the caller (sync `--concurrency`;
    /// `1` for `bb push`/single-file paths). Threaded into the concurrency-aware
    /// chunk plan so parallel uploads emit smaller chunks to stay in the memory
    /// budget.
    pub concurrency: u32,
    /// Shared cancellation flag (set by the Ctrl-C handler).
    pub shutdown: Arc<AtomicBool>,
}

/// Result of a completed upload.
#[derive(Debug)]
pub struct UploadOutcome {
    /// Server-confirmed file id.
    pub server_id: Uuid,
    /// Plaintext size (bytes).
    pub plaintext_bytes: u64,
    /// Total ciphertext uploaded (`plaintext + 28 * chunk_count`).
    pub ciphertext_bytes: u64,
}

/// Progress sink. One instance per sync/push run; [`begin_file`] is called once
/// per file and returns a per-file handle.
///
/// [`begin_file`]: ChunkProgress::begin_file
pub trait ChunkProgress: Send + Sync {
    /// Start tracking one file. `expected_ciphertext` is the bar length.
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress>;
    /// Tear down any shared UI (called once after the upload phase). No-op by
    /// default.
    fn finish_all(&self) {}
}

/// Per-file progress handle. `Sync` so a `&dyn FileProgress` can be held across
/// `.await` in a `Send` future (the watch loops upload from a spawned task).
pub trait FileProgress: Send + Sync {
    /// One server-confirmed chunk (called only after a 200 from the server, so
    /// progress reflects honest, on-the-wire bytes — never queued bytes).
    fn chunk_confirmed(&self, ciphertext_bytes: u64);
    /// File finished; clears the transient per-file bar and, on success,
    /// advances the overall file counter.
    fn finish(self: Box<Self>, success: bool);
}

// ── No-op progress (used for --json / --quiet / non-TTY and the watch loops) ──

/// Progress sink that does nothing — the caller keeps its plain `println!`
/// status lines instead.
pub struct NoopProgress;

impl ChunkProgress for NoopProgress {
    fn begin_file(&self, _: &str, _: u64) -> Box<dyn FileProgress> {
        Box::new(NoopFileProgress)
    }
}

struct NoopFileProgress;

impl FileProgress for NoopFileProgress {
    fn chunk_confirmed(&self, _: u64) {}
    fn finish(self: Box<Self>, _: bool) {}
}

// ── indicatif progress (rich + TTY only) ──────────────────────────────────────

/// Amber-accented multi-bar progress: one persistent overall bar plus a pool of
/// transient per-file bars (at most `concurrency` alive at once, since
/// `bb sync` only runs that many uploads concurrently).
///
/// Brand: the bar fill uses xterm colour 214 (the closest 256-colour match to
/// brand amber `#f5b800`); everything else is dim. No emojis.
pub struct BarProgress {
    mp: MultiProgress,
    overall: ProgressBar,
    files_done: Arc<AtomicU64>,
    files_total: u64,
}

impl BarProgress {
    /// Build the multi-bar UI. `files_total` and `total_ciphertext` size the
    /// overall bar (files counter + byte gauge respectively).
    pub fn new(files_total: u64, total_ciphertext: u64) -> Self {
        let mp = MultiProgress::new();
        let overall = mp.add(ProgressBar::new(total_ciphertext.max(1)));
        overall.set_style(
            ProgressStyle::with_template(
                "  {prefix:.dim} {bar:24.214/238} {bytes}/{total_bytes} · {bytes_per_sec} · ETA {eta}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("━━─"),
        );
        overall.set_prefix(format!("0/{files_total} files"));
        // Steady tick so speed/ETA refresh between confirmations.
        overall.enable_steady_tick(Duration::from_millis(120));
        Self {
            mp,
            overall,
            files_done: Arc::new(AtomicU64::new(0)),
            files_total,
        }
    }
}

impl ChunkProgress for BarProgress {
    fn begin_file(&self, file_name: &str, expected_ciphertext: u64) -> Box<dyn FileProgress> {
        let bar = self.mp.add(ProgressBar::new(expected_ciphertext.max(1)));
        bar.set_style(
            ProgressStyle::with_template("    {msg:.dim} {bar:20.214/238} {bytes}/{total_bytes}")
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("━━─"),
        );
        bar.set_message(display_name(file_name));
        Box::new(BarFileProgress {
            bar,
            overall: self.overall.clone(),
            files_done: Arc::clone(&self.files_done),
            files_total: self.files_total,
        })
    }

    fn finish_all(&self) {
        self.overall.finish_and_clear();
        let _ = self.mp.clear();
    }
}

struct BarFileProgress {
    bar: ProgressBar,
    overall: ProgressBar,
    files_done: Arc<AtomicU64>,
    files_total: u64,
}

impl FileProgress for BarFileProgress {
    fn chunk_confirmed(&self, ciphertext_bytes: u64) {
        self.bar.inc(ciphertext_bytes);
        self.overall.inc(ciphertext_bytes);
    }

    fn finish(self: Box<Self>, success: bool) {
        self.bar.finish_and_clear();
        if success {
            let done = self.files_done.fetch_add(1, Ordering::Relaxed) + 1;
            self.overall.set_prefix(format!("{done}/{} files", self.files_total));
        }
    }
}

/// Truncate a long filename for a progress label, keeping the (informative)
/// tail.
fn display_name(name: &str) -> String {
    const MAX: usize = 40;
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let tail: String = name
        .chars()
        .rev()
        .take(MAX - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

// ── The streaming upload driver ───────────────────────────────────────────────

/// Encrypt and upload one file with constant memory and honest progress.
///
/// Shared by `bb sync` (called inside the `buffer_unordered` closure, so files
/// run in parallel) and `bb push` (called sequentially). The master key is held
/// behind an `Arc` and used only synchronously here; only the `FileKey`-owning
/// encryptor crosses into the blocking thread.
pub async fn stream_encrypt_upload(
    api: &ApiClient,
    master_key: Arc<MasterKey>,
    spec: UploadSpec,
    progress: &dyn ChunkProgress,
) -> Result<UploadOutcome, String> {
    // Cheap cancellation path for files that never started.
    if spec.shutdown.load(Ordering::Relaxed) {
        return Err(INTERRUPTED.to_string());
    }

    // 1. Stat for size + mtime — no full read.
    let meta = std::fs::metadata(&spec.path).map_err(|e| format!("stat {}: {e}", spec.path.display()))?;
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    // 1b. Resume decision. A prior run that was interrupted leaves a sidecar
    //     entry (file_id) for this path; reuse it — and skip already-uploaded
    //     chunks — only if the file is unchanged AND the server still has the
    //     upload in progress. Otherwise mint a fresh id (`spec.file_id`). The
    //     server's init rejects a re-init of an existing id, so a resumed upload
    //     must skip `upload_init` and go straight to status → missing chunks →
    //     complete (the same flow web/mobile use).
    let resume_candidate = crate::resume::resumable_file_id(&spec.path, size, mtime_ns);
    let mut present: HashSet<u32> = HashSet::new();
    let resuming = match resume_candidate {
        Some(fid) => match api.upload_status(&fid.to_string()).await {
            Ok(set) => {
                present = set;
                true
            }
            Err(_) => {
                // Server no longer has it in progress (completed/trashed/gone).
                crate::resume::clear(&spec.path);
                false
            }
        },
        None => false,
    };
    let file_id = match (resuming, resume_candidate) {
        (true, Some(fid)) => fid,
        _ => spec.file_id,
    };
    let file_id_str = file_id.to_string();

    // 2. Encrypt the name + MIME envelope (master key used synchronously).
    let mime = beebeeb_core::media::guess_mime_type(&spec.file_name);
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(&master_key, &file_id_str, &spec.file_name, mime)
        .map_err(|e| format!("encrypt name: {e}"))?;
    let is_media = beebeeb_core::media::is_media(mime);

    // 3. Open the file and build the streaming encryptor (derives the file key
    //    once; the encryptor owns it and is `Send`). The CLI uses the Cli
    //    profile with a concurrency-aware chunk size: parallel `bb sync` uploads
    //    emit smaller chunks (memory budget), while `bb push` (concurrency 1)
    //    gets the full Cli 128 MiB cap. The server infers + stores the real
    //    chunk size at complete, so this is purely a client-side choice.
    let chunk_size = beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, spec.concurrency.max(1))
        .chunk_size_bytes;
    let file = std::fs::File::open(&spec.path).map_err(|e| format!("open {}: {e}", spec.path.display()))?;
    let encryptor = ChunkEncryptor::from_reader_with_chunk_size(&master_key, &file_id_str, size, chunk_size, file)
        .map_err(|e| format!("init encryptor for {}: {e}", spec.file_name))?;
    let chunk_count = encryptor.chunk_plan().chunk_count as u32;
    let expected_total = encryptor.expected_total_ciphertext();

    // 4. TOCTOU: re-stat just before init. `finish()` catches a file that
    //    SHRANK; a same-size grow can slip past and is reconciled by
    //    content-hash on the next sync run (documented on
    //    `ChunkEncryptor::finish`). A changed *size* here means the plan is
    //    stale, so bail and let the next run pick it up.
    if let Ok(m2) = std::fs::metadata(&spec.path) {
        if m2.len() != size {
            return Err(format!(
                "{} changed size during scan ({size} → {}); will retry next run",
                spec.file_name,
                m2.len()
            ));
        }
    }

    // 4b. Record this upload as resumable BEFORE any chunk goes out, so an
    //     interrupt mid-stream leaves a record the next run can pick up.
    crate::resume::record(&spec.path, file_id, size, mtime_ns);

    // 5. init with the client-computed total (= size + 28·chunk_count). The
    //    server recomputes the real size from the summed chunks on complete, so
    //    this is hygiene, not a contract. On resume the server record already
    //    exists (re-init would conflict), so skip straight to the pipeline.
    let server_id = if resuming {
        file_id_str.clone()
    } else {
        let init_resp = api
            .upload_init(
                Some(file_id),
                &name_encrypted,
                spec.parent_id,
                expected_total as i64,
                chunk_count as i32,
                is_media,
            )
            .await?;
        init_resp
            .get("file_id")
            .or_else(|| init_resp.get("id"))
            .and_then(|v| v.as_str())
            .ok_or("server response missing file_id")?
            .to_string()
    };

    // 6. Run the look-ahead pipeline, skipping any chunk indices the server
    //    already has from a prior interrupted run.
    let file_prog = progress.begin_file(&spec.file_name, expected_total);
    let result = run_pipeline(
        api,
        &server_id,
        encryptor,
        chunk_count,
        expected_total,
        &present,
        &spec.shutdown,
        file_prog.as_ref(),
    )
    .await;

    match &result {
        Ok(()) => file_prog.finish(true),
        Err(_) => file_prog.finish(false),
    }
    result?;

    // 7. Finalise the version.
    api.upload_complete(&server_id).await?;

    // 7b. Upload done — drop the resume record so a future upload of this path
    //     (e.g. a changed version) starts fresh rather than resuming this id.
    crate::resume::clear(&spec.path);

    // 8. Thumbnails: image/* only, one bounded extra read, best-effort.
    maybe_upload_thumbnail(api, &master_key, &file_id_str, &server_id, &spec.path, mime, size).await;

    let parsed: Uuid = server_id.parse().map_err(|e| format!("invalid server file id: {e}"))?;
    Ok(UploadOutcome {
        server_id: parsed,
        plaintext_bytes: size,
        ciphertext_bytes: expected_total,
    })
}

/// Distinct producer failure causes, kept separate so the driver can surface an
/// honest reason rather than a generic "upload failed".
enum ProducerErr {
    /// A `CoreError` from `next_chunk` (read/encrypt).
    Core(String),
    /// The `finish()` integrity guard tripped — the source shrank mid-stream.
    Finish(String),
    /// The shutdown flag tripped at the top of an iteration.
    Cancelled,
    /// The receiver closed — the consumer hit its own error first (which wins).
    ChannelClosed,
}

/// The producer/consumer core. Returns `Ok(())` only when every planned chunk
/// was emitted, server-confirmed, and the byte totals reconcile.
#[allow(clippy::too_many_arguments)]
async fn run_pipeline(
    api: &ApiClient,
    server_id: &str,
    encryptor: ChunkEncryptor,
    chunk_count: u32,
    expected_total: u64,
    present: &HashSet<u32>,
    shutdown: &Arc<AtomicBool>,
    file_prog: &dyn FileProgress,
) -> Result<(), String> {
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel::<EncryptedChunk>(CHANNEL_CAP);
    let shutdown_p = Arc::clone(shutdown);

    // Producer: ONE blocking task owns the encryptor for the whole file. AES
    // runs here, off the async reactor. `blocking_send` provides backpressure.
    let producer = tokio::task::spawn_blocking(move || -> Result<(), ProducerErr> {
        let mut enc = encryptor;
        loop {
            // spawn_blocking can't be aborted mid-chunk, so check at the top of
            // each iteration.
            if shutdown_p.load(Ordering::Relaxed) {
                return Err(ProducerErr::Cancelled);
            }
            match enc.next_chunk() {
                Ok(Some(chunk)) => {
                    if tx.blocking_send(chunk).is_err() {
                        // Receiver gone → consumer failed or we were cancelled;
                        // let the consumer's cause win.
                        return Err(ProducerErr::ChannelClosed);
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(ProducerErr::Core(format!("encrypt chunk: {e}"))),
            }
        }
        // Integrity guard: detects a source that shrank mid-stream.
        enc.finish().map(|_| ()).map_err(|e| ProducerErr::Finish(e.to_string()))
    });

    // Consumer: PUT each chunk as it arrives; advance progress only on 200.
    //
    // NOTE: this loop uses `break` (never `?`) so that the cleanup below —
    // `rx.close()` before awaiting the producer — ALWAYS runs. Closing the
    // receiver wakes a producer parked on the cap-1 channel, so there is no
    // deadlock; and if this function unwinds, dropping `rx` closes the channel
    // for the same reason.
    let mut confirmed_bytes: u64 = 0;
    let mut confirmed_chunks: u32 = 0;
    let mut consumer_err: Option<String> = None;
    while let Some(chunk) = rx.recv().await {
        if shutdown.load(Ordering::Relaxed) {
            consumer_err = Some(INTERRUPTED.to_string());
            break;
        }
        let len = chunk.data.len() as u64;
        let index = chunk.index;
        // Resume: a chunk the server already has is counted as confirmed
        // (advancing the byte totals + progress) without re-PUTting it.
        if present.contains(&index) {
            confirmed_bytes += len;
            confirmed_chunks += 1;
            file_prog.chunk_confirmed(len);
            continue;
        }
        match api.upload_chunk(server_id, index, Bytes::from(chunk.data)).await {
            Ok(_) => {
                confirmed_bytes += len;
                confirmed_chunks += 1;
                file_prog.chunk_confirmed(len);
            }
            Err(e) => {
                consumer_err = Some(format!("chunk {index} upload: {e}"));
                break;
            }
        }
    }

    // ── Cleanup (always): unblock + reap the producer before returning. ──
    rx.close();
    let producer_join = producer.await;

    // A proximate consumer error (network / cancellation) is what the user
    // needs to see first.
    if let Some(e) = consumer_err {
        return Err(e);
    }
    match producer_join {
        Ok(Ok(())) => {}
        Ok(Err(ProducerErr::Cancelled)) => return Err(INTERRUPTED.to_string()),
        Ok(Err(ProducerErr::ChannelClosed)) => {
            return Err("internal: chunk channel closed before completion".to_string());
        }
        Ok(Err(ProducerErr::Core(e))) => return Err(e),
        Ok(Err(ProducerErr::Finish(e))) => return Err(format!("integrity check failed: {e}")),
        Err(join_err) => return Err(format!("encrypt task failed: {join_err}")),
    }

    // Final client-side guards (hygiene; the server recomputes the total).
    if confirmed_chunks != chunk_count {
        return Err(format!(
            "incomplete upload: {confirmed_chunks}/{chunk_count} chunks confirmed"
        ));
    }
    if confirmed_bytes != expected_total {
        return Err(format!(
            "ciphertext total mismatch: confirmed {confirmed_bytes}, expected {expected_total}"
        ));
    }
    Ok(())
}

/// Generate + encrypt + upload a thumbnail for image files, best-effort. Does
/// exactly one extra bounded read of the source; never holds key material in
/// the CLI beyond the derived per-file key.
async fn maybe_upload_thumbnail(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id_str: &str,
    server_id: &str,
    path: &Path,
    mime: Option<&str>,
    size: u64,
) {
    let is_image = mime.map(|m| m.starts_with("image/")).unwrap_or(false);
    if !is_image || size > MAX_THUMBNAIL_SOURCE_BYTES {
        return;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());

    if let Some(thumb) = crate::thumbnail::generate_from_file(&bytes, mime) {
        if let Ok(enc) = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, &thumb.data) {
            let _ = api.upload_thumbnail(server_id, enc).await;
        }
    }
    if let Some(large) = crate::thumbnail::generate_large_from_file(&bytes, mime) {
        if let Ok(enc) = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, &large.data) {
            let _ = api.upload_thumbnail_large(server_id, enc).await;
        }
    }
}

/// Expected ciphertext for a file of `size` under `ChunkProfile::Cli` at the
/// given upload `concurrency`: `size + 28 · chunk_count`. Used by callers to
/// size the overall progress bar without opening the file — matches the
/// concurrency-aware chunk plan the upload driver actually emits.
pub fn expected_ciphertext_for(size: u64, concurrency: u32) -> u64 {
    let plan = beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, concurrency.max(1));
    size + CHUNK_OVERHEAD * plan.chunk_count
}

#[cfg(test)]
mod rss_regression {
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

    /// Minimal mock of the V2 upload endpoints. The chunk handler drains the body
    /// as a stream and discards it, so the mock holds at most one frame — the
    /// measured peak reflects the CLIENT pipeline, not server-side buffering.
    async fn spawn_mock() -> String {
        let app = Router::new()
            .route(
                "/api/v1/files/upload/init",
                post(|| async { Json(json!({ "file_id": Uuid::new_v4().to_string() })) }),
            )
            .route(
                "/api/v1/files/:id/chunks/:idx",
                put(|_p: Path<(String, u32)>, body: Body| async move {
                    let mut stream = body.into_data_stream();
                    let mut n: usize = 0;
                    while let Some(frame) = stream.next().await {
                        n += frame.map(|b| b.len()).unwrap_or(0);
                    }
                    Json(json!({ "size": n }))
                }),
            )
            .route(
                "/api/v1/files/:id/upload/complete",
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
}
