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
use futures_util::stream::{FuturesUnordered, StreamExt};

use crate::api::{ApiClient, ChunkApiError, GrantReply, now_ms};
use crate::download::{AtomicFile, DownloadStats};
use crate::upload::{ChunkProgress, FileProgress};

/// Result of an attempt to download over the direct transport.
pub(crate) enum DirectResult {
    /// The file is complete and published at the output path.
    Done(DownloadStats),
    /// Direct transport was not used (or was abandoned before anything was
    /// published): download this file through the API path.
    UseApi,
}

/// Support aid: `BB_TRANSPORT_TRACE=1` prints one line per transport decision
/// to stderr. Never a URL, never a header.
fn trace(message: impl FnOnce() -> String) {
    if std::env::var_os("BB_TRANSPORT_TRACE").is_some() {
        eprintln!("bb: transport: {}", message());
    }
}

enum Event {
    Window(GrantReply),
    Storage {
        index: u32,
        /// `Err(status)`: the HTTP status when there was one (0: none).
        result: Result<Vec<u8>, u16>,
    },
    Api {
        index: u32,
        result: Result<Vec<u8>, ChunkApiError>,
    },
}

type Task<'a> = Pin<Box<dyn Future<Output = Event> + Send + 'a>>;

/// Fetch one chunk from its presigned link: `200` and a `Content-Length` equal
/// to the grant's `length`, nothing else. The error carries at most a status.
async fn fetch_from_storage(client: &reqwest::Client, grant: dt::ChunkGrant) -> Result<Vec<u8>, u16> {
    let mut resp = client.get(&grant.url).send().await.map_err(|_| 0u16)?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(status);
    }
    if resp.content_length() != Some(grant.length) {
        return Err(status);
    }
    let want = usize::try_from(grant.length).map_err(|_| status)?;
    // Do not trust the length for the allocation up front; grow as bytes arrive.
    let mut body = Vec::with_capacity(want.min(8 * 1024 * 1024));
    while let Some(piece) = resp.chunk().await.map_err(|_| 0u16)? {
        if body.len() + piece.len() > want {
            return Err(status);
        }
        body.extend_from_slice(&piece);
    }
    if body.len() == want { Ok(body) } else { Err(status) }
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
    fn discard(self) {
        let mut writer = self.writer;
        writer.abort();
        self.progress.finish(false);
    }
}

fn discard(out: Option<Output>) {
    if let Some(out) = out {
        out.discard();
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
) -> Result<DirectResult, String> {
    let Some(discovery) = api.direct_download_offer().await else {
        return Ok(DirectResult::UseApi);
    };
    let Some(storage) = api.storage_client() else {
        return Ok(DirectResult::UseApi);
    };
    let config = dt::PlannerConfig::from_discovery(&discovery, rand::random());
    let mut planner = dt::DownloadPlanner::new(config, discovery, api.api_base(), file_id);
    let mut decryptor = ChunkDecryptor::for_push(master_key, file_id, pin);
    let mut tasks: FuturesUnordered<Task<'_>> = FuturesUnordered::new();
    let mut ready: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    let mut out: Option<Output> = None;
    let mut geometry_checked = false;
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
                    let Some(geometry) = planner.geometry() else {
                        discard(out);
                        return Ok(DirectResult::UseApi);
                    };
                    let length = geometry.chunk_length(index);
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
                    return finish(decryptor, out, planner.geometry());
                }
                dt::Poll::Stop(verdict) => {
                    drop(tasks);
                    discard(out);
                    return match verdict {
                        dt::Verdict::ApiForFile => {
                            trace(|| "falling back to the API for this file".to_string());
                            Ok(DirectResult::UseApi)
                        }
                        dt::Verdict::Restart => Err("the file changed while it was downloading; nothing was \
                                                     written. Run the command again"
                            .to_string()),
                        dt::Verdict::Terminal(t) => Err(terminal_message(&t)),
                        dt::Verdict::Corrupt => Err("download failed: a chunk did not authenticate from storage or \
                                                     from the API; the file is damaged or was tampered with, \
                                                     nothing was written"
                            .to_string()),
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
                    discard(out);
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
                            discard(out);
                            return Ok(DirectResult::UseApi);
                        }
                    }
                }
                Err(violation) => trace(|| format!("window refused: {violation}")),
            },
            Some(Event::Window(GrantReply::Failed(outcome))) => {
                trace(|| format!("window failed: {outcome:?}"));
                planner.window_failed(outcome, now);
            }
            Some(Event::Storage { index, result }) => match result {
                Ok(bytes) => {
                    if planner.fetch_ok(index).is_err() {
                        discard(out);
                        return Ok(DirectResult::UseApi);
                    }
                    ready.insert(index, bytes);
                }
                Err(status) => {
                    trace(|| format!("chunk {index} from storage failed (status {status})"));
                    if planner.fetch_failed(index, now).is_err() {
                        discard(out);
                        return Ok(DirectResult::UseApi);
                    }
                }
            },
            Some(Event::Api { index, result }) => match result {
                Ok(bytes) => {
                    if planner.fetch_ok(index).is_err() {
                        discard(out);
                        return Ok(DirectResult::UseApi);
                    }
                    ready.insert(index, bytes);
                }
                Err(ChunkApiError::Restart) => {
                    discard(out);
                    return Err(
                        "the file changed while it was downloading; nothing was written. Run the \
                                command again"
                            .to_string(),
                    );
                }
                Err(ChunkApiError::Terminal(message) | ChunkApiError::Failed(message)) => {
                    discard(out);
                    return Err(message);
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
                            discard(out);
                            return Err(e);
                        }
                        o.plaintext_bytes += chunk.data.len() as u64;
                        o.encrypted_bytes += frame.len() as u64;
                        o.progress.chunk_confirmed(frame.len() as u64);
                    }
                    if planner.consumed(next).is_err() {
                        discard(out);
                        return Ok(DirectResult::UseApi);
                    }
                }
                Err(_) => {
                    trace(|| format!("chunk {next} did not authenticate"));
                    if planner.chunk_unauthentic(next).is_err() {
                        discard(out);
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
    geometry: Option<dt::Geometry>,
) -> Result<DirectResult, String> {
    let Some(out) = out else {
        // Done with nothing written cannot happen (every file has a chunk).
        return Ok(DirectResult::UseApi);
    };
    if let Err(e) = decryptor.finish() {
        out.discard();
        return Err(format!("download incomplete: {e}"));
    }
    let Output {
        writer,
        progress,
        plaintext_bytes,
        encrypted_bytes,
    } = out;
    if let Some(g) = geometry {
        debug_assert_eq!(g.encrypted_size, encrypted_bytes);
    }
    if let Err(e) = writer.commit() {
        progress.finish(false);
        return Err(e);
    }
    progress.finish(true);
    Ok(DirectResult::Done(DownloadStats {
        plaintext_bytes,
        encrypted_bytes,
        transport: "direct",
    }))
}

/// What the user reads for an authorization answer.
fn terminal_message(t: &dt::Terminal) -> String {
    match (t.status, t.error.as_deref()) {
        (401, _) => crate::api::SESSION_EXPIRED_MESSAGE.to_string(),
        (status, Some(code)) => format!("download failed ({status}): {code}"),
        (status, None) => format!("download failed ({status})"),
    }
}
