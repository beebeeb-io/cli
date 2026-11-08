//! Upload session selection and recovery: resume a recorded session, or re-init
//! it once when the server has swept it, and classify which failures a fresh id
//! could actually fix.
//!
//! Owns: `SessionPlan`, `AttemptErr`, `session_err`, `fresh_id_can_fix`, and the
//! bounded same-id retry constants.
//! Must not: PUT chunks or own the channel (see `upload::pipeline`).
//! Tests: the swept-session and classifier cases live in
//! `upload::pipeline`'s `swept_session_reinit`.

use super::*;

/// How one upload attempt obtains its v2 session.
#[derive(Clone, Copy, Debug)]
pub(super) enum SessionPlan {
    /// Reuse a session recorded by an interrupted earlier run (skip init). `direct`: the session was
    /// opened as a direct upload (task 2173) and must be continued as one.
    Resume {
        file_id: Uuid,
        session_id: Uuid,
        direct: bool,
        /// The chunk size the session was opened with, when its record says (`--parallel`, spec §4.3).
        recorded_chunk_size: Option<u64>,
    },
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
        /// R4-C: the core resume tag that belongs to `file_id`. A sidecar
        /// record must always pair an id with ITS tag, never the current
        /// spec's tag next to another id.
        resume_tag: Option<[u8; 32]>,
    },
}

/// Why one attempt failed, kept typed so the driver can react to exactly the
/// two recoverable cases and nothing else.
pub(super) enum AttemptErr {
    /// The server no longer has the upload session (task 1589: its lease
    /// expired and the sweeper removed it). Recoverable once per file per run by
    /// a re-init. `confirmed` is what THIS attempt had already reported done
    /// (`FileProgress::chunk_done`) before it died: its bytes, chunks and
    /// already-stored chunks. The retry restarts at chunk 0 under a fresh
    /// progress handle, so the caller must roll this back on the run's counts
    /// before looping, or a full resend double-counts it (Codex thread #2).
    SessionGone { message: String, confirmed: Tally },
    /// The init for the stored `file_id` was refused in a way a fresh id can
    /// fix. Carries the fresh id to try (at most once).
    InitRefused { message: String, fresh_file_id: Uuid },
    /// Spec §5.2: a resumed session finished in another folder (or its file is gone), so this file is uploaded
    /// again under a fresh id, in the folder asked for. The record is already cleared and the line already
    /// printed; the driver inits `fresh_file_id` from chunk 0, with the tag it was minted with (`None` for a
    /// replace, which keeps the listed row's id).
    RestartFresh {
        fresh_file_id: Uuid,
        resume_tag: Option<[u8; 32]>,
    },
    /// Task 2173: the session was opened as a direct upload and cannot finish as one (the server turned
    /// the direction off, a node has no direct routes, a link was refused, storage kept failing). Its
    /// transport cannot change, so the driver abandons it and inits again WITHOUT the capability token,
    /// once per file per run. `confirmed` is rolled back like for [`AttemptErr::SessionGone`].
    DirectUnavailable { confirmed: Tally },
    /// Spec §5.2: a resumed session the server reports already completed (a chunk write answers `400 ... not
    /// writable: completed`). Not a failure: the file is finished. The driver calls `complete` (which answers
    /// `already_completed`) and clears the record, re-creating the file only if the server's row says it finished
    /// elsewhere or is gone.
    SessionCompleted { confirmed: Tally },
    /// LD-0137: the SOURCE FILE broke the attempt, not the upload: it could not be read as planned (core's
    /// integrity check on a file that grew or shrank, a failed read). Never leaves `upload_attempt`: it asks the
    /// caller whether the file changed (`ChunkProgress::before_complete`) and reports that if so, else the
    /// message as [`AttemptErr::Other`]. Other failures (the network, the server's answers, Ctrl-C) never take
    /// this route, whatever the file did meanwhile.
    Source(String),
    /// Anything else — surfaced to the caller unchanged.
    Other(String),
}

impl AttemptErr {
    pub(super) fn into_message(self) -> String {
        match self {
            AttemptErr::SessionGone { message, .. }
            | AttemptErr::InitRefused { message, .. }
            | AttemptErr::Source(message)
            | AttemptErr::Other(message) => message,
            AttemptErr::DirectUnavailable { .. } => "the direct upload is unavailable for this file".to_string(),
            AttemptErr::SessionCompleted { .. } => "the upload session is already completed".to_string(),
            AttemptErr::RestartFresh { .. } => "restarting this file under a fresh id".to_string(),
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
/// same text the caller always saw. `confirmed` is what this attempt had
/// already gotten past the server before `e` — see [`AttemptErr::SessionGone`].
pub(super) fn session_err(prefix: Option<String>, e: crate::api::ApiError, confirmed: Tally) -> AttemptErr {
    let gone = crate::api::is_upload_session_gone(&e);
    let text = match prefix {
        Some(p) => format!("{p}: {e}"),
        None => e.message,
    };
    if gone {
        AttemptErr::SessionGone {
            message: text,
            confirmed,
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
pub(super) fn fresh_id_can_fix(status: u16) -> bool {
    status == 404
}

/// How many extra attempts a `5xx` from the STORED-id recovery init gets, with
/// the SAME id, before it is surfaced — never replaced with a fresh id (task
/// 1589, Codex thread #1). Exponential backoff starting at
/// `STORED_ID_INIT_BACKOFF_BASE`, doubling each attempt.
pub(super) const STORED_ID_INIT_RETRIES: u32 = 3;

pub(super) const STORED_ID_INIT_BACKOFF_BASE: Duration = Duration::from_millis(150);
