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

/// Look-ahead channel depth. `1` keeps peak memory tight while still
/// overlapping "encrypt N+1" with "PUT N"; bump to `2` for a touch more
/// overlap at the cost of one extra chunk resident.
pub(super) const CHANNEL_CAP: usize = 1;

/// Thumbnails are best-effort. To honour the constant-memory promise we never
/// read more than this much of a file just to make one.
pub(super) const MAX_THUMBNAIL_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Per-chunk AEAD overhead on the wire: `nonce(12) + tag(16)`.
pub(super) const CHUNK_OVERHEAD: u64 = 28;

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
            // Abandon BEFORE the record goes, and drop it only once the session
            // is gone (or never existed). A transient failure keeps the record
            // and fails this file for this run: an upload now would overwrite
            // the record (one per path), and nothing would end that session.
            match api.upload_abandon(&stale.file_id.to_string()).await {
                Ok(_) => {}
                Err(e) if e.status == 404 => {}
                Err(e) => {
                    return Err(format!(
                        "{}: could not end the interrupted upload ({}); it is tried again next run",
                        display_name(&spec.file_name),
                        e.message
                    )
                    .into());
                }
            }
            if !crate::ui::is_quiet() {
                eprintln!(
                    "  {}: the interrupted upload cannot be continued by this upload; starting it again",
                    display_name(&spec.file_name)
                );
            }
        }
        crate::resume::clear(&spec.path);
    }
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
                },
                SessionResume::RestartWithFreshSession => {
                    crate::resume::clear(&spec.path);
                    if let Err(e) = api.upload_abandon(&pending.file_id.to_string()).await
                        && e.status != 404
                        && !crate::ui::is_quiet()
                    {
                        // The init below then meets the live lease (409) and fails:
                        // a later run retries. Never falls back to the old session.
                        eprintln!(
                            "  {}: could not abandon the interrupted upload ({})",
                            display_name(&spec.file_name),
                            e.message
                        );
                    }
                    if !crate::ui::is_quiet() {
                        eprintln!(
                            "  {}: the interrupted upload cannot be continued in this chunk format; starting it again",
                            display_name(&spec.file_name)
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
    loop {
        match upload_attempt(api, &master_key, &spec, size, mtime_ns, plan, direct_given_up, progress).await {
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
                    return Err(msg.into());
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
                    resume_tag: tag_for(file_id),
                };
            }
            Err(AttemptErr::DirectUnavailable { bytes_confirmed }) => {
                // The direct session takes no proxy PUT, and the init below would meet its live lease: end
                // it first, like a session that cannot be continued (`RestartWithFreshSession`). Once per
                // file per run: `direct_given_up` keeps this file's re-init off the direct route, and
                // `note_direct_upload_failed` keeps every later NEW session off it for the trust period.
                let (SessionPlan::Resume { file_id, .. } | SessionPlan::Init { file_id, .. }) = plan;
                direct_given_up = true;
                api.note_direct_upload_failed();
                if bytes_confirmed > 0 {
                    progress.rollback(bytes_confirmed);
                }
                crate::resume::clear(&spec.path);
                if let Err(e) = api.upload_abandon(&file_id.to_string()).await
                    && e.status != 404
                    && !crate::ui::is_quiet()
                {
                    eprintln!(
                        "  {}: could not end the direct upload session ({})",
                        display_name(&spec.file_name),
                        e.message
                    );
                }
                if !crate::ui::is_quiet() {
                    eprintln!(
                        "  {}: the direct upload is unavailable for this file; sending it through the API",
                        display_name(&spec.file_name)
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
            Err(e) => return Err(e.into_message().into()),
        }
    }
}

/// One attempt: build the encryptor for `plan`'s file id, open (or reuse) the
/// session, stream every chunk, complete. Consumes nothing the retry needs —
/// the encryptor is rebuilt per attempt because the file key derives from the
/// file id. `direct_given_up`: a direct upload of THIS file already had to be given up in this run, so a new
/// session must not ask for one again.
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
) -> Result<UploadOutcome, AttemptErr> {
    let (file_id, resumed_session, resumed_direct, record_resume_tag) = match plan {
        SessionPlan::Resume {
            file_id,
            session_id,
            direct,
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
    //    profile with a concurrency-aware chunk size: parallel `bb sync` uploads
    //    emit smaller chunks (memory budget), while `bb push` (concurrency 1)
    //    gets the full Cli 128 MiB cap. This same plan is sent to the v2 init so
    //    the server frames the file exactly as the encryptor emits it.
    let chunk_size =
        beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, spec.concurrency.max(1)).chunk_size_bytes;
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
    let (upload_session_id, direct) = if let Some(sid) = resumed_session {
        let direct = if resumed_direct {
            match api.direct_upload_offer().await {
                DirectOffer::Offered(discovery) => Some(discovery),
                // A definite no (forced off, or the server does not offer the direction any more): a direct
                // session takes no proxy PUT, so it is given up.
                DirectOffer::NotOffered => return Err(AttemptErr::DirectUnavailable { bytes_confirmed: 0 }),
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
        (sid.to_string(), direct)
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
            crate::resume::record_with_transport(
                &spec.path,
                proposed(spec, file_id),
                record_resume_tag,
                sid,
                format,
                size,
                mtime_ns,
                session_direct,
            );
        }
        let direct = if session_direct { offer } else { None };
        (init.upload_session_id, direct)
    };

    // 6. Run the look-ahead pipeline. Every chunk is PUT to the session; on a
    //    resumed session the server returns `{skipped:true}` for chunks it
    //    already holds, so re-PUTs are cheap and correct.
    let file_prog = progress.begin_file(&spec.file_name, expected_total);
    let result = run_pipeline(
        api,
        &upload_session_id,
        direct.as_ref(),
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
    let completed = match api.upload_complete_typed(&upload_session_id).await {
        Ok(v) => v,
        Err(e) => {
            // Every chunk was confirmed (we only reach here on `result == Ok(())`,
            // which `run_pipeline` only returns once `confirmed_bytes ==
            // expected_total`) — so a swept-session retry must roll back exactly
            // `expected_total`, the full amount THIS attempt reported.
            file_prog.finish(false);
            return Err(session_err(None, e, expected_total));
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
    )
    .await;

    Ok(UploadOutcome {
        server_id: file_id,
        plaintext_bytes: size,
        ciphertext_bytes: expected_total,
        transport: if direct.is_some() { "direct" } else { "api" },
    })
}

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

/// The producer/consumer core. Returns `Ok(())` only when every planned chunk
/// was emitted, server-confirmed, and the byte totals reconcile.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_pipeline(
    api: &ApiClient,
    upload_session_id: &str,
    direct: Option<&beebeeb_core::direct_transfer::Discovery>,
    encryptor: ChunkEncryptor,
    chunk_count: u32,
    expected_total: u64,
    shutdown: &Arc<AtomicBool>,
    file_prog: &dyn FileProgress,
) -> Result<(), AttemptErr> {
    use tokio::sync::mpsc;

    let upload_id = crate::api::DirectUploadId::from(upload_session_id);
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
        // Task 2173: a direct session sends the chunk grant -> PUT to storage -> commit; an API session
        // PUTs it to the API as it always did. Either way the chunk counts only once the server holds it.
        let stored = match direct {
            Some(discovery) => {
                match crate::direct_upload::upload_chunk(
                    api,
                    discovery,
                    &upload_id,
                    index,
                    Bytes::from(chunk.data),
                    shutdown,
                )
                .await
                {
                    Ok(()) => Ok(()),
                    Err(crate::direct_upload::DirectUploadError::UseApi) => {
                        consumer_err = Some(AttemptErr::DirectUnavailable {
                            bytes_confirmed: confirmed_bytes,
                        });
                        break;
                    }
                    Err(crate::direct_upload::DirectUploadError::Interrupted) => {
                        consumer_err = Some(AttemptErr::Other(INTERRUPTED.to_string()));
                        break;
                    }
                    Err(crate::direct_upload::DirectUploadError::SessionGone(message)) => {
                        // The same recovery as a swept session on the proxy route (task 1589).
                        consumer_err = Some(AttemptErr::SessionGone {
                            message: format!("chunk {index} upload: {message}"),
                            bytes_confirmed: confirmed_bytes,
                        });
                        break;
                    }
                    Err(crate::direct_upload::DirectUploadError::Terminal { status, message }) => {
                        Err(crate::api::ApiError {
                            code: None,
                            message,
                            status,
                        })
                    }
                }
            }
            None => api
                .upload_chunk_typed(upload_session_id, index, Bytes::from(chunk.data))
                .await
                .map(|_| ()),
        };
        match stored {
            Ok(()) => {
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
) {
    let is_image = mime.map(|m| m.starts_with("image/")).unwrap_or(false);
    if !is_image || size > MAX_THUMBNAIL_SOURCE_BYTES {
        return;
    }
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

/// Expected ciphertext for a file of `size` under `ChunkProfile::Cli` at the
/// given upload `concurrency`: `size + 28 · chunk_count`. Used by callers to
/// size the overall progress bar without opening the file — matches the
/// concurrency-aware chunk plan the upload driver actually emits.
pub fn expected_ciphertext_for(size: u64, concurrency: u32) -> u64 {
    let plan = beebeeb_types::plan_chunks_concurrent(size, ChunkProfile::Cli, concurrency.max(1));
    size + CHUNK_OVERHEAD * plan.chunk_count
}

#[cfg(test)]
pub(super) mod rss_regression;

/// Task 1589: a resumed upload whose server session was swept (lease expired)
/// must drop its `pending-uploads.json` record, re-init ONCE (stored
/// `file_id` first, a fresh one only if the server refuses it), and upload from
/// chunk 0 — never PUT to the dead session forever, never loop.
#[cfg(test)]
pub(super) mod swept_session_reinit;
