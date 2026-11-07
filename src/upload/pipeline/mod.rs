//! The streaming upload driver: one producer task encrypts chunks into a
//! bounded channel while the async consumer PUTs them, so peak memory follows
//! the chunk size rather than the file size.
//!
//! Owns: `stream_encrypt_upload`, `upload_attempt`, the producer/consumer
//! pipeline, thumbnail upload, and the expected-ciphertext estimate.
//! Must not: choose a progress sink or a session plan (see `upload::progress`
//! and `upload::session`).
//! Tests: `rss_regression` (constant-memory bound) and `swept_session_reinit`
//! (session recovery, writer pin) in the sibling files.

use super::*;
use crate::api::InitTransport;
use crate::upload::limits::used_in_flight;

mod parallel;
use parallel::run_pipeline;

/// Look-ahead channel depth. `1` keeps peak memory tight while still
/// overlapping "encrypt N+1" with "PUT N"; bump to `2` for a touch more
/// overlap at the cost of one extra chunk resident.
pub(super) const CHANNEL_CAP: usize = 1;

/// Thumbnails are best-effort. To honour the constant-memory promise we never
/// read more than this much of a file just to make one.
pub(super) const MAX_THUMBNAIL_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Per-chunk AEAD overhead on the wire: `nonce(12) + tag(16)`.
pub(super) use crate::transfer_stats::CHUNK_OVERHEAD;

/// Chunks of ONE file on the wire at once. The consumer loop in [`run_pipeline`] awaits each chunk's
/// transfer before it takes the next one off the channel (`rx.recv()`), so a file sends one chunk at a
/// time while the producer encrypts the next (`CHANNEL_CAP`). `bb sync` runs `concurrency` files at once:
/// its run-wide limit is `RunLimit`'s `min(concurrency, Σ)` of this over the files in flight. Test `push_sends_one_chunk_at_a_time`
/// measures the peak through the real loop, so a change to the loop has to change this constant.
pub(crate) const CHUNKS_IN_FLIGHT_PER_FILE: u32 = 1;

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
) -> Result<UploadOutcome, UploadError> {
    if spec.replace_file_key.is_some() && (spec.base_version_number.is_none() || spec.replace_pin.is_none()) {
        return Err("explicit replacement key requires an existing ID, base version and pin".into());
    }
    // Cheap cancellation path for files that never started.
    if spec.shutdown.load(Ordering::Relaxed) {
        return Err(UploadError::Interrupted);
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
    //
    //     Core's resume rule (task 1908 round 3, P1-A) decides first: the old
    //     session may only be continued if its frames and the new encryptor's
    //     are both v1. Otherwise (a v2 writer, or a record that does not say its
    //     format) the session is abandoned — never PUT to again — and the SAME
    //     file id is inited fresh and uploaded from chunk 0.
    let writing = beebeeb_core::chunk_stream::chunk_format_for_pin(writer_pin(&spec));
    // L13 (SF-0001): a create continues only a record of an id this client
    // minted, a replace only this CLI's replace record of the row it lists
    // (`resume::resumable_by`). A record for another destination never
    // redirects this write.
    let replacing = replacing(&spec);
    let pending = crate::resume::resumable_upload_for(&spec.path, size, mtime_ns, replacing);
    // Any other record for this path (same size and mtime) is one this write may
    // not continue: a pre-update CLI's, one whose marker names another id, or
    // another write's. It is never resumed. Runs once per file per run.
    if let Some(stale) = crate::resume::unresumable_upload(&spec.path, size, mtime_ns, replacing) {
        // A create ends it: its session would otherwise hold its lease. A
        // replace ends it only when it names the row this replace lists (that
        // row's live lease 409s every retry of this replacement, L7a); a record
        // under another identity is another file's session and is only
        // discarded, never abandoned.
        if replacing.is_none_or(|row| stale.file_id == row) {
            end_interrupted(
                api,
                &spec,
                progress,
                stale.file_id,
                "the interrupted upload cannot be continued by this upload; starting it again",
            )
            .await?;
        } else {
            crate::resume::clear(&spec.path);
        }
    }
    // `--parallel` (spec §4.3): a record whose chunk size is outside core's ladder (above the Cli cap, under the
    // 4 MiB floor, or 0) was written by no CLI (a damaged or edited record); framing against it would send bodies
    // no session of this CLI accepts, or fail the encryptor on every run with the record never cleared. Ended
    // like a record this upload cannot continue; the file starts again.
    let pending = match pending {
        Some(p)
            if p.chunk_size_bytes
                .is_some_and(|c| !(crate::upload::limits::MIN_CHUNK..=ChunkProfile::Cli.static_cap()).contains(&c)) =>
        {
            end_interrupted(
                api,
                &spec,
                progress,
                p.file_id,
                "the interrupted upload's chunk size is not one this CLI writes; starting it again",
            )
            .await?;
            None
        }
        other => other,
    };
    // Task 2334: the server fixes a new file's parent when its upload is inited and keeps it at completion, so a
    // continued session completes in the folder its FIRST attempt named. A create continues only an upload going
    // to the folder this write asks for; one going elsewhere is ended (its row, listed and counted in the quota
    // until then, is deleted with it) and this file starts again here.
    let pending = match pending {
        Some(p) if replacing.is_none() => resumable_here(api, &spec, progress, p).await?,
        other => other,
    };
    // R4-C: a sidecar record pairs an id with the tag core minted for THAT id.
    // `spec.resume_tag` belongs to `spec.file_id`; a pending id keeps the tag
    // it was recorded with, or (on a mismatch) is abandoned and the current
    // spec id is used instead.
    let pending_id = pending.as_ref().map(|p| p.file_id);
    let pending_tag = pending.as_ref().and_then(|p| p.resume_tag);
    let tag_for = |file_id: uuid::Uuid| {
        if Some(file_id) == pending_id {
            pending_tag
        } else {
            spec.resume_tag
        }
    };
    let mut plan = match pending {
        Some(pending) => {
            // L13/N4: a create's record carries the core resume tag. Continue
            // the id only when core still resumes it with that tag; a record
            // without one (a replace, or an older CLI) keeps the id-only gate.
            let tag_resumes = pending.resume_tag.is_none_or(|tag| {
                beebeeb_core::item_id::NewItemId::resume(&master_key, &pending.file_id.to_string(), &tag).is_ok()
            });
            let resume = if tag_resumes {
                beebeeb_core::chunk_stream::resume_policy(pending.chunk_format, writing)
            } else {
                SessionResume::RestartWithFreshSession
            };
            match resume {
                SessionResume::ContinueSession => SessionPlan::Resume {
                    file_id: pending.file_id,
                    session_id: pending.session_id,
                    direct: pending.direct,
                    recorded_chunk_size: pending.chunk_size_bytes,
                },
                SessionResume::RestartWithFreshSession => {
                    crate::resume::clear(&spec.path);
                    if let Err(e) = api.upload_abandon(&pending.file_id.to_string()).await
                        && e.status != 404
                        && !crate::ui::is_quiet()
                    {
                        // The init below then meets the live lease (409) and fails:
                        // a later run retries. Never falls back to the old session.
                        progress.note(
                            &format!(
                                "  {}: could not abandon the interrupted upload ({})",
                                display_name(&spec.file_name),
                                e.message
                            ),
                            Stream::Stderr,
                        );
                    }
                    if !crate::ui::is_quiet() {
                        progress.note(
                            &format!(
                                "  {}: the interrupted upload cannot be continued in this chunk format; starting it again",
                                display_name(&spec.file_name)
                            ),
                            Stream::Stderr,
                        );
                    }
                    // R4-C: a mismatched tag proves nothing about the pending
                    // id, so start from the id this run minted, with its own tag.
                    let restart_id = if tag_resumes { pending.file_id } else { spec.file_id };
                    SessionPlan::Init {
                        file_id: restart_id,
                        fallback_file_id: (tag_resumes && pending.file_id != spec.file_id).then_some(spec.file_id),
                        is_recovery: true,
                        resume_tag: tag_for(restart_id),
                    }
                }
            }
        }
        None => SessionPlan::Init {
            file_id: spec.file_id,
            fallback_file_id: None,
            is_recovery: false,
            resume_tag: spec.resume_tag,
        },
    };
    // This file's direct upload was given up in this run: its re-init (and nothing after it) asks for the API.
    // `note_direct_upload_failed` remembers the same fact for every LATER file for the trust period, but a
    // memory with an expiry cannot be what bounds this loop, so the file keeps its own bit.
    let mut direct_given_up = false;
    // Every attempt's sent chunks, for the file's speed: a chunk re-sent after a failed attempt crossed the
    // wire twice (spec §4.3: progress is net of rollbacks, speed is not).
    let mut transferred: u64 = 0;
    // The lowered-N line (spec §4.4) is said once per file, whatever the attempts.
    let mut lowered_noted = false;
    loop {
        match upload_attempt(
            api,
            &master_key,
            &spec,
            size,
            mtime_ns,
            plan,
            direct_given_up,
            progress,
            &mut transferred,
            &mut lowered_noted,
        )
        .await
        {
            Ok(outcome) => return Ok(outcome),
            Err(AttemptErr::SessionGone {
                message: msg,
                confirmed,
            }) => {
                // Only a RESUMED session is re-inited, and a re-init always
                // yields an `Init` plan — so this arm runs at most once per
                // file per run (the loop bound). A session this run opened
                // itself vanishing is not the case this recovery is for.
                let SessionPlan::Resume { file_id, .. } = plan else {
                    return Err(msg.into());
                };
                // The dead attempt's chunk_done reports must not survive
                // into the retry's own count (Codex thread #2): the retry gets
                // a BRAND NEW `FileProgress` from `begin_file_chunks` and re-sends
                // every chunk from 0, so anything left on the shared/overall
                // counter from this failed attempt would be double-counted
                // once the retry's own confirmations land on top of it.
                if confirmed.chunks > 0 {
                    progress.rollback_tally(&spec.file_name, confirmed);
                }
                // The recorded session is dead: drop it so neither this run nor
                // a later one ever PUTs to it again.
                crate::resume::clear(&spec.path);
                if !crate::ui::is_quiet() {
                    progress.note(
                        &format!(
                            "  {}: the interrupted upload expired on the server; starting it again",
                            display_name(&spec.file_name)
                        ),
                        Stream::Stderr,
                    );
                }
                plan = SessionPlan::Init {
                    file_id,
                    fallback_file_id: (file_id != spec.file_id).then_some(spec.file_id),
                    is_recovery: true,
                    resume_tag: tag_for(file_id),
                };
            }
            Err(AttemptErr::DirectUnavailable { confirmed }) => {
                // The direct session takes no proxy PUT, and the init below would meet its live lease: end
                // it first, like a session that cannot be continued (`RestartWithFreshSession`). Once per
                // file per run: `direct_given_up` keeps this file's re-init off the direct route, and
                // `note_direct_upload_failed` keeps every later NEW session off it for the trust period.
                let (SessionPlan::Resume { file_id, .. } | SessionPlan::Init { file_id, .. }) = plan;
                direct_given_up = true;
                api.note_direct_upload_failed();
                if confirmed.chunks > 0 {
                    progress.rollback_tally(&spec.file_name, confirmed);
                }
                crate::resume::clear(&spec.path);
                if let Err(e) = api.upload_abandon(&file_id.to_string()).await
                    && e.status != 404
                    && !crate::ui::is_quiet()
                {
                    progress.note(
                        &format!(
                            "  {}: could not end the direct upload session ({})",
                            display_name(&spec.file_name),
                            e.message
                        ),
                        Stream::Stderr,
                    );
                }
                if !crate::ui::is_quiet() {
                    progress.note(
                        &format!(
                            "  {}: the direct upload is unavailable for this file; sending it through the API",
                            display_name(&spec.file_name)
                        ),
                        Stream::Stderr,
                    );
                }
                plan = SessionPlan::Init {
                    file_id,
                    fallback_file_id: (file_id != spec.file_id).then_some(spec.file_id),
                    is_recovery: true,
                    resume_tag: tag_for(file_id),
                };
            }
            Err(AttemptErr::InitRefused { fresh_file_id, .. }) => {
                plan = SessionPlan::Init {
                    file_id: fresh_file_id,
                    fallback_file_id: None,
                    is_recovery: false,
                    resume_tag: spec.resume_tag,
                };
            }
            // Spec §5.2: the resumed session finished elsewhere or is gone; upload this file again here under a
            // fresh id, from chunk 0 (the record was already cleared, the line already printed). Once per file:
            // the new session is not a resume, so it never comes back to this arm.
            Err(AttemptErr::RestartFresh {
                fresh_file_id,
                resume_tag,
            }) => {
                plan = SessionPlan::Init {
                    file_id: fresh_file_id,
                    fallback_file_id: None,
                    is_recovery: false,
                    resume_tag,
                };
            }
            Err(e) => return Err(e.into_message().into()),
        }
    }
}

/// Task 2334: may this create continue `pending`, the interrupted upload of the same local file? Only when its
/// row is in the folder this write asks for (`spec.parent_id`): the server set that row's parent at the first
/// attempt's init, and completion keeps it. `None`: start this file afresh.
///
/// The server's row decides when it answers: it is where completion puts the file, and the web app can move a
/// row in either direction while its upload waits. When the server cannot say, the parent recorded when the
/// session was opened decides; a record from before 2334 has none, so the record is kept and this file fails
/// for this run. No login, or a refused one, is said in the words the rest of the CLI uses.
async fn resumable_here(
    api: &ApiClient,
    spec: &UploadSpec,
    progress: &dyn ChunkProgress,
    pending: crate::resume::PendingUpload,
) -> Result<Option<crate::resume::PendingUpload>, UploadError> {
    api.require_auth().map_err(|e| UploadError::from(e.message))?;
    let name = display_name(&spec.file_name);
    let answer = match spec.read_row.filter(|read| read.file_id == pending.file_id) {
        // The caller read this row moments ago (`bb push`, checking the same-name file is its own upload).
        Some(read) => RowAnswer::In(read.parent_id),
        None => read_row_parent(api, pending.file_id).await?,
    };
    let server_parent = match answer {
        // No row: its session is gone too, and the swept-session recovery (task 1589) re-inits the stored id,
        // which creates the row anew under the parent this write sends. Nothing can complete elsewhere.
        RowAnswer::Gone => return Ok(Some(pending)),
        RowAnswer::In(parent) => Ok(parent),
        RowAnswer::Unknown(why) => Err(why),
    };
    match (server_parent, pending.destination) {
        (Ok(parent), _) if parent == spec.parent_id => Ok(Some(pending)),
        (Ok(_), _) => end_elsewhere(api, spec, progress, pending).await,
        (Err(_), Some(recorded)) if recorded == spec.parent_id => Ok(Some(pending)),
        (Err(_), Some(_)) => end_elsewhere(api, spec, progress, pending).await,
        (Err(e), None) => Err(format!(
            "{name}: could not check which folder the interrupted upload was going to ({e}); \
             its progress is kept: run the command again"
        )
        .into()),
    }
}

/// The folder a `GET /files/{id}` row is in (`None` = the root), or why the row does not say. The one reading
/// of `parent_id` for the push path (`commands::push_repeat` too).
pub(crate) fn row_parent(row: &serde_json::Value) -> Result<Option<Uuid>, String> {
    match row.get("parent_id") {
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(parent)) => Uuid::parse_str(parent)
            .map(Some)
            .map_err(|_| "the answer's folder id is not an id".to_string()),
        _ => Err("the answer names no folder".to_string()),
    }
}

/// What `GET /files/{id}` says about an interrupted upload's row.
enum RowAnswer {
    /// The folder it is in (`None` = the root).
    In(Option<Uuid>),
    /// There is no such row.
    Gone,
    /// The answer cannot say where it is.
    Unknown(String),
}

/// Ask the server where `file_id`'s row is. A refused login is the one error, in the CLI's usual words.
async fn read_row_parent(api: &ApiClient, file_id: Uuid) -> Result<RowAnswer, UploadError> {
    Ok(match api.get_file(&file_id.to_string()).await {
        Ok(row) => match row_parent(&row) {
            Ok(parent) => RowAnswer::In(parent),
            Err(why) => RowAnswer::Unknown(why),
        },
        Err(e) if e.status == 404 => RowAnswer::Gone,
        Err(e) if e.status == 401 => return Err(e.message.into()),
        Err(e) => RowAnswer::Unknown(e.message),
    })
}

/// End `pending`, an interrupted upload going to another folder, so this file starts again in the folder this
/// write asks for.
async fn end_elsewhere(
    api: &ApiClient,
    spec: &UploadSpec,
    progress: &dyn ChunkProgress,
    pending: crate::resume::PendingUpload,
) -> Result<Option<crate::resume::PendingUpload>, UploadError> {
    end_interrupted(
        api,
        spec,
        progress,
        pending.file_id,
        "the interrupted upload was going to another folder; starting it again here",
    )
    .await?;
    Ok(None)
}

/// End the interrupted upload `file_id` of `spec`'s file by the one explicit route
/// (`POST /files/{id}/upload/abandon`, which deletes a never-completed first upload's row: until then it is listed
/// and counted in the quota), then drop its record and say `why_again` (unless quiet) through `progress`, so the
/// line goes around live transfer lines (task 2333, spec §6.7).
///
/// Abandon BEFORE the record goes, and drop it only once the session is gone (or never existed: a 404). A
/// failure keeps the record and fails this file for this run: an upload now would overwrite the record (one per
/// path), and nothing would end that session or its row.
async fn end_interrupted(
    api: &ApiClient,
    spec: &UploadSpec,
    progress: &dyn ChunkProgress,
    file_id: Uuid,
    why_again: &str,
) -> Result<(), UploadError> {
    let name = display_name(&spec.file_name);
    match api.upload_abandon(&file_id.to_string()).await {
        Ok(_) => {}
        Err(e) if e.status == 404 => {}
        Err(e) => {
            return Err(format!(
                "{name}: could not end the interrupted upload ({}); it is tried again next run",
                e.message
            )
            .into());
        }
    }
    crate::resume::clear(&spec.path);
    if !crate::ui::is_quiet() {
        progress.note(&format!("  {name}: {why_again}"), Stream::Stderr);
    }
    Ok(())
}

/// One attempt: build the encryptor for `plan`'s file id, open (or reuse) the
/// session, stream every chunk, complete. Consumes nothing the retry needs —
/// the encryptor is rebuilt per attempt because the file key derives from the
/// file id. `direct_given_up`: a direct upload of THIS file already had to be given up in this run, so a new
/// session must not ask for one again. `transferred` sums the bytes every attempt of this file sent.
#[allow(clippy::too_many_arguments)]
pub(super) async fn upload_attempt(
    api: &ApiClient,
    master_key: &Arc<MasterKey>,
    spec: &UploadSpec,
    size: u64,
    mtime_ns: u128,
    plan: SessionPlan,
    direct_given_up: bool,
    progress: &dyn ChunkProgress,
    transferred: &mut u64,
    lowered_noted: &mut bool,
) -> Result<UploadOutcome, AttemptErr> {
    let (file_id, resumed_session, resumed_direct, record_resume_tag) = match plan {
        SessionPlan::Resume {
            file_id,
            session_id,
            direct,
            ..
        } => (file_id, Some(session_id), direct, None),
        SessionPlan::Init {
            file_id, resume_tag, ..
        } => (file_id, None, false, resume_tag),
    };
    let file_id_str = file_id.to_string();

    let mime = beebeeb_core::media::guess_mime_type(&spec.file_name);
    let is_media = beebeeb_core::media::is_media(mime);
    let pin = writer_pin(spec);

    // 3. Open the file and build the streaming encryptor (derives the file key
    //    once; the encryptor owns it and is `Send`). The CLI uses the Cli
    //    profile with a concurrency-aware chunk size: what runs at once (sync's
    //    files, push's chunks) emits smaller chunks (memory budget). This same
    //    plan is sent to the v2 init so the server frames the file exactly as
    //    the encryptor emits it. A resumed session keeps the size it was opened
    //    with (`--parallel`, spec §4.3): the plan is the session's, not the run's.
    let chunk_size = match plan {
        SessionPlan::Resume {
            recorded_chunk_size, ..
        } => chunk_size_for(
            size,
            spec.concurrency,
            spec.unrecorded_resume_concurrency,
            Some(recorded_chunk_size),
        ),
        SessionPlan::Init { .. } => chunk_size_for(size, spec.concurrency, spec.unrecorded_resume_concurrency, None),
    };
    // How many chunks of THIS file go at once: what the caller asked, lowered so every chunk-sized buffer fits
    // the 640 MiB budget (spec §4.2). A lowered value says so, once, at the file's start (spec §4.4).
    let used = used_in_flight(spec.chunks_in_flight, chunk_size);
    progress.set_limit(used);
    if used < spec.chunks_in_flight.max(1) && !crate::ui::is_quiet() && !*lowered_noted {
        *lowered_noted = true;
        // Binary MiB, to match the "640 MiB upload buffer" it is weighed against (spec §4.4), not `human_size`'s
        // decimal MB.
        progress.note(
            &format!(
                "    {}: {used} chunks at once, not {}: its {} MiB chunks fill the 640 MiB upload buffer",
                display_name(&spec.file_name),
                spec.chunks_in_flight,
                chunk_size / (1024 * 1024),
            ),
            Stream::Stderr,
        );
    }
    // The attempt's admission (spec §4.2, §7): the read buffer's share, taken before the encryptor is built and
    // held for its whole life, and a reserve of one seal (R + 2 C ≤ 384 MiB, so a single file never waits). Across
    // the files of a folder push this is where an attempt waits, holding nothing, until it fits the one budget.
    let admission = spec.limits.admit(chunk_size).await;
    // A Ctrl-C while this attempt waited for the budget: nothing is opened (no init goes out after it).
    if spec.shutdown.load(Ordering::Relaxed) {
        return Err(AttemptErr::Other(INTERRUPTED.to_string()));
    }
    let file = std::fs::File::open(&spec.path).map_err(|e| format!("open {}: {e}", spec.path.display()))?;
    let encryptor = match &spec.replace_file_key {
        Some(key) => ChunkEncryptor::from_reader_with_file_key(
            crate::crypto::key_copy(key),
            &file_id_str,
            size,
            chunk_size,
            file,
        ),
        None => ChunkEncryptor::from_reader_with_chunk_size(master_key, &file_id_str, size, chunk_size, file),
    }
    .and_then(|e| e.with_recorded_pin(pin))
    .map_err(|e| format!("init encryptor for {}: {e}", spec.file_name))?;
    let chunk_count = encryptor.chunk_plan().chunk_count as u32;
    let expected_total = encryptor.expected_total_ciphertext();
    let format = encryptor.format();

    // 3b. Encrypt the name + MIME envelope, recording the format the content is
    //     written in (core's rule: `recorded_for(encryptor.format())` — nothing
    //     for v1, `chunk_format: 2` for a v2 replace). The name key derives from
    //     the DURABLE file_id, so a replace re-encrypts the name under the same id
    //     the server already stores.
    let name_encrypted = match &spec.replace_file_key {
        Some(key) => beebeeb_core::encrypt::encrypt_name_for(
            key,
            &spec.file_name,
            mime,
            ExpectedChunkFormat::recorded_for(format),
        ),
        None => {
            let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
            beebeeb_core::encrypt::encrypt_name_for(
                &file_key,
                &spec.file_name,
                mime,
                ExpectedChunkFormat::recorded_for(format),
            )
        }
    }
    .map_err(|e| format!("encrypt name: {e}"))?;

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
    //    `upload_session_id` keys the chunk PUTs + complete. L13: the durable
    //    file id is the one this client proposed; init must echo it exactly.
    //
    //    Task 2173: when the server offers the direct upload (discovery says `object-upload-v1`; not forced
    //    off with `BB_TRANSPORT=api`; none given up for this file in this run or in this process within the
    //    trust period), init asks for it with the capability header, and the answer's `transport` says what
    //    the server chose. `direct` is the discovery answer the chunk loop checks every link against, `None`
    //    for an API session.
    //
    //    The capabilities are asked only when the answer is used: a resumed API session has no use for it (and a
    //    slow capabilities route costs up to 15 s a try).
    let (upload_session_id, direct, init_pool) = if let Some(sid) = resumed_session {
        let direct = if resumed_direct {
            match api.direct_upload_offer().await {
                DirectOffer::Offered(discovery) => Some(discovery),
                // A definite no (forced off, or the server does not offer the direction any more): a direct
                // session takes no proxy PUT, so it is given up.
                DirectOffer::NotOffered => {
                    return Err(AttemptErr::DirectUnavailable {
                        confirmed: Tally::default(),
                    });
                }
                // The login is not valid (no session, or the lookup was refused with a 401/403): the way out is
                // `bb login`, not `--transport api`, and the session and its record are kept (a 50 GB push
                // killed at 90 % would start again from 0). Each says what it was, in the words the rest of the
                // CLI uses for it.
                DirectOffer::Unauthorized(refusal) => {
                    return Err(AttemptErr::Other(
                        refusal.resume_message(&display_name(&spec.file_name)),
                    ));
                }
                // Nobody answered. That says nothing about the server, and giving the session up throws away
                // every chunk it already holds: keep the session and its record, fail this file for this run.
                DirectOffer::Unknown => {
                    return Err(AttemptErr::Other(format!(
                        "{}: the interrupted direct upload cannot be continued right now, because the server's \
                         transfer capabilities could not be read. Its progress is kept: run the command again, \
                         or use --transport api to send the file through the API instead",
                        display_name(&spec.file_name)
                    )));
                }
            }
        } else {
            None
        };
        (sid.to_string(), direct, None)
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
        // A NEW session asks for the direct upload only when it is offered, this file's own direct upload was not
        // already given up in this run, and none was given up in this process within the trust period: when direct
        // is broken server-wide, each further file would pay an init, its tries, an abandon and a second init. (A
        // file already in flight when the first one gives up still pays it: the bound is the files in flight.)
        let offer = if direct_given_up || api.direct_upload_failed_recently() {
            None
        } else {
            api.direct_upload_offer().await.discovery()
        };
        let is_stored_id_recovery = matches!(plan, SessionPlan::Init { is_recovery: true, .. });
        let mut retries_used = 0u32;
        let init = loop {
            match api
                .upload_init_for_transport(
                    Some(file_id),
                    &name_encrypted,
                    spec.parent_id,
                    size as i64,
                    chunk_size as i64,
                    chunk_count as i32,
                    is_media,
                    spec.base_version_number,
                    if offer.is_some() {
                        InitTransport::Direct
                    } else {
                        InitTransport::Api
                    },
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
        // L13 (SF-0001): the answer must echo the id THIS client minted. A
        // different id means the server would store the content under another
        // item, whose derived key someone else may hold — abort before any
        // chunk goes out, any name is sealed under it or it is used as a parent.
        if let Err(e) = proposed(spec, file_id).verify(Some(&init.file_id)) {
            // The session this attempt just opened is not a session for our
            // item: end it (best effort, by the id WE sent — never the echoed
            // one, which may name another item) so it cannot hold a lease over
            // our id. Nothing is recorded: the record is written below.
            let _ = api.upload_abandon(&file_id_str).await;
            return Err(AttemptErr::Other(e.to_string()));
        }
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
        let session_direct = init.is_direct();
        if let Ok(sid) = init.upload_session_id.parse::<Uuid>() {
            crate::resume::record_upload(
                &spec.path,
                proposed(spec, file_id),
                record_resume_tag,
                sid,
                format,
                size,
                mtime_ns,
                session_direct,
                spec.parent_id,
                chunk_size,
            );
        }
        let direct = if session_direct { offer } else { None };
        (init.upload_session_id, direct, init.pool)
    };

    // 6. Run the look-ahead pipeline. Every chunk is PUT to the session; on a
    //    resumed session the server returns `{skipped:true}` for chunks it
    //    already holds, so re-PUTs are cheap and correct.
    let file_prog = progress.begin_file_chunks(&spec.file_name, expected_total, chunk_count, chunk_size);
    // A new session's pool is named by its init answer; a resumed one's by its completion (below).
    if let Some(label) = init_pool {
        file_prog.pool(label);
    }
    let result = run_pipeline(
        api,
        &upload_session_id,
        direct.as_ref(),
        encryptor,
        chunk_count,
        expected_total,
        &spec.shutdown,
        &spec.limits,
        used,
        chunk_size,
        admission,
        spec.interrupt.as_ref(),
        file_prog.as_ref(),
        transferred,
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
    let confirmed = match result {
        Ok(confirmed) => confirmed,
        // Spec §5.2: a resumed session the server reports already completed is finished, not failed. `complete`
        // answers `already_completed` and the file id; the server's row decides whether the file is done here,
        // done in another folder (upload it again here, leaving the finished one), or gone (upload it again).
        Err(AttemptErr::SessionCompleted { confirmed }) => {
            let name = display_name(&spec.file_name);
            let completed = match api.upload_complete_typed(&upload_session_id).await {
                Ok(v) => v,
                Err(e) => {
                    file_prog.finish(false);
                    return Err(session_err(None, e, Tally::default()));
                }
            };
            // L13: the completion must name the item this client minted (as the normal path checks).
            if let Err(e) = proposed(spec, file_id).verify(crate::item_id::echoed_id(&completed)) {
                let _ = api.upload_abandon(&file_id_str).await;
                file_prog.finish(false);
                return Err(AttemptErr::Other(e.to_string()));
            }
            let outcome = || UploadOutcome {
                server_id: file_id,
                plaintext_bytes: size,
                version_number: completed.get("version_number").and_then(serde_json::Value::as_i64),
                ciphertext_bytes: expected_total,
                transport: if direct.is_some() { "direct" } else { "api" },
                transferred_bytes: *transferred,
            };
            // A replace versions the row it echoed: the file is done.
            if replacing(spec).is_some() {
                file_prog.finish(true);
                crate::resume::clear(&spec.path);
                return Ok(outcome());
            }
            // A create: the server's row says where the finished upload landed.
            let here_or_restart = match read_row_parent(api, file_id).await {
                Ok(RowAnswer::In(parent)) if parent == spec.parent_id => {
                    file_prog.finish(true);
                    crate::resume::clear(&spec.path);
                    return Ok(outcome());
                }
                Ok(RowAnswer::In(_)) => "the interrupted upload finished in another folder; uploading it again here",
                Ok(RowAnswer::Gone) => "the interrupted upload finished, but its file is gone; uploading it again",
                Ok(RowAnswer::Unknown(why)) => {
                    // The server cannot say: keep the record and fail this file for this run (2334's words).
                    file_prog.finish(false);
                    return Err(AttemptErr::Other(format!(
                        "{name}: could not check which folder the interrupted upload finished in ({why}); \
                         its progress is kept: run the command again"
                    )));
                }
                Err(e) => {
                    file_prog.finish(false);
                    return Err(AttemptErr::Other(e.to_string()));
                }
            };
            // The finished file is left as it is (never abandoned, never moved); the record goes and this file
            // starts again under this run's own fresh id, in the folder asked for. What this attempt counted done
            // is taken back first: the new attempt counts from chunk 0 (as a swept session's retry does).
            if confirmed.chunks > 0 {
                progress.rollback_tally(&spec.file_name, confirmed);
            }
            crate::resume::clear(&spec.path);
            if !crate::ui::is_quiet() {
                progress.note(&format!("  {name}: {here_or_restart}"), Stream::Stderr);
            }
            file_prog.finish(false);
            // A push that resumes a row it found by name passes that row's id as its own, with no tag
            // (`commands::push`): then the fresh id is minted here, with its tag, as a new file's is. Otherwise
            // the run's own minted id is free to use.
            let (fresh_file_id, resume_tag) = if spec.file_id == file_id {
                let minted = crate::item_id::mint_owner(master_key).map_err(|e| AttemptErr::Other(e.to_string()))?;
                (minted.id(), Some(*minted.tag()))
            } else {
                (spec.file_id, spec.resume_tag)
            };
            return Err(AttemptErr::RestartFresh {
                fresh_file_id,
                resume_tag,
            });
        }
        Err(e) => {
            file_prog.finish(false);
            return Err(e);
        }
    };

    // 6b. LD-0113: the caller may stop a file that changed while it was read;
    //     its session is abandoned before anything completes.
    if let Err(why) = progress.before_complete(&spec.path) {
        let _ = api.upload_abandon(&file_id_str).await;
        crate::resume::clear(&spec.path);
        file_prog.finish(false);
        return Err(AttemptErr::Other(why));
    }

    // 7. Finalise the version.
    let completed = match api.upload_complete_typed(&upload_session_id).await {
        Ok(v) => v,
        Err(e) => {
            // Every chunk was confirmed (`run_pipeline` returns `Ok` only once
            // its tally is `expected_total` bytes in `chunk_count` chunks) — so a
            // swept-session retry must roll back exactly that: everything THIS
            // attempt reported, already-stored chunks included.
            file_prog.finish(false);
            return Err(session_err(None, e, confirmed));
        }
    };
    // L13 (SF-0001): the completion answer must name the same item this client
    // minted — before the thumbnail is sealed under it or the outcome reports it.
    if let Err(e) = proposed(spec, file_id).verify(crate::item_id::echoed_id(&completed)) {
        // The session answered another item: end it (best effort, by the id WE
        // proposed — the server may already hold the content under it, and an
        // abandon of a completed upload is a no-op). The resume record is KEPT
        // (L5): it names this client's own id, so the next attempt continues or
        // re-inits under that same id. Dropping it would make the next run mint
        // a fresh id and upload the file again beside a copy the server may
        // already hold — a duplicate on every pass of a daemon. Nothing is
        // sealed under the echoed id either way.
        let _ = api.upload_abandon(&file_id_str).await;
        file_prog.finish(false);
        return Err(AttemptErr::Other(e.to_string()));
    }
    if resumed_session.is_some()
        && let Some(label) = PoolLabel::from_answer(&completed)
    {
        file_prog.pool(label);
    }
    file_prog.finish(true);

    // 7b. Upload done — drop the resume record so a future upload of this path
    //     (e.g. a changed version) starts fresh rather than resuming this id.
    crate::resume::clear(&spec.path);

    // 8. Thumbnails: image/* only, one bounded extra read, best-effort.
    maybe_upload_thumbnail(
        api,
        master_key,
        spec.replace_file_key.as_deref(),
        &file_id_str,
        &file_id_str,
        &spec.path,
        mime,
        size,
        pin,
        &spec.limits,
    )
    .await;

    Ok(UploadOutcome {
        server_id: file_id,
        plaintext_bytes: size,
        version_number: completed.get("version_number").and_then(serde_json::Value::as_i64),
        ciphertext_bytes: expected_total,
        transport: if direct.is_some() { "direct" } else { "api" },
        transferred_bytes: *transferred,
    })
}

/// Generate + encrypt + upload a thumbnail for image files, best-effort. Does
/// exactly one extra bounded read of the source; never holds key material in
/// the CLI beyond the derived per-file key.
#[allow(clippy::too_many_arguments)]
pub(super) async fn maybe_upload_thumbnail(
    api: &ApiClient,
    master_key: &MasterKey,
    explicit_key: Option<&beebeeb_core::kdf::FileKey>,
    file_id_str: &str,
    server_id: &str,
    path: &Path,
    mime: Option<&str>,
    size: u64,
    pin: ExpectedChunkFormat,
    limits: &Arc<crate::upload::limits::ChunkLimits>,
) {
    let is_image = mime.map(|m| m.starts_with("image/")).unwrap_or(false);
    if !is_image || size > MAX_THUMBNAIL_SOURCE_BYTES {
        return;
    }
    // Spec §7: one at a time across the files of a push, the source read charged to the budget at its cap. The
    // decoder's own working memory is outside the budget, as in `bb sync`.
    let _turn = limits.thumbnail_turn(MAX_THUMBNAIL_SOURCE_BYTES).await;
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    let derived;
    let file_key = if let Some(key) = explicit_key {
        key
    } else {
        derived = beebeeb_core::kdf::derive_file_key(master_key, file_id_str.as_bytes());
        &derived
    };

    if let Some(thumb) = crate::thumbnail::generate_from_file(&bytes, mime) {
        // Same pin rule as the content and the name.
        if let Ok(enc) = seal_thumbnail(file_key, &thumb.data, pin) {
            let _ = api.upload_thumbnail(server_id, enc).await;
        }
    }
    if let Some(large) = crate::thumbnail::generate_large_from_file(&bytes, mime) {
        if let Ok(enc) = seal_thumbnail(file_key, &large.data, pin) {
            let _ = api.upload_thumbnail_large(server_id, enc).await;
        }
    }
}

/// Seal a thumbnail under the file's recorded pin. Core's non-deprecated
/// thumbnail sealers take a `NewItemId` or a `WriteTarget`; the CLI's resolver
/// cannot issue a `WriteTarget` for legacy rows, so the pinned case keeps the
/// key-based writer (the deprecation note allows it for rows core has not
/// resolved).
#[allow(deprecated)]
fn seal_thumbnail(
    file_key: &beebeeb_core::kdf::FileKey,
    data: &[u8],
    pin: ExpectedChunkFormat,
) -> Result<Vec<u8>, beebeeb_core::CoreError> {
    if pin == ExpectedChunkFormat::Unrecorded {
        beebeeb_core::encrypt::encrypt_thumbnail(file_key, data)
    } else {
        beebeeb_core::encrypt::encrypt_thumbnail_for(file_key, data, pin)
    }
}

/// The chunk size the driver frames a file of `size` with (`--parallel`, spec §4.3): a new session plans at
/// `concurrency`; a resumed one keeps the size its record names, and a record from before this change (no size)
/// is planned at `unrecorded_resume_concurrency`, what the caller planned at then. `pending`: the record of the
/// file, `Some(None)` for one that names no size. The one rule for the driver and for the push's plan
/// (`commands::push_report`), so the run's totals match the chunks sent.
pub fn planned_chunk_size(
    size: u64,
    concurrency: u32,
    unrecorded_resume_concurrency: u32,
    pending: Option<&crate::resume::PendingUpload>,
) -> u64 {
    chunk_size_for(
        size,
        concurrency,
        unrecorded_resume_concurrency,
        pending.map(|p| p.chunk_size_bytes),
    )
}

/// [`planned_chunk_size`] on the resumed session's recorded size (`Some(recorded)`) or none (a new session).
fn chunk_size_for(
    size: u64,
    concurrency: u32,
    unrecorded_resume_concurrency: u32,
    resumed: Option<Option<u64>>,
) -> u64 {
    let plan_at = |concurrency: u32| {
        beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, concurrency.max(1)).chunk_size_bytes
    };
    match resumed {
        Some(Some(recorded)) => recorded,
        Some(None) => plan_at(unrecorded_resume_concurrency),
        None => plan_at(concurrency),
    }
}

/// The ciphertext and chunk count of a file of `size` framed at `chunk_size` (an empty file is one empty chunk):
/// what the driver sends for a session of that geometry, so a command's totals match a resumed file's
/// (`planned_chunk_size`), not only a fresh plan's.
pub fn expected_at_chunk_size(size: u64, chunk_size: u64) -> (u64, u32) {
    let chunks = if size == 0 { 1 } else { size.div_ceil(chunk_size.max(1)) };
    (size + CHUNK_OVERHEAD * chunks, chunks as u32)
}

/// The chunk count the upload driver emits for a file of `size` at `concurrency` (`plan_chunks_concurrent`, the
/// Cli profile). Since `--parallel` the push plans through `planned_chunk_size`; this states a
/// test's premise ("16 chunks of 4 MiB").
#[cfg(test)]
pub fn expected_chunks_for(size: u64, concurrency: u32) -> u32 {
    beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, concurrency.max(1)).chunk_count as u32
}

#[cfg(test)]
pub(super) mod rss_regression;

/// Task 1589: a resumed upload whose server session was swept (lease expired)
/// must drop its `pending-uploads.json` record, re-init ONCE (stored
/// `file_id` first, a fresh one only if the server refuses it), and upload from
/// chunk 0 — never PUT to the dead session forever, never loop.
#[cfg(test)]
pub(super) mod swept_session_reinit;

/// Task 2334: an interrupted create continues only into the folder this write asks for.
#[cfg(test)]
mod resume_destination;

/// Task 2333: what the driver tells its progress sink (chunks in flight, retries, pools, rollbacks).
#[cfg(test)]
mod live_stats_tests;

/// `--parallel` Task 1: at one chunk at a time, today's requests in today's order (the golden).
#[cfg(test)]
pub(crate) mod request_log_tests;

/// `--parallel` Task 3 (spec §4.3): a resumed session keeps the chunk size it was opened with.
#[cfg(test)]
mod resume_geometry_tests;

/// `--parallel` Task 5 (spec §4.2, §5, §6.2): up to `used` chunks of a file at once, the lowered-N line, the
/// interrupt, out-of-order resume and a chunk that fails for good.
#[cfg(test)]
mod parallel_tests;

/// `--parallel` Task 5 (spec §4.5): the real heap peak of the whole pipeline matches the budget table.
#[cfg(test)]
mod memory_tests;

/// `--parallel` Task 5 (spec §5.2): a resumed session the server reports already completed is finished.
#[cfg(test)]
mod completed_session_tests;
