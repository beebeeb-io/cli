//! The upload contract: what one upload needs ([`UploadSpec`]) and what it
//! reports back ([`UploadOutcome`]), plus the writer-pin and target-id rules
//! that decide which chunk format a new file gets.
//!
//! Owns: `UploadSpec`/`UploadOutcome`, `INTERRUPTED`, `writer_pin`,
//! `replacing`, `proposed`.
//! Must not: perform I/O or drive the pipeline (see `upload::pipeline`).
//! Tests: the pin cases and the caller-facing contract are exercised in
//! `upload::pipeline`'s `swept_session_reinit`.

use super::*;

/// Sentinel error returned when an upload is cancelled (Ctrl-C / shutdown
/// flag). Callers compare against this to count "remaining" rather than
/// reporting a failure. Mirrors the existing `__rate_limited__` convention.
pub const INTERRUPTED: &str = "__interrupted__";

/// Everything needed to upload one file.
pub struct UploadSpec {
    /// Local path to read from.
    pub path: PathBuf,
    /// Display + name-encryption filename (may be suffixed for keep-both).
    pub file_name: String,
    /// The file UUID. A fresh random id for a new file, or the existing id for
    /// a server-side version replace (`bb push --replace`).
    pub file_id: Uuid,
    /// Task 2275 L13/N4: the core resume tag minted with a NEW `file_id`
    /// (`None` for a replace). Persisted with the resume record so a resumed
    /// create can prove the id came from core's mint before continuing it.
    pub resume_tag: Option<[u8; 32]>,
    /// For a `bb push --replace`: the existing file's current `version_number`,
    /// passed to the v2 init as `base_version_number` so the server enforces an
    /// optimistic concurrency check (stale-version 409) and bumps the version.
    /// `None` for a fresh upload (the server then keys off `file_id` absence /
    /// presence to decide replace vs. new).
    pub base_version_number: Option<i32>,
    /// For a replace: the EXISTING file's read pin, from its encrypted metadata
    /// (`crypto::content_pin`, task 1943), so a v2-pinned file is replaced as v2
    /// (the server keeps its v2 metadata). Ignored for a new file: see
    /// [`writer_pin`].
    pub replace_pin: Option<ExpectedChunkFormat>,
    /// Existing item key; only legal with an existing ID/base version and recorded pin.
    pub replace_file_key: Option<Arc<beebeeb_core::kdf::FileKey>>,
    /// Destination folder, or `None` for the vault root.
    pub parent_id: Option<Uuid>,
    /// The concurrency a NEW session's chunk plan follows (`plan_chunks_concurrent`): `bb sync`'s files at
    /// once (`--concurrency`), `bb push`'s chunks at once (`--parallel`). A higher value plans smaller chunks,
    /// so the buffers of what runs at once stay within the memory budget.
    pub concurrency: u32,
    /// `--parallel` (spec §4.3): the concurrency a resumed session whose record names NO chunk size (one written
    /// before `--parallel`) is planned at, which must be what this caller planned at before that change: `bb
    /// push` 1, `bb sync` its own `--concurrency`. Kept apart from `concurrency` so the rule cannot drift with
    /// `--parallel`.
    pub unrecorded_resume_concurrency: u32,
    /// How many chunks of THIS file may be in flight at once, before the budget lowers it (`bb push --parallel`;
    /// `1` for `bb sync`, which runs whole files in parallel instead). The driver takes `used = min(this, budget
    /// fit)`.
    pub chunks_in_flight: u32,
    /// The byte budget this file's chunk-sized buffers are charged to (spec §4.2). `bb sync` gets a FRESH
    /// `ChunkLimits::single()` per file (one shared across its parallel files could deadlock: two pipelines each
    /// holding a read buffer and waiting for a seal); `bb push` gets one for the whole push.
    pub limits: Arc<crate::upload::limits::ChunkLimits>,
    /// One awaitable interrupt for the whole push (spec §5.1): a Ctrl-C drops the chunks in flight and ends a
    /// rate-limit pause at once. `None` for `bb sync`, which keeps its own `shutdown` flag and its uninterruptible
    /// 429 wait.
    pub interrupt: Option<crate::interrupt::Interrupt>,
    /// Shared cancellation flag (set by the Ctrl-C handler).
    pub shutdown: Arc<AtomicBool>,
    /// Task 2334: the interrupted upload's row as the caller just read it, so the driver does not read it
    /// again. `None`: the driver reads it when it needs it.
    pub read_row: Option<ReadRow>,
}

/// An upload's row as `GET /files/{id}` answered it moments ago: its id and the folder it is in (`None` = the
/// root).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRow {
    pub file_id: Uuid,
    pub parent_id: Option<Uuid>,
}

/// The pin this upload's writer follows (task 1943; core
/// `chunk_format_for_pin`). A NEW file never has one: it is written in core's
/// `WRITE_FORMAT` and its metadata records nothing, so the new-file path never
/// hands `ExpectedChunkFormat::V2` to `ChunkEncryptor::with_recorded_pin`, even
/// if a caller filled in `replace_pin`. Only a replace (`base_version_number` is
/// `Some`) takes the existing file's pin.
pub(super) fn writer_pin(spec: &UploadSpec) -> ExpectedChunkFormat {
    match (spec.base_version_number, spec.replace_pin) {
        (Some(_), Some(pin)) => pin,
        _ => ExpectedChunkFormat::Unrecorded,
    }
}

/// L13 (SF-0001): the listed row a replace versions (`base_version_number` is
/// `Some`), `None` for a create. A replace proposes that row's id; a create, an
/// id its caller minted.
pub(super) fn replacing(spec: &UploadSpec) -> Option<Uuid> {
    spec.base_version_number.map(|_| spec.file_id)
}

/// L13 (SF-0001): what an attempt under `file_id` proposes, which decides how
/// every echo is checked ([`crate::item_id::Proposed`]).
pub(super) fn proposed(spec: &UploadSpec, file_id: Uuid) -> crate::item_id::Proposed {
    match replacing(spec) {
        Some(_) => crate::item_id::Proposed::Listed(file_id),
        None => crate::item_id::Proposed::Minted(file_id),
    }
}

/// Result of a completed upload.
#[derive(Debug)]
pub struct UploadOutcome {
    /// Server-confirmed file id.
    pub server_id: Uuid,
    /// Plaintext size (bytes).
    pub plaintext_bytes: u64,
    /// The version this upload completed, as the completion answer names it (server
    /// `routes/uploads/complete.rs`). `None` when the answer does not say: the idempotent "already completed"
    /// answer to a repeated completion carries no version.
    pub version_number: Option<i64>,
    /// Total ciphertext uploaded (`plaintext + 28 * chunk_count`).
    pub ciphertext_bytes: u64,
    /// How the chunks reached the server (task 2173): `"direct"` (grant -> object storage -> commit) or
    /// `"api"` (the chunk PUT route). A direct session that was given up reports the route that finished
    /// the file, `"api"`.
    pub transport: &'static str,
    /// Task 2333: the ciphertext of the chunks this file's attempts sent and had confirmed, summed over every
    /// attempt (a chunk re-sent after a swept session counts each time; a chunk the server already held, none;
    /// one chunk retried over HTTP counts once, when confirmed).
    pub transferred_bytes: u64,
}
