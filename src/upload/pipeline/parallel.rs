//! The `--parallel` producer/consumer core (spec §4.2, §5): one producer seals chunks in order under a byte
//! budget, `used` workers PUT them concurrently from one `FuturesUnordered`, and a Ctrl-C or a chunk that fails
//! for good drops every PUT in flight at once. Split from `pipeline/mod.rs` (the file-size limit) and the one
//! function core plan task 3.5 later replaces with core's driver.
//!
//! Owns: `run_pipeline`, the per-chunk PUT future and `ProducerErr`.
//! Must not: open or complete a session (that is `pipeline::upload_attempt`).
//! Tests: `parallel_tests`, `memory_tests` and `completed_session_tests` beside `pipeline/mod.rs`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use beebeeb_core::chunk_stream::{ChunkEncryptor, EncryptedChunk};
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;

use super::CHANNEL_CAP;
use crate::api::ApiClient;
use crate::interrupt::Interrupt;
use crate::upload::INTERRUPTED;
use crate::upload::limits::{Admission, BufferShare, ChunkLimits};
use crate::upload::session::AttemptErr;
use crate::upload::{FileProgress, Held, InFlight, Route, Tally};
use tokio::sync::OwnedSemaphorePermit;

/// Distinct producer failure causes, kept separate so the driver can surface an
/// honest reason rather than a generic "upload failed".
pub(super) enum ProducerErr {
    /// A `CoreError` from `next_chunk` (read/encrypt).
    Core(String),
    /// The `finish()` integrity guard tripped — the source shrank mid-stream.
    Finish(String),
    /// The shutdown flag tripped at the top of an iteration.
    Cancelled,
    /// The receiver closed — the consumer hit its own error first (which wins).
    ChannelClosed,
}

/// One chunk off the wire, carrying its budget share and in-flight guard so the consumer can report
/// `chunk_done` (a 2333 seam) BEFORE the guard settles and the share is returned.
struct ChunkDone<'a> {
    index: u32,
    len: u64,
    outcome: ChunkOutcome,
    /// `chunk_settled` fires when this drops: after `chunk_done`, or at once if the future was cancelled.
    _flight: InFlight<'a>,
    /// The frame's budget returns when this drops.
    _share: BufferShare,
    /// Its place among the push's chunks in flight (spec §7), freed when this drops.
    _slot: OwnedSemaphorePermit,
}

/// How one chunk's PUT ended.
enum ChunkOutcome {
    Stored(Held),
    Failed(ChunkFail),
}

/// A chunk that failed for good, turned into an [`AttemptErr`] by the consumer with the tally confirmed so far.
enum ChunkFail {
    SessionGone(String),
    SessionCompleted,
    DirectUnavailable,
    Interrupted,
    Other(String),
}

impl ChunkFail {
    fn into_attempt_err(self, confirmed: Tally) -> AttemptErr {
        match self {
            ChunkFail::SessionGone(message) => AttemptErr::SessionGone { message, confirmed },
            ChunkFail::SessionCompleted => AttemptErr::SessionCompleted { confirmed },
            ChunkFail::DirectUnavailable => AttemptErr::DirectUnavailable { confirmed },
            ChunkFail::Interrupted => AttemptErr::Other(INTERRUPTED.to_string()),
            ChunkFail::Other(message) => AttemptErr::Other(message),
        }
    }
}

/// The future for one chunk's PUT (API or direct). It owns the in-flight guard and the budget share, and returns
/// them with the outcome, so the consumer reports `chunk_done` before either is dropped. Cancelling the future
/// (an interrupt or a sibling's final error) drops both: the settle guard fires and the share returns.
#[allow(clippy::too_many_arguments)]
fn chunk_put<'a>(
    api: &'a ApiClient,
    direct: Option<&'a beebeeb_core::direct_transfer::Discovery>,
    upload_id: &'a crate::api::DirectUploadId,
    upload_session_id: &'a str,
    shutdown: &'a Arc<AtomicBool>,
    file_prog: &'a dyn FileProgress,
    route: Route,
    chunk: EncryptedChunk,
    share: BufferShare,
    slot: OwnedSemaphorePermit,
) -> Pin<Box<dyn Future<Output = ChunkDone<'a>> + Send + 'a>> {
    Box::pin(async move {
        let index = chunk.index;
        let len = chunk.data.len() as u64;
        let flight = InFlight::start(file_prog, index, route);
        let outcome = match direct {
            Some(discovery) => match crate::direct_upload::upload_chunk(
                api,
                discovery,
                upload_id,
                index,
                Bytes::from(chunk.data),
                shutdown,
                file_prog,
            )
            .await
            {
                Ok(held) => ChunkOutcome::Stored(held),
                Err(crate::direct_upload::DirectUploadError::UseApi) => {
                    ChunkOutcome::Failed(ChunkFail::DirectUnavailable)
                }
                Err(crate::direct_upload::DirectUploadError::Interrupted) => {
                    ChunkOutcome::Failed(ChunkFail::Interrupted)
                }
                Err(crate::direct_upload::DirectUploadError::SessionCompleted) => {
                    ChunkOutcome::Failed(ChunkFail::SessionCompleted)
                }
                Err(crate::direct_upload::DirectUploadError::SessionGone(message)) => {
                    ChunkOutcome::Failed(ChunkFail::SessionGone(format!("chunk {index} upload: {message}")))
                }
                Err(crate::direct_upload::DirectUploadError::Terminal { status, message }) => ChunkOutcome::Failed(
                    ChunkFail::Other(format!("chunk {index} upload: {message} (HTTP {status})")),
                ),
            },
            // The API route always sends the bytes, even for a chunk the server answers `skipped`.
            None => match api
                .upload_chunk_observed(upload_session_id, index, Bytes::from(chunk.data), &|| {
                    file_prog.chunk_retried(index)
                })
                .await
            {
                Ok(_) => ChunkOutcome::Stored(Held::Transferred),
                Err(e) if crate::api::is_upload_session_completed(&e) => {
                    ChunkOutcome::Failed(ChunkFail::SessionCompleted)
                }
                Err(e) if crate::api::is_upload_session_gone(&e) => {
                    ChunkOutcome::Failed(ChunkFail::SessionGone(format!("chunk {index} upload: {e}")))
                }
                Err(e) => ChunkOutcome::Failed(ChunkFail::Other(format!("chunk {index} upload: {e}"))),
            },
        };
        ChunkDone {
            index,
            len,
            outcome,
            _flight: flight,
            _share: share,
            _slot: slot,
        }
    })
}

/// The producer/consumer core (`--parallel`, spec §4.2, §5). One producer seals chunks in order, each under a
/// `2 C` budget share it shrinks to `C` once sealed; `used` workers PUT them concurrently from one
/// `FuturesUnordered`, each PUT holding one of the push's chunk slots (spec §7: ≤ `asked` across its files). Returns what it confirmed only when every planned chunk was emitted, server-confirmed,
/// and the byte totals reconcile. Adds the bytes of every chunk it SENT (not one the server already held) to
/// `transferred`. A Ctrl-C (the `interrupt`) or a chunk that fails for good drops every PUT in flight at once.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_pipeline(
    api: &ApiClient,
    upload_session_id: &str,
    direct: Option<&beebeeb_core::direct_transfer::Discovery>,
    encryptor: ChunkEncryptor,
    chunk_count: u32,
    expected_total: u64,
    shutdown: &Arc<AtomicBool>,
    limits: &Arc<ChunkLimits>,
    used: u32,
    chunk_size: u64,
    admission: Admission,
    interrupt: Option<&Interrupt>,
    file_prog: &dyn FileProgress,
    transferred: &mut u64,
) -> Result<Tally, AttemptErr> {
    use tokio::sync::mpsc;

    let upload_id = crate::api::DirectUploadId::from(upload_session_id);
    // The channel carries each sealed frame with its budget share (cap 1: one frame waiting beside the workers).
    let (tx, mut rx) = mpsc::channel::<(EncryptedChunk, BufferShare)>(CHANNEL_CAP);
    let shutdown_p = Arc::clone(shutdown);
    let limits_p = Arc::clone(limits);
    let used = used.max(1) as usize;

    // Producer: ONE blocking task owns the encryptor and the attempt's admission (the read buffer's share and its
    // reserve) for the whole file. Before each seal it takes `2 C` (the frame and the AEAD copy), from the pool or
    // the reserve; once sealed it shrinks to `C` (the copy is freed) and sends the frame with its `C`. The budget
    // wait also selects on `tx.closed()`, so a failed consumer never makes it seal one more chunk.
    let producer = tokio::task::spawn_blocking(move || -> Result<(), ProducerErr> {
        let mut enc = encryptor;
        let admission = admission;
        let handle = tokio::runtime::Handle::current();
        loop {
            // spawn_blocking can't be aborted mid-chunk, so check at the top of each iteration.
            if shutdown_p.load(Ordering::Relaxed) {
                return Err(ProducerErr::Cancelled);
            }
            let mut seal_share = match handle.block_on(async {
                tokio::select! {
                    share = admission.seal_share(&limits_p, 2 * chunk_size) => Some(share),
                    _ = tx.closed() => None,
                }
            }) {
                Some(share) => share,
                None => return Err(ProducerErr::ChannelClosed),
            };
            match enc.next_chunk() {
                Ok(Some(chunk)) => {
                    // The AEAD copy is freed now; the frame keeps its `C` until its PUT settles.
                    seal_share.shrink_to(chunk_size);
                    if tx.blocking_send((chunk, seal_share)).is_err() {
                        return Err(ProducerErr::ChannelClosed);
                    }
                }
                Ok(None) => break, // `seal_share` drops, returning `2 C`.
                Err(e) => return Err(ProducerErr::Core(format!("encrypt chunk: {e}"))),
            }
        }
        // Integrity guard: detects a source that shrank mid-stream.
        enc.finish().map(|_| ()).map_err(|e| ProducerErr::Finish(e.to_string()))
    });

    // Consumer: up to `used` PUTs in flight from one `FuturesUnordered`. It reports `chunk_done` on each 200
    // (counting an already-stored chunk once, a transferred one once and toward `transferred`); the first final
    // error or the interrupt stops it pulling and drops the set, so the chunks in flight are cancelled.
    let route = if direct.is_some() { Route::Direct } else { Route::Api };
    let mut confirmed = Tally::default();
    let mut in_flight: FuturesUnordered<Pin<Box<dyn Future<Output = ChunkDone<'_>> + Send>>> = FuturesUnordered::new();
    let mut consumer_err: Option<AttemptErr> = None;
    let mut sealing_done = false;
    // A chunk taken off the channel, waiting for one of the push's chunk slots (spec §7). Counted with the ones in
    // flight against `used`.
    let mut waiting: Option<(EncryptedChunk, BufferShare)> = None;

    while consumer_err.is_none() {
        // Take a new chunk only while there is room and nothing is stopping the run.
        let may_take = !sealing_done
            && waiting.is_none()
            && in_flight.len() < used
            && !shutdown.load(Ordering::Relaxed)
            && interrupt_clear(interrupt);
        if sealing_done && in_flight.is_empty() && waiting.is_none() {
            break;
        }
        // Nothing in flight and nothing may be taken: the run is stopping (the shutdown flag of `bb sync`, which
        // has no interrupt arm to fire). Without this every `select!` arm is disabled and tokio panics.
        if in_flight.is_empty() && !may_take && waiting.is_none() {
            consumer_err = Some(AttemptErr::Other(INTERRUPTED.to_string()));
            break;
        }
        tokio::select! {
            biased;
            // Ctrl-C: drop every PUT in flight at once (the set is dropped when the loop breaks).
            _ = wait_interrupt(interrupt), if interrupt.is_some() => {
                consumer_err = Some(AttemptErr::Other(INTERRUPTED.to_string()));
            }
            Some(done) = in_flight.next(), if !in_flight.is_empty() => {
                match done.outcome {
                    ChunkOutcome::Stored(held) => {
                        confirmed.bytes += done.len;
                        confirmed.chunks += 1;
                        match held {
                            Held::AlreadyStored => confirmed.already_stored += 1,
                            Held::Transferred => *transferred += done.len,
                        }
                        // Before `done` drops (its `_flight` settles and its `_share` returns).
                        file_prog.chunk_done(done.index, done.len, held);
                    }
                    ChunkOutcome::Failed(fail) => {
                        consumer_err = Some(fail.into_attempt_err(confirmed));
                    }
                }
            }
            slot = limits.chunk_slot(), if waiting.is_some() => {
                if let Some((chunk, share)) = waiting.take() {
                    in_flight.push(chunk_put(
                        api, direct, &upload_id, upload_session_id, shutdown, file_prog, route, chunk, share, slot,
                    ));
                }
            }
            maybe = rx.recv(), if may_take => {
                match maybe {
                    Some(sealed) => waiting = Some(sealed),
                    None => sealing_done = true,
                }
            }
        }
    }

    // ── Cleanup (always): drop the in-flight PUTs, unblock and reap the producer. ──
    // Dropping the set cancels every pending PUT (aborting its body), fires each settle guard and returns each
    // budget share; then closing the receiver wakes a producer parked on the channel or the budget.
    drop(in_flight);
    drop(waiting);
    rx.close();
    let producer_join = producer.await;

    // A proximate consumer error (network, a final answer, cancellation) is what the user needs to see first.
    if let Some(e) = consumer_err {
        return Err(e);
    }
    let producer_err = match producer_join {
        Ok(Ok(())) => None,
        Ok(Err(ProducerErr::Cancelled)) => Some(AttemptErr::Other(INTERRUPTED.to_string())),
        Ok(Err(ProducerErr::ChannelClosed)) => Some(AttemptErr::Other(
            "internal: chunk channel closed before completion".to_string(),
        )),
        // The source file could not be read as planned (LD-0137): the caller may know it changed.
        Ok(Err(ProducerErr::Core(e))) => Some(AttemptErr::Source(e)),
        Ok(Err(ProducerErr::Finish(e))) => Some(AttemptErr::Source(format!("integrity check failed: {e}"))),
        Err(join_err) => Some(AttemptErr::Other(format!("encrypt task failed: {join_err}"))),
    };
    if let Some(e) = producer_err {
        return Err(e);
    }

    // Final client-side guards (hygiene; the server recomputes the total).
    if confirmed.chunks != chunk_count {
        return Err(AttemptErr::Other(format!(
            "incomplete upload: {}/{chunk_count} chunks confirmed",
            confirmed.chunks
        )));
    }
    if confirmed.bytes != expected_total {
        return Err(AttemptErr::Other(format!(
            "ciphertext total mismatch: confirmed {}, expected {expected_total}",
            confirmed.bytes
        )));
    }
    Ok(confirmed)
}

/// `false` once the push's interrupt has fired (so the consumer takes no new chunk); always `true` when there is
/// no interrupt (`bb sync`, which stops through its shutdown flag).
fn interrupt_clear(interrupt: Option<&Interrupt>) -> bool {
    interrupt.is_none_or(|i| !i.is_set())
}

/// Resolves when the push's interrupt fires; never, when there is none (the `select!` arm is then disabled by its
/// `if interrupt.is_some()` guard, so this future is not polled).
async fn wait_interrupt(interrupt: Option<&Interrupt>) {
    match interrupt {
        Some(i) => i.wait().await,
        None => std::future::pending().await,
    }
}
