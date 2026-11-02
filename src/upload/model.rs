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
    /// Number of files uploaded in parallel by the caller (sync `--concurrency`;
    /// `1` for `bb push`/single-file paths). Threaded into the concurrency-aware
    /// chunk plan so parallel uploads emit smaller chunks to stay in the memory
    /// budget.
    pub concurrency: u32,
    /// Shared cancellation flag (set by the Ctrl-C handler).
    pub shutdown: Arc<AtomicBool>,
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
    /// Total ciphertext uploaded (`plaintext + 28 * chunk_count`).
    pub ciphertext_bytes: u64,
    /// How the chunks reached the server (task 2173): `"direct"` (grant -> object storage -> commit) or
    /// `"api"` (the chunk PUT route). A direct session that was given up reports the route that finished
    /// the file, `"api"`.
    pub transport: &'static str,
}
