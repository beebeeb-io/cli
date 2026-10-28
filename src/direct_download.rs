//! Direct S3 download driver (task 2173).
//!
//! When the server offers `object-download-v1`, `bb pull` and the sync daemon
//! fetch a file's encrypted chunks straight from object storage through
//! short-lived presigned links, and decrypt them exactly as the API path does.
//! The API only authorizes and signs.
//!
//! The rules (which links may be followed, when to renew, when to fall back)
//! live in `beebeeb_core::direct_transfer` and are shared by every client. This
//! module is the CLI's half: it performs the HTTP, keeps the planner's
//! bookkeeping, decrypts chunks IN ORDER through core's [`ChunkDecryptor`], and
//! writes through the same atomic temp file as the API path, published only
//! after every chunk authenticated and `finish()` passed.
//!
//! What it does not do: touch a file when it falls back. Until the first chunk
//! has been decrypted nothing exists on disk; after that, any fallback to the
//! API path deletes the partial temp file first, so two transports never write
//! into one output.
//!
//! No URL is ever printed. Storage errors are dropped at the HTTP boundary
//! (reqwest error text contains the URL), and the optional trace
//! (`BB_TRANSPORT_TRACE=1`, for support) prints only chunk numbers and statuses.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use beebeeb_core::chunk_stream::{ChunkDecryptor, ExpectedChunkFormat};
use beebeeb_core::direct_transfer as dt;
use beebeeb_core::kdf::MasterKey;
use bytes::Bytes;
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::api::{ApiClient, ChunkApiError, GrantReply, now_ms};
use crate::download::{AtomicFile, DownloadError, DownloadStats};
use crate::upload::{ChunkProgress, FileProgress};

/// Result of an attempt to download over the direct transport.
pub(crate) enum DirectResult {
    /// The file is complete and published at the output path.
    Done(DownloadStats),
    /// Direct transport was not used (or was abandoned before anything was
    /// published): download this file through the API path.
    UseApi,
}

/// Is `BB_TRANSPORT_TRACE` switched on? Set and not an explicit "off" (`0`, `false`, `off`, `no`,
/// or empty): a variable that is merely present must not mean "on" when it says 0.
fn trace_is_on(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off" | "no"
        ),
    }
}

/// Support aid: `BB_TRANSPORT_TRACE=1` prints one line per transport decision
/// to stderr. Never a URL, never a header.
fn trace(message: impl FnOnce() -> String) {
    if trace_is_on(std::env::var("BB_TRANSPORT_TRACE").ok().as_deref()) {
        eprintln!("bb: transport: {}", message());
    }
}

enum Event {
    Window(GrantReply),
    Storage {
        index: u32,
        /// `Err(status)`: the HTTP status when there was one (0: none).
        result: Result<Bytes, u16>,
    },
    Api {
        index: u32,
        result: Result<Vec<u8>, ChunkApiError>,
    },
}

type Task<'a> = Pin<Box<dyn Future<Output = Event> + Send + 'a>>;

/// Fetch one chunk from its presigned link: `200` and a `Content-Length` equal
/// to the grant's `length`, nothing else. The error carries at most a status.
async fn fetch_from_storage(client: &reqwest::Client, grant: dt::ChunkGrant) -> Result<Bytes, u16> {
    let resp = client.get(&grant.url).send().await.map_err(|_| 0u16)?;
    let status = resp.status().as_u16();
    if status != 200 || resp.content_length() != Some(grant.length) {
        return Err(status);
    }
    // The HTTP stack frames the body by that Content-Length: a short body is a
    // read error, a longer one never reaches us. Memory is bounded by the
    // grant's length, which the planner has already checked against the
    // manifest's geometry (at most core's MAX_CHUNK_SIZE + 28).
    resp.bytes().await.map_err(|_| 0u16)
}

/// The partly written output: exists only after the first chunk decrypted.
struct Output {
    writer: AtomicFile,
    progress: Box<dyn FileProgress>,
    plaintext_bytes: u64,
    encrypted_bytes: u64,
}

impl Output {
    /// Throw the partial output away (temp file removed, progress closed as failed).
    ///
    /// The bytes it already reported on the run-wide counter are taken back: the file starts over
    /// (the API stream) or fails, and either way they would otherwise be counted twice.
    fn discard(self, overall: &dyn ChunkProgress) {
        let mut writer = self.writer;
        writer.abort();
        self.progress.finish(false);
        overall.rollback(self.encrypted_bytes);
    }
}

fn discard(out: Option<Output>, overall: &dyn ChunkProgress) {
    if let Some(out) = out {
        out.discard(overall);
    }
}

/// Download `file_id` over the direct transport when the server offers it.
///
/// `Ok(UseApi)`: not used or abandoned; nothing is left on disk. `Err`: a
/// failure the API path would not fix (authorization refused, the file changed
/// while it was downloading, a chunk that never authenticated, a disk error);
/// nothing is published either.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_download(
    api: &ApiClient,
    master_key: &MasterKey,
    file_id: &str,
    pin: ExpectedChunkFormat,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
) -> Result<DirectResult, DownloadError> {
    try_download_inner(
        api,
        ChunkDecryptor::for_push(master_key, file_id, pin),
        pin,
        true,
        file_id,
        chunk_count,
        out_path,
        progress,
        display_name,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn try_download_with_file_key(
    api: &ApiClient,
    file_key: std::sync::Arc<beebeeb_core::kdf::FileKey>,
    file_id: &str,
    pin: ExpectedChunkFormat,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
) -> Result<DirectResult, DownloadError> {
    let decryptor = ChunkDecryptor::for_push_with_file_key(
        beebeeb_core::kdf::FileKey::from_bytes(*file_key.as_bytes()),
        file_id,
        pin,
    );
    try_download_inner(
        api,
        decryptor,
        pin,
        false,
        file_id,
        chunk_count,
        out_path,
        progress,
        display_name,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn try_download_inner(
    api: &ApiClient,
    mut decryptor: ChunkDecryptor,
    pin: ExpectedChunkFormat,
    allow_legacy: bool,
    file_id: &str,
    chunk_count: u32,
    out_path: &Path,
    progress: &dyn ChunkProgress,
    display_name: &str,
) -> Result<DirectResult, DownloadError> {
    let Some(discovery) = api.direct_download_offer().await else {
        return Ok(DirectResult::UseApi);
    };
    let Some(storage) = api.storage_client() else {
        return Ok(DirectResult::UseApi);
    };
    let config = dt::PlannerConfig::from_discovery(&discovery, rand::random());
    let mut planner = dt::DownloadPlanner::new(config, discovery, api.api_base(), file_id);
    let mut tasks: FuturesUnordered<Task<'_>> = FuturesUnordered::new();
    let mut ready: BTreeMap<u32, Bytes> = BTreeMap::new();
    let mut out: Option<Output> = None;
    let mut geometry_checked = false;
    // Chunks that came through the pinned API route instead of storage.
    let mut api_chunks: u32 = 0;
    let expected_chunks = chunk_count.max(1);

    loop {
        // Everything the planner has for us right now.
        let wake: Option<i64> = loop {
            match planner.poll(now_ms()) {
                dt::Poll::RequestWindow(request) => {
                    trace(|| format!("window request start={} count={}", request.start_index, request.count));
                    tasks.push(Box::pin(async move {
                        Event::Window(api.download_grants(file_id, &request).await)
                    }));
                }
                dt::Poll::Fetch { index, grant } => {
                    tasks.push(Box::pin(async move {
                        Event::Storage {
                            index,
                            result: fetch_from_storage(storage, grant).await,
                        }
                    }));
                }
                dt::Poll::FetchViaApi {
                    index,
                    object_version_id,
                } => {
                    let Some(length) = planner.geometry().and_then(|g| g.chunk_length(index)) else {
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    };
                    trace(|| format!("chunk {index} via the pinned API route"));
                    tasks.push(Box::pin(async move {
                        Event::Api {
                            index,
                            result: api
                                .download_chunk_pinned(file_id, index, &object_version_id, length)
                                .await,
                        }
                    }));
                }
                dt::Poll::Wait { until_ms } => break until_ms,
                dt::Poll::Done => {
                    return finish(decryptor, out, api_chunks > 0, progress);
                }
                dt::Poll::Stop(verdict) => {
                    drop(tasks);
                    discard(out, progress);
                    return match verdict {
                        dt::Verdict::ApiForFile => {
                            trace(|| "falling back to the API for this file".to_string());
                            Ok(DirectResult::UseApi)
                        }
                        dt::Verdict::Restart => Err(changed()),
                        dt::Verdict::Terminal(t) => Err(terminal_message(&t).into()),
                        dt::Verdict::Corrupt => Err(DownloadError::Unreadable(
                            "download failed: a chunk did not authenticate from storage or from the API; the file \
                             is damaged or was tampered with, nothing was written"
                                .to_string(),
                        )),
                    };
                }
            }
        };

        // Wait for something to finish, or for the next backoff to end.
        let event = if tasks.is_empty() {
            match wake {
                Some(at) => {
                    tokio::time::sleep(Duration::from_millis((at - now_ms()).max(0) as u64)).await;
                    None
                }
                None => {
                    // Nothing outstanding and nothing scheduled: the planner is stuck.
                    // Never hang a download on that; the API path is always correct.
                    trace(|| "planner stalled; falling back to the API".to_string());
                    discard(out, progress);
                    return Ok(DirectResult::UseApi);
                }
            }
        } else if let Some(at) = wake {
            let nap = Duration::from_millis((at - now_ms()).max(0) as u64);
            tokio::select! {
                ev = tasks.next() => ev,
                _ = tokio::time::sleep(nap) => None,
            }
        } else {
            tasks.next().await
        };

        let now = now_ms();
        match event {
            None => {}
            Some(Event::Window(GrantReply::Window(response))) => match planner.window_answered(response, now) {
                Ok(()) => {
                    if !geometry_checked {
                        geometry_checked = true;
                        let known = planner.geometry().map(|g| g.chunk_count);
                        if known != Some(expected_chunks) {
                            // The manifest and the file's metadata disagree: let the
                            // API path, which has its own checks, serve this file.
                            trace(|| "manifest chunk count differs from the metadata".to_string());
                            discard(out, progress);
                            return Ok(DirectResult::UseApi);
                        }
                    }
                }
                Err(violation) => trace(|| format!("window refused: {violation}")),
            },
            Some(Event::Window(GrantReply::Malformed)) => {
                trace(|| "window answer could not be decoded".to_string());
                planner.window_malformed();
            }
            Some(Event::Window(GrantReply::Failed(outcome))) => {
                trace(|| format!("window failed: {outcome:?}"));
                planner.window_failed(outcome, now);
            }
            Some(Event::Storage { index, result }) => match result {
                Ok(bytes) => {
                    if let Err(e) = planner.fetch_ok(index) {
                        trace(|| format!("planner error: {e}"));
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    }
                    ready.insert(index, bytes);
                }
                Err(status) => {
                    trace(|| format!("chunk {index} from storage failed (status {status})"));
                    if let Err(e) = planner.fetch_failed(index, now) {
                        trace(|| format!("planner error: {e}"));
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    }
                }
            },
            Some(Event::Api { index, result }) => match result {
                Ok(bytes) => {
                    if let Err(e) = planner.fetch_ok(index) {
                        trace(|| format!("planner error: {e}"));
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    }
                    api_chunks += 1;
                    ready.insert(index, Bytes::from(bytes));
                }
                Err(ChunkApiError::Restart) => {
                    discard(out, progress);
                    return Err(changed());
                }
                Err(ChunkApiError::Terminal(message)) => {
                    discard(out, progress);
                    return Err(message.into());
                }
                // The pinned route failed after its own retries: nothing per chunk is left to
                // try. The planner ends the file and the API stream takes it from the start.
                Err(ChunkApiError::Failed(message)) => {
                    trace(|| format!("chunk {index} through the pinned API route failed: {message}"));
                    if let Err(e) = planner.api_fetch_failed(index) {
                        trace(|| format!("planner error: {e}"));
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    }
                }
            },
        }

        // Decrypt and write what is ready, strictly in order.
        loop {
            let next = planner.consumed_count();
            let Some(frame) = ready.remove(&next) else { break };
            let Some(geometry) = planner.geometry() else { break };
            if out.is_none() {
                let writer = AtomicFile::create(out_path)?;
                out = Some(Output {
                    writer,
                    progress: progress.begin_file(display_name, geometry.encrypted_size),
                    plaintext_bytes: 0,
                    encrypted_bytes: 0,
                });
            }
            match decryptor.push_frame(&frame) {
                Ok(chunk) => {
                    if let Some(o) = out.as_mut() {
                        if let Err(e) = o.writer.write_all(&chunk.data) {
                            discard(out, progress);
                            return Err(e.into());
                        }
                        o.plaintext_bytes += chunk.data.len() as u64;
                        o.encrypted_bytes += frame.len() as u64;
                        o.progress.chunk_confirmed(frame.len() as u64);
                    }
                    if let Err(e) = planner.consumed(next) {
                        trace(|| format!("planner error: {e}"));
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    }
                }
                Err(_) if allow_legacy && next == 0 && pin != ExpectedChunkFormat::V2 => {
                    // Frame 0 under the string-UUID key failed before any plaintext was written. That
                    // is what a pre-`bb repair` file keyed by the BINARY uuid (or a JSON-blob file)
                    // looks like, and only the API path's buffered fallback can read those: a
                    // refetch would fail the same way and end as "corrupt". Index > 0 stays corrupt.
                    // A V2 pin has no legacy form (its reader tries only the string-UUID key), so a
                    // frame 0 that fails there IS corrupt or tampered, not a legacy file.
                    trace(|| "chunk 0 did not authenticate; the API path's legacy readers take this file".to_string());
                    discard(out, progress);
                    return Ok(DirectResult::UseApi);
                }
                Err(_) => {
                    trace(|| format!("chunk {next} did not authenticate"));
                    if let Err(e) = planner.chunk_unauthentic(next) {
                        trace(|| format!("planner error: {e}"));
                        discard(out, progress);
                        return Ok(DirectResult::UseApi);
                    }
                    break;
                }
            }
        }
    }
}

/// Every chunk is written: the stream must end on the frame that authenticated
/// as last (truncation, task 1908), then the temp file is published.
fn finish(
    decryptor: ChunkDecryptor,
    out: Option<Output>,
    mixed: bool,
    overall: &dyn ChunkProgress,
) -> Result<DirectResult, DownloadError> {
    let Some(out) = out else {
        // Done with nothing written cannot happen (every file has a chunk).
        return Ok(DirectResult::UseApi);
    };
    if let Err(e) = decryptor.finish() {
        out.discard(overall);
        // The stream ended before the frame marked last: the remote copy is cut short or was
        // tampered with. Like a chunk that does not authenticate, this client cannot read it now.
        return Err(DownloadError::Unreadable(format!("download incomplete: {e}")));
    }
    let Output {
        writer,
        progress,
        plaintext_bytes,
        encrypted_bytes,
    } = out;
    if let Err(e) = writer.commit() {
        progress.finish(false);
        return Err(e.into());
    }
    progress.finish(true);
    Ok(DirectResult::Done(DownloadStats {
        plaintext_bytes,
        encrypted_bytes,
        // "mixed": some chunks came through the pinned API route (a link storage would
        // not serve, a window the server could not give); the file is still one version.
        transport: if mixed { "mixed" } else { "direct" },
    }))
}

/// The pinned version went away: only fresh metadata can say what the file is now.
fn changed() -> DownloadError {
    DownloadError::Changed("the file changed while it was downloading; nothing was written".to_string())
}

/// What the user reads for an authorization answer.
fn terminal_message(t: &dt::Terminal) -> String {
    match (t.status, t.error.as_deref()) {
        (401, _) => crate::api::SESSION_EXPIRED_MESSAGE.to_string(),
        (status, Some(code)) => format!("download failed ({status}): {code}"),
        (status, None) => format!("download failed ({status})"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[test]
    fn the_trace_is_on_only_when_the_variable_says_so() {
        assert!(!trace_is_on(None));
        for off in ["", "0", "false", "False", "off", "OFF", "no", " 0 "] {
            assert!(!trace_is_on(Some(off)), "{off:?} must mean off");
        }
        for on in ["1", "true", "yes", "on", "anything"] {
            assert!(trace_is_on(Some(on)), "{on:?} must mean on");
        }
    }

    /// A run-wide progress counter that records what was taken back.
    struct Overall {
        rolled_back: Arc<AtomicU64>,
    }

    impl ChunkProgress for Overall {
        fn begin_file(&self, _: &str, _: u64) -> Box<dyn FileProgress> {
            Box::new(OneFile)
        }
        fn rollback(&self, bytes: u64) {
            self.rolled_back.fetch_add(bytes, Ordering::SeqCst);
        }
    }

    struct OneFile;

    impl FileProgress for OneFile {
        fn chunk_confirmed(&self, _: u64) {}
        fn finish(self: Box<Self>, _: bool) {}
    }

    #[test]
    fn a_discarded_partial_file_takes_its_bytes_back_off_the_run_wide_counter() {
        let dir = std::env::temp_dir().join(format!("bb-discard-2173-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.bin");
        let rolled_back = Arc::new(AtomicU64::new(0));
        let overall = Overall {
            rolled_back: rolled_back.clone(),
        };
        let writer = AtomicFile::create(&out).unwrap();
        let partial = Output {
            writer,
            progress: Box::new(OneFile),
            plaintext_bytes: 1000,
            encrypted_bytes: 1028,
        };
        discard(Some(partial), &overall);
        assert_eq!(rolled_back.load(Ordering::SeqCst), 1028, "what the file had counted");
        assert!(!out.exists(), "nothing was published");
        // No partial output, nothing to take back.
        discard(None, &overall);
        assert_eq!(rolled_back.load(Ordering::SeqCst), 1028);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn unhex(h: &str) -> Vec<u8> {
        (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn a_stream_cut_before_its_last_frame_is_unreadable_and_takes_its_bytes_back() {
        // The checked-in core v2 file with its last frame missing: every frame that arrived
        // authenticates, but the stream never reaches the frame marked last. This client cannot read
        // the file now; it is not "some other failure", and sync decides on the type.
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/core_1908_vectors.json")).unwrap();
        let f = &doc["v2_file"];
        let key: [u8; 32] = unhex(f["master_key_hex"].as_str().unwrap()).try_into().unwrap();
        let mk = MasterKey::from_bytes(key);
        let wire = unhex(f["wire_hex"].as_str().unwrap());
        let stride = f["chunk_size"].as_u64().unwrap() as usize + 28;
        let mut decryptor = ChunkDecryptor::for_push(&mk, f["file_id"].as_str().unwrap(), ExpectedChunkFormat::V2);
        for frame in wire.chunks(stride).take(3) {
            decryptor.push_frame(frame).unwrap();
        }
        let dir = std::env::temp_dir().join(format!("bb-finish-2173-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out_path = dir.join("out.bin");
        let rolled_back = Arc::new(AtomicU64::new(0));
        let overall = Overall {
            rolled_back: rolled_back.clone(),
        };
        let partial = Output {
            writer: AtomicFile::create(&out_path).unwrap(),
            progress: Box::new(OneFile),
            plaintext_bytes: 3,
            encrypted_bytes: 3 * stride as u64,
        };
        match finish(decryptor, Some(partial), false, &overall) {
            Err(DownloadError::Unreadable(message)) => assert!(message.contains("download incomplete"), "{message}"),
            Err(other) => panic!("expected Unreadable, got {other:?}"),
            Ok(_) => panic!("a file without its last frame was published"),
        }
        assert_eq!(rolled_back.load(Ordering::SeqCst), 3 * stride as u64);
        assert!(!out_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
