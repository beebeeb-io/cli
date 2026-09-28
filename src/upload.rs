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
    /// For a `bb push --replace`: the existing file's current `version_number`,
    /// passed to the v2 init as `base_version_number` so the server enforces an
    /// optimistic concurrency check (stale-version 409) and bumps the version.
    /// `None` for a fresh upload (the server then keys off `file_id` absence /
    /// presence to decide replace vs. new).
    pub base_version_number: Option<i32>,
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
    /// Undo ciphertext bytes a FAILED attempt already reported via
    /// [`FileProgress::chunk_confirmed`] on the shared/overall counter (task
    /// 1589, Codex thread #2). A swept-session recovery restarts the same
    /// logical file from chunk 0 under a brand-new [`FileProgress`] handle; if
    /// the failed attempt's bytes are left in place, the retry's own
    /// confirmations on top of them make the run-wide total exceed the
    /// run-wide plan. No-op by default (also correct for [`NoopProgress`]).
    fn rollback(&self, _ciphertext_bytes: u64) {}
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

    fn rollback(&self, ciphertext_bytes: u64) {
        // `ProgressBar` has no `dec`; subtract via `set_position`. This is a
        // best-effort display correction (a `bb sync` file that rolls back
        // while OTHER files are concurrently incrementing the same shared bar
        // can race this read-then-write), guarded with `saturating_sub` so it
        // can never wrap the position negative.
        let pos = self.overall.position();
        self.overall.set_position(pos.saturating_sub(ciphertext_bytes));
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

/// How one upload attempt obtains its v2 session.
#[derive(Clone, Copy, Debug)]
enum SessionPlan {
    /// Reuse a session recorded by an interrupted earlier run (skip init).
    Resume { file_id: Uuid, session_id: Uuid },
    /// Open a new session for `file_id`.
    Init {
        file_id: Uuid,
        /// Set only on the re-init after a swept session (task 1589): if the
        /// server refuses the stored id with the ONE status that proves it is
        /// unusable, the driver may try once more with this fresh id. `None`
        /// for a `--replace` recovery — there IS no fresh id a replace could
        /// use, since it must land on this exact existing file (Codex thread
        /// #1 follow-up on 3cb4daf).
        fallback_file_id: Option<Uuid>,
        /// True for a re-init after a swept (resumed) session — eligible for
        /// the bounded SAME-id 5xx retry regardless of whether
        /// `fallback_file_id` is set. Kept SEPARATE from `fallback_file_id`
        /// precisely because a replace recovery has no fallback id but must
        /// still get the retry: gating the retry on `fallback_file_id.is_some()`
        /// silently skipped it for every `--replace` upload.
        is_recovery: bool,
    },
}

/// Why one attempt failed, kept typed so the driver can react to exactly the
/// two recoverable cases and nothing else.
enum AttemptErr {
    /// The server no longer has the upload session (task 1589: its lease
    /// expired and the sweeper removed it). Recoverable once per file per run by
    /// a re-init. `bytes_confirmed` is how much ciphertext THIS attempt had
    /// already reported to `FileProgress::chunk_confirmed` before it died —
    /// the retry restarts at chunk 0 under a fresh progress handle, so the
    /// caller must roll this amount back on the shared/overall counter before
    /// looping, or a full resend double-counts it (Codex thread #2).
    SessionGone { message: String, bytes_confirmed: u64 },
    /// The init for the stored `file_id` was refused in a way a fresh id can
    /// fix. Carries the fresh id to try (at most once).
    InitRefused { message: String, fresh_file_id: Uuid },
    /// Anything else — surfaced to the caller unchanged.
    Other(String),
}

impl AttemptErr {
    fn into_message(self) -> String {
        match self {
            AttemptErr::SessionGone { message, .. }
            | AttemptErr::InitRefused { message, .. }
            | AttemptErr::Other(message) => message,
        }
    }
}

impl From<String> for AttemptErr {
    fn from(m: String) -> Self {
        AttemptErr::Other(m)
    }
}

/// Map a chunk-PUT / complete failure: a swept session becomes
/// [`AttemptErr::SessionGone`], everything else [`AttemptErr::Other`] with the
/// same text the caller always saw. `bytes_confirmed` is the ciphertext this
/// attempt had already gotten past the server before `e` — see
/// [`AttemptErr::SessionGone`].
fn session_err(prefix: Option<String>, e: crate::api::ApiError, bytes_confirmed: u64) -> AttemptErr {
    let gone = crate::api::is_upload_session_gone(&e);
    let text = match prefix {
        Some(p) => format!("{p}: {e}"),
        None => e.message,
    };
    if gone {
        AttemptErr::SessionGone {
            message: text,
            bytes_confirmed,
        }
    } else {
        AttemptErr::Other(text)
    }
}

/// Whether an init refused for the STORED `file_id` (the swept-session
/// recovery path) may be retried with a fresh id instead. Grounded in the v2
/// init handler (`beebeeb-api/src/routes/uploads.rs`, `init_upload`): the
/// ONLY response that PROVES the stored id itself is unusable — rather than
/// merely reflecting a transient or unrelated failure — is `404`, returned
/// when an explicit `file_id` resolves to a file this caller can no longer
/// read at all (cross-owner, shared access since revoked; ~line 433). Every
/// other 4xx that handler can return is unrelated to the id: `400` is generic
/// request validation (file name / chunk-plan shape) that would reject a
/// fresh id identically; `409` is a live upload or a stale base version, and
/// swapping ids there would create a duplicate file (excluded below); `402`/
/// `413` are quota. A `5xx` proves nothing about the id EITHER WAY — it can be
/// transient, or the server may have committed the init despite the response
/// never arriving (task 1589, Codex thread #1) — so it is never treated as an
/// id problem here; the caller retries it with the SAME id
/// (`STORED_ID_INIT_RETRIES`, bounded, with backoff) and surfaces it if that
/// runs out. Never for 401/403 (auth), 429 (handled in `api`).
fn fresh_id_can_fix(status: u16) -> bool {
    status == 404
}

/// How many extra attempts a `5xx` from the STORED-id recovery init gets, with
/// the SAME id, before it is surfaced — never replaced with a fresh id (task
/// 1589, Codex thread #1). Exponential backoff starting at
/// `STORED_ID_INIT_BACKOFF_BASE`, doubling each attempt.
const STORED_ID_INIT_RETRIES: u32 = 3;
const STORED_ID_INIT_BACKOFF_BASE: Duration = Duration::from_millis(150);

/// Encrypt and upload one file with constant memory and honest progress.
///
/// Shared by `bb sync` (called inside the `buffer_unordered` closure, so files
/// run in parallel) and `bb push` (called sequentially). The master key is held
/// behind an `Arc` and used only synchronously here; only the `FileKey`-owning
/// encryptor crosses into the blocking thread.
///
/// ## Swept sessions (task 1589)
///
/// A resumed session (from `pending-uploads.json`) may no longer exist: the
/// server gives every upload session an expiring lease and sweeps it once the
/// lease runs out. Its chunk PUT / complete then answer 404 (or 400 "not
/// writable: expired" on older servers). The driver then drops the stale
/// record, re-inits with the stored `file_id` (the server re-creates a swept
/// new file under the same id, or versions the existing one), and uploads from
/// chunk 0. If the server refuses the stored id, it tries once with the
/// caller's fresh id. At most ONE re-init per file per run — a second swept
/// session in the same run is returned as an error, never looped on.
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
    //     entry (file_id + v2 upload_session_id) for this path; reuse it — and
    //     let the server short-circuit chunks it already has — only if the file
    //     is unchanged. Otherwise mint a fresh upload (`spec.file_id` + a new
    //     init session). The v2 init rejects re-init of a file whose upload
    //     still holds a live lease (409), so a resumed upload must SKIP
    //     `upload_init` and re-PUT every chunk to the stored session; the v2
    //     chunk route is idempotent (already-stored → `{skipped:true}`), so
    //     re-PUTs are cheap.
    let mut plan = match crate::resume::resumable_upload(&spec.path, size, mtime_ns) {
        Some((file_id, session_id)) => SessionPlan::Resume { file_id, session_id },
        None => SessionPlan::Init {
            file_id: spec.file_id,
            fallback_file_id: None,
            is_recovery: false,
        },
    };
    loop {
        match upload_attempt(api, &master_key, &spec, size, mtime_ns, plan, progress).await {
            Ok(outcome) => return Ok(outcome),
            Err(AttemptErr::SessionGone {
                message: msg,
                bytes_confirmed,
            }) => {
                // Only a RESUMED session is re-inited, and a re-init always
                // yields an `Init` plan — so this arm runs at most once per
                // file per run (the loop bound). A session this run opened
                // itself vanishing is not the case this recovery is for.
                let SessionPlan::Resume { file_id, .. } = plan else {
                    return Err(msg);
                };
                // The dead attempt's chunk_confirmed reports must not survive
                // into the retry's own count (Codex thread #2): the retry gets
                // a BRAND NEW `FileProgress` from `begin_file` and re-sends
                // every chunk from 0, so anything left on the shared/overall
                // counter from this failed attempt would be double-counted
                // once the retry's own confirmations land on top of it.
                if bytes_confirmed > 0 {
                    progress.rollback(bytes_confirmed);
                }
                // The recorded session is dead: drop it so neither this run nor
                // a later one ever PUTs to it again.
                crate::resume::clear(&spec.path);
                if !crate::ui::is_quiet() {
                    eprintln!(
                        "  {}: the interrupted upload expired on the server; starting it again",
                        display_name(&spec.file_name)
                    );
                }
                plan = SessionPlan::Init {
                    file_id,
                    fallback_file_id: (file_id != spec.file_id).then_some(spec.file_id),
                    is_recovery: true,
                };
            }
            Err(AttemptErr::InitRefused { fresh_file_id, .. }) => {
                plan = SessionPlan::Init {
                    file_id: fresh_file_id,
                    fallback_file_id: None,
                    is_recovery: false,
                };
            }
            Err(e) => return Err(e.into_message()),
        }
    }
}

/// One attempt: build the encryptor for `plan`'s file id, open (or reuse) the
/// session, stream every chunk, complete. Consumes nothing the retry needs —
/// the encryptor is rebuilt per attempt because the file key derives from the
/// file id.
async fn upload_attempt(
    api: &ApiClient,
    master_key: &Arc<MasterKey>,
    spec: &UploadSpec,
    size: u64,
    mtime_ns: u128,
    plan: SessionPlan,
    progress: &dyn ChunkProgress,
) -> Result<UploadOutcome, AttemptErr> {
    let (file_id, resumed_session) = match plan {
        SessionPlan::Resume { file_id, session_id } => (file_id, Some(session_id)),
        SessionPlan::Init { file_id, .. } => (file_id, None),
    };
    let file_id_str = file_id.to_string();

    // 2. Encrypt the name + MIME envelope (master key used synchronously). The
    //    name key derives from the DURABLE file_id, so a replace re-encrypts the
    //    name under the same id the server already stores.
    let mime = beebeeb_core::media::guess_mime_type(&spec.file_name);
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(master_key, &file_id_str, &spec.file_name, mime)
        .map_err(|e| format!("encrypt name: {e}"))?;
    let is_media = beebeeb_core::media::is_media(mime);

    // 3. Open the file and build the streaming encryptor (derives the file key
    //    once; the encryptor owns it and is `Send`). The CLI uses the Cli
    //    profile with a concurrency-aware chunk size: parallel `bb sync` uploads
    //    emit smaller chunks (memory budget), while `bb push` (concurrency 1)
    //    gets the full Cli 128 MiB cap. This same plan is sent to the v2 init so
    //    the server frames the file exactly as the encryptor emits it.
    let chunk_size =
        beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, spec.concurrency.max(1)).chunk_size_bytes;
    let file = std::fs::File::open(&spec.path).map_err(|e| format!("open {}: {e}", spec.path.display()))?;
    let encryptor = ChunkEncryptor::from_reader_with_chunk_size(master_key, &file_id_str, size, chunk_size, file)
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
            return Err(AttemptErr::Other(format!(
                "{} changed size during scan ({size} → {}); will retry next run",
                spec.file_name,
                m2.len()
            )));
        }
    }

    // 5. v2 init: open an upload SESSION on `/api/v1/uploads/init`. We send the
    //    PLAINTEXT `size` (the v2 contract — the server recomputes stored bytes
    //    from the summed chunks at complete) plus the exact chunk plan the
    //    encryptor uses. On a replace (`spec.base_version_number` is `Some`) the
    //    server versions by `file_id` and enforces the stale-version 409. On
    //    resume the session already exists, so reuse the stored session id and
    //    skip straight to the pipeline.
    //
    //    `upload_session_id` keys the chunk PUTs + complete; `server_id` is the
    //    durable file id returned by init (equals `file_id` on replace).
    let (upload_session_id, server_id) = if let Some(sid) = resumed_session {
        (sid.to_string(), file_id_str.clone())
    } else {
        // The STORED-id recovery init (a re-init after a swept session) gets a
        // bounded, backed-off retry on a `5xx` before anything else runs: a
        // `5xx` never proves the id itself is bad (Codex thread #1 — see
        // `fresh_id_can_fix`), so it is not treated as fallback-worthy, but
        // giving up on the FIRST one would surface transient failures a
        // plain retry would have ridden out. Gated on `is_recovery`, NOT on
        // `fallback_file_id.is_some()` — a `--replace` recovery has no
        // fallback id (there is no OTHER id it could use) but must still get
        // this retry (Codex thread #1 follow-up on 3cb4daf). An ordinary
        // (non-recovery) init is unaffected: it still fails on the first
        // error, unchanged from before this fix.
        let is_stored_id_recovery = matches!(plan, SessionPlan::Init { is_recovery: true, .. });
        let mut retries_used = 0u32;
        let init = loop {
            match api
                .upload_init_typed(
                    Some(file_id),
                    &name_encrypted,
                    spec.parent_id,
                    size as i64,
                    chunk_size as i64,
                    chunk_count as i32,
                    is_media,
                    spec.base_version_number,
                )
                .await
            {
                Ok(init) => break init,
                Err(e)
                    if is_stored_id_recovery
                        && (500..=599).contains(&e.status)
                        && retries_used < STORED_ID_INIT_RETRIES =>
                {
                    tokio::time::sleep(STORED_ID_INIT_BACKOFF_BASE * 2u32.pow(retries_used)).await;
                    retries_used += 1;
                }
                Err(e) => {
                    return Err(match plan {
                        SessionPlan::Init {
                            fallback_file_id: Some(fresh_file_id),
                            ..
                        } if fresh_id_can_fix(e.status) => AttemptErr::InitRefused {
                            message: e.message,
                            fresh_file_id,
                        },
                        _ => AttemptErr::Other(e.message),
                    });
                }
            }
        };
        // The server's plan is authoritative — frame the upload against it, not
        // a hardcoded local guess. We sent our `ChunkEncryptor` plan; the v2
        // server uses client `chunk_size_bytes`/`chunk_count` verbatim, so the
        // two MUST agree. A mismatch means the server reframed (e.g. a profile
        // ceiling), which would desync the chunk PUTs from the encryptor — bail
        // loudly rather than upload a corrupt frame layout.
        if init.chunk_count != chunk_count as u64 || init.chunk_size_bytes != chunk_size {
            return Err(AttemptErr::Other(format!(
                "server reframed the upload plan (sent {chunk_count}×{chunk_size}B, \
                 got {}×{}B) — refusing to upload a mismatched layout",
                init.chunk_count, init.chunk_size_bytes
            )));
        }
        // Record this upload as resumable now that the session exists, BEFORE
        // any chunk goes out, so an interrupt mid-stream leaves a record the
        // next run can pick up.
        if let Ok(sid) = init.upload_session_id.parse::<Uuid>() {
            crate::resume::record(&spec.path, file_id, sid, size, mtime_ns);
        }
        (init.upload_session_id, init.file_id)
    };

    // 6. Run the look-ahead pipeline. Every chunk is PUT to the session; on a
    //    resumed session the server returns `{skipped:true}` for chunks it
    //    already holds, so re-PUTs are cheap and correct.
    let file_prog = progress.begin_file(&spec.file_name, expected_total);
    let result = run_pipeline(
        api,
        &upload_session_id,
        encryptor,
        chunk_count,
        expected_total,
        &spec.shutdown,
        file_prog.as_ref(),
    )
    .await;

    // `finish(true)` — which also advances the shared files-done counter — is
    // deliberately NOT called here on `Ok(())`. Every chunk landing is not yet
    // "this file is done": `complete` below can still fail (task 1589: the
    // session's lease can expire between the last chunk PUT and `complete`).
    // Calling `finish(true)` here and then retrying the whole file from chunk
    // 0 on a swept `complete` used to advance files-done TWICE for one
    // logical file (Codex thread #2). So a failure at either stage finishes
    // the bar as `false`, and `finish(true)` fires only once both stages of
    // THIS attempt actually succeeded.
    if let Err(e) = result {
        file_prog.finish(false);
        return Err(e);
    }

    // 7. Finalise the version.
    if let Err(e) = api.upload_complete_typed(&upload_session_id).await {
        // Every chunk was confirmed (we only reach here on `result == Ok(())`,
        // which `run_pipeline` only returns once `confirmed_bytes ==
        // expected_total`) — so a swept-session retry must roll back exactly
        // `expected_total`, the full amount THIS attempt reported.
        file_prog.finish(false);
        return Err(session_err(None, e, expected_total));
    }
    file_prog.finish(true);

    // 7b. Upload done — drop the resume record so a future upload of this path
    //     (e.g. a changed version) starts fresh rather than resuming this id.
    crate::resume::clear(&spec.path);

    // 8. Thumbnails: image/* only, one bounded extra read, best-effort.
    maybe_upload_thumbnail(api, master_key, &file_id_str, &server_id, &spec.path, mime, size).await;

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
async fn run_pipeline(
    api: &ApiClient,
    upload_session_id: &str,
    encryptor: ChunkEncryptor,
    chunk_count: u32,
    expected_total: u64,
    shutdown: &Arc<AtomicBool>,
    file_prog: &dyn FileProgress,
) -> Result<(), AttemptErr> {
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
    let mut consumer_err: Option<AttemptErr> = None;
    while let Some(chunk) = rx.recv().await {
        if shutdown.load(Ordering::Relaxed) {
            consumer_err = Some(AttemptErr::Other(INTERRUPTED.to_string()));
            break;
        }
        let len = chunk.data.len() as u64;
        let index = chunk.index;
        // The v2 chunk route is idempotent: on a resumed session a chunk the
        // server already holds returns `{skipped:true}` cheaply (the blob isn't
        // re-written). So we always PUT and count the 200 either way — no
        // client-side "present" set needed.
        match api
            .upload_chunk_typed(upload_session_id, index, Bytes::from(chunk.data))
            .await
        {
            Ok(_) => {
                confirmed_bytes += len;
                confirmed_chunks += 1;
                file_prog.chunk_confirmed(len);
            }
            Err(e) => {
                // `confirmed_bytes` is every byte THIS chunk-PUT loop already
                // reported to `file_prog.chunk_confirmed` before `e` — exactly
                // what a swept-session retry (starting a fresh `FileProgress`
                // at chunk 0) must roll back on the shared/overall counter.
                consumer_err = Some(session_err(Some(format!("chunk {index} upload")), e, confirmed_bytes));
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
    let producer_err = match producer_join {
        Ok(Ok(())) => None,
        Ok(Err(ProducerErr::Cancelled)) => Some(INTERRUPTED.to_string()),
        Ok(Err(ProducerErr::ChannelClosed)) => Some("internal: chunk channel closed before completion".to_string()),
        Ok(Err(ProducerErr::Core(e))) => Some(e),
        Ok(Err(ProducerErr::Finish(e))) => Some(format!("integrity check failed: {e}")),
        Err(join_err) => Some(format!("encrypt task failed: {join_err}")),
    };
    if let Some(e) = producer_err {
        return Err(AttemptErr::Other(e));
    }

    // Final client-side guards (hygiene; the server recomputes the total).
    if confirmed_chunks != chunk_count {
        return Err(AttemptErr::Other(format!(
            "incomplete upload: {confirmed_chunks}/{chunk_count} chunks confirmed"
        )));
    }
    if confirmed_bytes != expected_total {
        return Err(AttemptErr::Other(format!(
            "ciphertext total mismatch: confirmed {confirmed_bytes}, expected {expected_total}"
        )));
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

/// Task 1589: a resumed upload whose server session was swept (lease expired)
/// must drop its `pending-uploads.json` record, re-init ONCE (stored
/// `file_id` first, a fresh one only if the server refuses it), and upload from
/// chunk 0 — never PUT to the dead session forever, never loop.
#[cfg(test)]
mod swept_session_reinit {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::extract::{Path, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::{post, put};
    use axum::{Json, Router};
    use beebeeb_core::kdf::MasterKey;
    use futures_util::StreamExt;
    use serde_json::json;
    use uuid::Uuid;

    use super::{ChunkProgress, FileProgress, NoopProgress, UploadSpec, stream_encrypt_upload};
    use crate::api::ApiClient;

    /// What the mock answers for a session it no longer has.
    #[derive(Clone, Copy)]
    enum Gone {
        /// Current servers (PR #120): 404.
        NotFound,
        /// Older servers for a kept `status='expired'` row.
        BadRequestExpired,
    }

    #[derive(Default)]
    struct Mock {
        /// Sessions the server holds. Anything else is "swept".
        live: HashSet<String>,
        /// Sessions whose complete answers "gone" although chunks were taken.
        complete_gone: HashSet<String>,
        /// When set, sessions opened by init are dead immediately (to prove the
        /// driver re-inits at most once).
        kill_new_sessions: bool,
        /// Init answers this status for the given file_id, every time.
        init_refuse: HashMap<String, u16>,
        /// Init answers `.0` for the given file_id, `.1` more times (counting
        /// down), then succeeds normally — for proving a bounded SAME-id retry
        /// (task 1589, Codex thread #1). Checked before `init_refuse`.
        init_fail_then_succeed: HashMap<String, (u16, u32)>,
        /// A session in `live` still goes "gone" once it has already ACCEPTED
        /// this many chunk PUTs — proving a MID-pipeline sweep (some chunks
        /// already confirmed) rolls those bytes back rather than
        /// double-counting them on the retry's full resend (task 1589, Codex
        /// thread #2).
        dies_after_chunks: HashMap<String, u32>,
        chunk_accept_count: HashMap<String, u32>,
        gone: Option<Gone>,
        /// file_id sent on every init, in order.
        inits: Vec<String>,
        /// (session, index) of every chunk PUT, in order.
        chunk_puts: Vec<(String, u32)>,
        /// session of every successful complete.
        completed: Vec<String>,
        /// session -> file_id, for complete's response.
        session_file: HashMap<String, String>,
    }

    type Shared = Arc<Mutex<Mock>>;

    fn gone_response(g: Option<Gone>) -> Response {
        match g.unwrap_or(Gone::NotFound) {
            Gone::NotFound => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
            Gone::BadRequestExpired => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "upload session is not writable: expired" })),
            )
                .into_response(),
        }
    }

    async fn spawn(state: Shared) -> String {
        let app = Router::new()
            .route(
                "/api/v1/uploads/init",
                post(
                    |State(st): State<Shared>, Json(body): Json<serde_json::Value>| async move {
                        let file_id = body
                            .get("file_id")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .unwrap_or_else(|| Uuid::new_v4().to_string());
                        let mut m = st.lock().unwrap();
                        m.inits.push(file_id.clone());
                        if let Some((code, remaining)) = m.init_fail_then_succeed.get_mut(&file_id) {
                            if *remaining > 0 {
                                *remaining -= 1;
                                let status = StatusCode::from_u16(*code).unwrap();
                                return (status, Json(json!({ "error": "refused by mock" }))).into_response();
                            }
                        }
                        if let Some(code) = m.init_refuse.get(&file_id).copied() {
                            let status = StatusCode::from_u16(code).unwrap();
                            return (status, Json(json!({ "error": "refused by mock" }))).into_response();
                        }
                        let sid = Uuid::new_v4().to_string();
                        if !m.kill_new_sessions {
                            m.live.insert(sid.clone());
                        }
                        m.session_file.insert(sid.clone(), file_id.clone());
                        Json(json!({
                            "upload_session_id": sid,
                            "file_id": file_id,
                            "chunk_size_bytes": body.get("chunk_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                            "chunk_count": body.get("chunk_count").and_then(|v| v.as_u64()).unwrap_or(0),
                        }))
                        .into_response()
                    },
                ),
            )
            .route(
                "/api/v1/uploads/:session/chunks/:idx",
                put(
                    |State(st): State<Shared>, Path((sid, idx)): Path<(String, u32)>, body: Body| async move {
                        let mut stream = body.into_data_stream();
                        let mut n = 0usize;
                        while let Some(frame) = stream.next().await {
                            n += frame.map(|b| b.len()).unwrap_or(0);
                        }
                        let mut m = st.lock().unwrap();
                        m.chunk_puts.push((sid.clone(), idx));
                        if !m.live.contains(&sid) {
                            return gone_response(m.gone);
                        }
                        if let Some(&limit) = m.dies_after_chunks.get(&sid) {
                            let count = *m.chunk_accept_count.get(&sid).unwrap_or(&0);
                            if count >= limit {
                                return gone_response(m.gone);
                            }
                            m.chunk_accept_count.insert(sid.clone(), count + 1);
                        }
                        Json(json!({ "index": idx, "size": n, "skipped": false })).into_response()
                    },
                ),
            )
            .route(
                "/api/v1/uploads/:session/complete",
                post(|State(st): State<Shared>, Path(sid): Path<String>| async move {
                    let mut m = st.lock().unwrap();
                    if !m.live.contains(&sid) || m.complete_gone.contains(&sid) {
                        return gone_response(m.gone);
                    }
                    m.completed.push(sid.clone());
                    let fid = m.session_file.get(&sid).cloned().unwrap_or_default();
                    Json(json!({ "id": fid, "size_bytes": 0, "chunk_count": 0 })).into_response()
                }),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// A scratch dir with the sidecar redirected into it. Holding the guard
    /// serialises every test that touches the process-global env var.
    struct Scratch {
        dir: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let guard = crate::resume::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!("bb-1589-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // SAFETY: serialised by TEST_ENV_LOCK; no other thread reads it
            // concurrently outside that lock.
            unsafe {
                std::env::set_var("BB_PENDING_UPLOADS_PATH", dir.join("pending.json"));
            }
            Self { dir, _guard: guard }
        }

        fn file(&self, size: usize) -> (std::path::PathBuf, u64, u128) {
            let p = self.dir.join("payload.bin");
            std::fs::write(&p, vec![0x5Au8; size]).unwrap();
            let meta = std::fs::metadata(&p).unwrap();
            let mtime = meta
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            (p, meta.len(), mtime)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("BB_PENDING_UPLOADS_PATH");
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn spec(path: &std::path::Path, file_id: Uuid) -> UploadSpec {
        UploadSpec {
            path: path.to_path_buf(),
            file_name: "payload.bin".to_string(),
            file_id,
            base_version_number: None,
            parent_id: None,
            concurrency: 1,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn run(state: &Shared, spec: UploadSpec) -> Result<super::UploadOutcome, String> {
        let base = spawn(state.clone()).await;
        let api = ApiClient::new_for_test(base);
        let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
        stream_encrypt_upload(&api, mk, spec, &NoopProgress).await
    }

    async fn run_with_progress(
        state: &Shared,
        spec: UploadSpec,
        progress: &dyn ChunkProgress,
    ) -> Result<super::UploadOutcome, String> {
        let base = spawn(state.clone()).await;
        let api = ApiClient::new_for_test(base);
        let mk = Arc::new(MasterKey::from_bytes([9u8; 32]));
        stream_encrypt_upload(&api, mk, spec, progress).await
    }

    /// Records exactly the two run-wide numbers a REAL `ChunkProgress`
    /// consumer (`BarProgress`) exposes to the user: the shared ciphertext
    /// total after every `chunk_confirmed` and every `rollback`, and how many
    /// times a file was reported finished successfully. Proves Codex thread
    /// #2: a swept-session retry — a brand-new `FileProgress` per attempt,
    /// restarting at chunk 0 — must not leave either number counting the same
    /// logical file twice.
    #[derive(Default)]
    struct RecordingProgress {
        overall_bytes: Arc<std::sync::atomic::AtomicU64>,
        files_done: Arc<std::sync::atomic::AtomicU64>,
    }

    impl ChunkProgress for RecordingProgress {
        fn begin_file(&self, _file_name: &str, _expected_ciphertext: u64) -> Box<dyn FileProgress> {
            Box::new(RecordingFileProgress {
                overall_bytes: Arc::clone(&self.overall_bytes),
                files_done: Arc::clone(&self.files_done),
            })
        }

        fn rollback(&self, ciphertext_bytes: u64) {
            let _ = self.overall_bytes.fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |v| Some(v.saturating_sub(ciphertext_bytes)),
            );
        }
    }

    struct RecordingFileProgress {
        overall_bytes: Arc<std::sync::atomic::AtomicU64>,
        files_done: Arc<std::sync::atomic::AtomicU64>,
    }

    impl FileProgress for RecordingFileProgress {
        fn chunk_confirmed(&self, ciphertext_bytes: u64) {
            self.overall_bytes
                .fetch_add(ciphertext_bytes, std::sync::atomic::Ordering::Relaxed);
        }

        fn finish(self: Box<Self>, success: bool) {
            if success {
                self.files_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// Shared body of the two "swept on the first chunk PUT" cases.
    async fn swept_on_chunk_recovers(gone: Gone, tag: &str) {
        let s = Scratch::new(tag);
        let (path, size, mtime) = s.file(300 * 1024);
        let stored_fid = Uuid::new_v4();
        let dead_sid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, dead_sid, size, mtime);

        let state: Shared = Arc::new(Mutex::new(Mock {
            gone: Some(gone),
            ..Default::default()
        }));
        let fresh_fid = Uuid::new_v4();
        let out = run(&state, spec(&path, fresh_fid)).await.expect("upload must recover");

        let m = state.lock().unwrap();
        // The dead session was tried first (the resume), exactly once.
        assert_eq!(m.chunk_puts.first().map(|c| c.0.clone()), Some(dead_sid.to_string()));
        assert_eq!(m.chunk_puts.iter().filter(|c| c.0 == dead_sid.to_string()).count(), 1);
        // Exactly one re-init, with the STORED file_id.
        assert_eq!(m.inits, vec![stored_fid.to_string()]);
        // The new session got every chunk from 0 and was completed.
        let new_sid = m.completed.first().cloned().expect("new session completed");
        let idx: Vec<u32> = m.chunk_puts.iter().filter(|c| c.0 == new_sid).map(|c| c.1).collect();
        assert_eq!(idx.first(), Some(&0), "re-upload starts at chunk 0");
        assert!(idx.windows(2).all(|w| w[1] == w[0] + 1));
        assert_eq!(out.server_id, stored_fid);
        // The stale record is gone (and the success cleared the new one).
        assert_eq!(crate::resume::resumable_upload(&path, size, mtime), None);
    }

    #[tokio::test]
    async fn resumed_session_404_on_chunk_reinits_with_stored_id() {
        swept_on_chunk_recovers(Gone::NotFound, "chunk404").await;
    }

    #[tokio::test]
    async fn resumed_session_400_expired_on_chunk_reinits_with_stored_id() {
        swept_on_chunk_recovers(Gone::BadRequestExpired, "chunk400").await;
    }

    #[tokio::test]
    async fn resumed_session_404_on_complete_reinits() {
        let s = Scratch::new("complete404");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        let sid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, sid, size, mtime);
        let mut mock = Mock::default();
        mock.live.insert(sid.to_string());
        mock.complete_gone.insert(sid.to_string());
        let state: Shared = Arc::new(Mutex::new(mock));

        let out = run(&state, spec(&path, Uuid::new_v4()))
            .await
            .expect("upload must recover");
        let m = state.lock().unwrap();
        assert_eq!(m.inits, vec![stored_fid.to_string()]);
        assert_eq!(m.completed.len(), 1);
        assert_ne!(m.completed[0], sid.to_string());
        assert_eq!(out.server_id, stored_fid);
        assert_eq!(crate::resume::resumable_upload(&path, size, mtime), None);
    }

    /// Codex thread #2, the `complete` trigger: every chunk of the first
    /// attempt landed (so `finish(true)` would have already fired under the
    /// PRE-fix ordering) before `complete` answered "gone". The retry re-sends
    /// every chunk from 0 under a brand-new `FileProgress`. The run-wide
    /// totals must reflect ONE finished file at its real size — not two
    /// finished-file counts, and not double the ciphertext.
    #[tokio::test]
    async fn swept_at_complete_does_not_double_count_progress() {
        let s = Scratch::new("complete404-progress");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        let sid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, sid, size, mtime);
        let mut mock = Mock::default();
        mock.live.insert(sid.to_string());
        mock.complete_gone.insert(sid.to_string());
        let state: Shared = Arc::new(Mutex::new(mock));
        let progress = RecordingProgress::default();

        let out = run_with_progress(&state, spec(&path, Uuid::new_v4()), &progress)
            .await
            .expect("upload must recover");

        assert_eq!(
            progress.files_done.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one logical file finished once, not twice (finish(true) must not fire before complete() succeeds)"
        );
        assert_eq!(
            progress.overall_bytes.load(std::sync::atomic::Ordering::Relaxed),
            out.ciphertext_bytes,
            "the failed attempt's fully-confirmed bytes must be rolled back, not left under the retry's own total"
        );
    }

    /// Codex thread #2, the chunk-PUT trigger: the resumed session accepts
    /// its FIRST chunk, then goes gone on the second — a genuine MID-pipeline
    /// sweep with a nonzero partial byte count reported before the failure.
    /// Needs >1 chunk, so the payload crosses the 4 MiB minimum chunk size.
    #[tokio::test]
    async fn swept_mid_pipeline_does_not_double_count_progress() {
        let s = Scratch::new("midchunk-progress");
        let (path, size, mtime) = s.file(5 * 1024 * 1024);
        let stored_fid = Uuid::new_v4();
        let dead_sid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, dead_sid, size, mtime);
        let mut mock = Mock::default();
        mock.live.insert(dead_sid.to_string());
        mock.dies_after_chunks.insert(dead_sid.to_string(), 1);
        let state: Shared = Arc::new(Mutex::new(mock));
        let progress = RecordingProgress::default();

        let out = run_with_progress(&state, spec(&path, Uuid::new_v4()), &progress)
            .await
            .expect("upload must recover");

        {
            let m = state.lock().unwrap();
            let dead_chunks: Vec<u32> = m
                .chunk_puts
                .iter()
                .filter(|c| c.0 == dead_sid.to_string())
                .map(|c| c.1)
                .collect();
            assert!(
                dead_chunks.len() >= 2,
                "premise: the dead session must take a successful chunk 0 before dying on chunk 1: {dead_chunks:?}"
            );
        }
        assert_eq!(
            progress.files_done.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one logical file finished once"
        );
        assert_eq!(
            progress.overall_bytes.load(std::sync::atomic::Ordering::Relaxed),
            out.ciphertext_bytes,
            "chunk 0's bytes from the failed attempt must be rolled back before the retry re-sends everything from 0"
        );
    }

    #[tokio::test]
    async fn reinit_happens_at_most_once_per_file_per_run() {
        let s = Scratch::new("once");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        let dead_sid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, dead_sid, size, mtime);
        // Every session, including the re-inited one, is swept at once.
        let state: Shared = Arc::new(Mutex::new(Mock {
            kill_new_sessions: true,
            ..Default::default()
        }));

        let res = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run(&state, spec(&path, Uuid::new_v4())),
        )
        .await
        .expect("must not loop");
        assert!(res.is_err(), "a second swept session is an error, not a retry");
        let m = state.lock().unwrap();
        assert_eq!(m.inits.len(), 1, "exactly one re-init: {:?}", m.inits);
        assert!(m.completed.is_empty());
        // The dead original record was cleared; the one left is the new
        // session's, so the next run gets its own single re-init.
        let left = crate::resume::resumable_upload(&path, size, mtime);
        assert!(left.is_some_and(|(_, sid)| sid != dead_sid), "left: {left:?}");
    }

    #[tokio::test]
    async fn refused_stored_id_falls_back_to_one_fresh_id() {
        // 404 is the ONE status the v2 init handler returns that PROVES the
        // stored id is unusable (routes/uploads.rs: an explicit file_id the
        // caller can no longer read at all) — see `fresh_id_can_fix`. It is
        // the only status this fallback still fires on after Codex thread #1
        // (a 400 or a 5xx no longer does; see the two tests below).
        let s = Scratch::new("fallback");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, Uuid::new_v4(), size, mtime);
        let mut mock = Mock::default();
        mock.init_refuse.insert(stored_fid.to_string(), 404);
        let state: Shared = Arc::new(Mutex::new(mock));
        let fresh_fid = Uuid::new_v4();

        let out = run(&state, spec(&path, fresh_fid))
            .await
            .expect("fresh id must succeed");
        let m = state.lock().unwrap();
        assert_eq!(m.inits, vec![stored_fid.to_string(), fresh_fid.to_string()]);
        assert_eq!(out.server_id, fresh_fid);
        assert_eq!(m.completed.len(), 1);
    }

    /// Codex thread #1: a 400 from the stored-id init is GENERIC request
    /// validation (file name / chunk-plan shape) that a fresh id would fail
    /// identically — it says nothing about the id being bad, so (unlike 404)
    /// it must be surfaced, never used to justify a fresh-id fallback.
    #[tokio::test]
    async fn refused_stored_id_400_is_surfaced_not_papered_over() {
        let s = Scratch::new("400-surfaced");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, Uuid::new_v4(), size, mtime);
        let mut mock = Mock::default();
        mock.init_refuse.insert(stored_fid.to_string(), 400);
        let state: Shared = Arc::new(Mutex::new(mock));

        let res = run(&state, spec(&path, Uuid::new_v4())).await;
        assert!(res.is_err(), "a 400 must be surfaced, not silently worked around");
        let m = state.lock().unwrap();
        assert_eq!(
            m.inits,
            vec![stored_fid.to_string()],
            "no fresh-id fallback for a 400: {:?}",
            m.inits
        );
    }

    /// Codex thread #1: a 5xx from the stored-id init proves nothing about the
    /// id — it can be transient — so it must be retried with the SAME id
    /// (bounded, with backoff), never swapped for a fresh one.
    #[tokio::test]
    async fn stored_id_5xx_is_retried_with_same_id_then_succeeds() {
        let s = Scratch::new("5xx-retry-succeeds");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, Uuid::new_v4(), size, mtime);
        let mut mock = Mock::default();
        // Fails with 500 twice, then succeeds on the third attempt — well
        // within STORED_ID_INIT_RETRIES.
        mock.init_fail_then_succeed.insert(stored_fid.to_string(), (500, 2));
        let state: Shared = Arc::new(Mutex::new(mock));

        let out = run(&state, spec(&path, Uuid::new_v4()))
            .await
            .expect("must succeed once the transient 5xx clears, on the SAME id");
        let m = state.lock().unwrap();
        assert_eq!(
            m.inits,
            vec![stored_fid.to_string(); 3],
            "every retry must use the STORED id, never a fresh one: {:?}",
            m.inits
        );
        assert_eq!(out.server_id, stored_fid);
        assert_eq!(m.completed.len(), 1);
    }

    /// Codex thread #1 follow-up (found reviewing the fix at 3cb4daf): a
    /// `--replace` upload's stored id EQUALS `spec.file_id` (both are the
    /// existing target file's id — there is no OTHER id a replace could use),
    /// so `fallback_file_id` is always `None` for it. The bounded SAME-id 5xx
    /// retry must not be gated on a fallback id being available, or a
    /// replace's re-init surfaces the FIRST transient 5xx instead of riding
    /// it out like every other swept-session recovery does.
    #[tokio::test]
    async fn replace_upload_5xx_is_retried_with_same_id_even_with_no_fallback_available() {
        let s = Scratch::new("replace-5xx-retry");
        let (path, size, mtime) = s.file(64 * 1024);
        let target_fid = Uuid::new_v4();
        crate::resume::record(&path, target_fid, Uuid::new_v4(), size, mtime);
        let mut mock = Mock::default();
        mock.init_fail_then_succeed.insert(target_fid.to_string(), (500, 2));
        let state: Shared = Arc::new(Mutex::new(mock));

        // SAME id as the stored one — a replace can never fall back to a
        // fresh id, since it must land on this exact existing file.
        let mut replace_spec = spec(&path, target_fid);
        replace_spec.base_version_number = Some(3);

        let out = run(&state, replace_spec)
            .await
            .expect("a transient 5xx on a replace's stored-id re-init must be retried, not surfaced on the first one");
        let m = state.lock().unwrap();
        assert_eq!(
            m.inits,
            vec![target_fid.to_string(); 3],
            "every retry must reuse the SAME id — a replace has no fresh id to fall back to: {:?}",
            m.inits
        );
        assert_eq!(out.server_id, target_fid);
    }

    /// The bound: once every retry is spent, a persistent 5xx is surfaced —
    /// still never falling back to a fresh id (which could leave a live
    /// session under the stored id and complete a duplicate file under the
    /// fresh one, per the review comment).
    #[tokio::test]
    async fn stored_id_5xx_exhausts_retries_and_is_surfaced() {
        let s = Scratch::new("5xx-exhausted");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, Uuid::new_v4(), size, mtime);
        let mut mock = Mock::default();
        mock.init_refuse.insert(stored_fid.to_string(), 503);
        let state: Shared = Arc::new(Mutex::new(mock));

        let res = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run(&state, spec(&path, Uuid::new_v4())),
        )
        .await
        .expect("bounded retries must not hang");
        assert!(res.is_err(), "a persistent 5xx must eventually be surfaced");
        let m = state.lock().unwrap();
        assert_eq!(
            m.inits,
            vec![stored_fid.to_string(); 1 + super::STORED_ID_INIT_RETRIES as usize],
            "exactly the initial attempt plus the bounded retries, all on the STORED id: {:?}",
            m.inits
        );
        assert!(m.completed.is_empty());
    }

    #[tokio::test]
    async fn conflict_on_reinit_is_not_papered_over_with_a_fresh_id() {
        let s = Scratch::new("conflict");
        let (path, size, mtime) = s.file(64 * 1024);
        let stored_fid = Uuid::new_v4();
        crate::resume::record(&path, stored_fid, Uuid::new_v4(), size, mtime);
        let mut mock = Mock::default();
        mock.init_refuse.insert(stored_fid.to_string(), 409);
        let state: Shared = Arc::new(Mutex::new(mock));

        let res = run(&state, spec(&path, Uuid::new_v4())).await;
        assert!(res.is_err());
        let m = state.lock().unwrap();
        assert_eq!(
            m.inits,
            vec![stored_fid.to_string()],
            "no duplicate file under a fresh id"
        );
        // The dead record is gone either way.
        assert_eq!(crate::resume::resumable_upload(&path, size, mtime), None);
    }

    #[tokio::test]
    async fn a_session_this_run_opened_is_not_reinited() {
        let s = Scratch::new("fresh");
        let (path, _size, _mtime) = s.file(64 * 1024);
        let state: Shared = Arc::new(Mutex::new(Mock {
            kill_new_sessions: true,
            ..Default::default()
        }));
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            run(&state, spec(&path, Uuid::new_v4())),
        )
        .await
        .expect("must not loop");
        assert!(res.is_err());
        assert_eq!(
            state.lock().unwrap().inits.len(),
            1,
            "no re-init for a session this run opened"
        );
    }

    #[test]
    fn session_gone_classifier() {
        use crate::api::{ApiError, is_upload_session_gone};
        let e = |status: u16, msg: &str| ApiError {
            code: Some(msg.to_string()),
            message: msg.to_string(),
            status,
        };
        assert!(is_upload_session_gone(&e(404, "not found")));
        assert!(is_upload_session_gone(&e(
            400,
            "upload session is not writable: expired"
        )));
        assert!(!is_upload_session_gone(&e(
            400,
            "upload session is not writable: completed"
        )));
        assert!(!is_upload_session_gone(&e(
            400,
            "chunk index 9 out of range (expected 0..3)"
        )));
        assert!(!is_upload_session_gone(&e(
            409,
            "upload is already in progress for this file"
        )));
        assert!(!is_upload_session_gone(&e(500, "internal")));
        assert!(!is_upload_session_gone(&e(0, "Can't reach the Beebeeb API")));
    }

    /// Codex thread #1: 404 is the ONLY status that proves the stored id is
    /// unusable (see `fresh_id_can_fix`'s doc comment for why each other
    /// status is excluded, grounded in the v2 init handler).
    #[test]
    fn fresh_id_can_fix_classifier() {
        use super::fresh_id_can_fix;
        assert!(fresh_id_can_fix(404));
        assert!(
            !fresh_id_can_fix(400),
            "generic validation — a fresh id fails identically"
        );
        assert!(!fresh_id_can_fix(401), "auth — never a reason to change ids");
        assert!(!fresh_id_can_fix(403), "auth — never a reason to change ids");
        assert!(
            !fresh_id_can_fix(409),
            "a live upload or stale base version — a fresh id would create a duplicate"
        );
        assert!(!fresh_id_can_fix(402), "quota");
        assert!(!fresh_id_can_fix(413), "quota");
        assert!(!fresh_id_can_fix(429), "rate limiting, handled in `api`");
        assert!(
            !fresh_id_can_fix(500),
            "a 5xx proves nothing about the id — retried with the SAME id instead"
        );
        assert!(
            !fresh_id_can_fix(503),
            "a 5xx proves nothing about the id — retried with the SAME id instead"
        );
    }
}
