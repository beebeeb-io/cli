//! The sync pass itself: the control modes, the one-shot run and the watch handoff.

use super::*;
use crate::upload::ChunkProgress;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    local_dir: Option<PathBuf>,
    remote_path_arg: Option<String>,
    command: SyncCommand,
    options: SyncOptions,
) -> Result<(), String> {
    let SyncOptions {
        dry_run,
        force,
        delete_remote,
        once,
        rehash,
        allow_mass_delete,
        rebind,
        concurrency,
        verbose,
    } = options;
    // ── --status / --stop-session / --stop-all: session control ────────────
    match &command {
        SyncCommand::StopSession(name) => {
            let api = ApiClient::from_config();
            api.require_auth()?;
            let master_key = load_master_key()?;
            return stop_session_by_name(&api, &master_key, name).await;
        }
        SyncCommand::StopAll => {
            let api = ApiClient::from_config();
            api.require_auth()?;
            let master_key = load_master_key()?;
            return stop_all_sessions(&api, &master_key).await;
        }
        SyncCommand::Status => {
            let api = ApiClient::from_config();
            api.require_auth()?;
            let master_key = load_master_key()?;
            return print_session_status(&api, &master_key).await;
        }
        _ => {}
    }

    // If no local_dir and no control flags, show status + hint (Task 4)
    let local_dir = match local_dir {
        Some(d) => d,
        None => {
            let api = ApiClient::from_config();
            api.require_auth()?;
            let master_key = load_master_key()?;
            print_session_status(&api, &master_key).await?;
            println!();
            println!(
                "  {} {}",
                "hint".custom_color(crate::colors::INK_DIM),
                "Run bb sync <local> <remote> to start a new sync".custom_color(crate::colors::INK_DIM),
            );
            return Ok(());
        }
    };

    match command {
        SyncCommand::StopLegacy => {
            // Legacy --stop: try to uninstall any old single-plist LaunchAgent
            return crate::daemon::uninstall_launchagent("sync")
                .map_err(String::from)
                .map(|removed| {
                    if removed {
                        println!(
                            "  {} sync daemon removed",
                            "\u{2713}".custom_color(crate::colors::GREEN_OK),
                        );
                    } else {
                        println!(
                            "  {} no legacy sync daemon installed",
                            "\u{00b7}".custom_color(crate::colors::INK_DIM),
                        );
                    }
                });
        }
        SyncCommand::Daemon => return spawn_sync_daemon(&local_dir, remote_path_arg.as_deref()),
        _ => {}
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    if !local_dir.exists() {
        return Err(format!("local directory not found: {}", local_dir.display()));
    }
    if !local_dir.is_dir() {
        return Err(format!("not a directory: {}", local_dir.display()));
    }

    let local_dir = std::fs::canonicalize(&local_dir).map_err(|e| format!("cannot resolve path: {e}"))?;

    let state_path = local_dir.join(STATE_FILE);
    let mut state: SyncState = if state_path.exists() {
        let data = std::fs::read(&state_path).map_err(|e| format!("read state: {e}"))?;
        serde_json::from_slice(&data).map_err(|e| format!("invalid state file: {e}"))?
    } else {
        SyncState::default()
    };

    let account_id = check_sync_account(&api, &state).await?;
    if state.account_id.is_none() {
        state.owned_keys.clear();
        state.dirty_shared_roots.clear();
        state.removed_index_ids.clear();
        state.owner_index_dirty = false;
    }

    let remote_path = match remote_path_arg.or_else(|| state.remote_path.clone()) {
        Some(p) => normalize_remote_path(&p),
        None => {
            return Err("remote_path is required (no .bb-sync.json found). \
                        Usage: bb sync <local_dir> <remote_path>"
                .to_string());
        }
    };

    // Multiple owners (each parallel upload future) need the key; `MasterKey`
    // is not `Clone`, so share it behind an `Arc`. The downstream functions
    // that take `&MasterKey` get it via deref coercion.
    let master_key = Arc::new(load_master_key()?);

    // Graceful Ctrl-C: the first interrupt asks the upload phase to stop
    // cleanly (so we can report "interrupted — N done, M remaining" and resume
    // next run); a second interrupt hard-exits (covers the watch loop and a
    // wedged upload).
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let s = Arc::clone(&shutdown);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                s.store(true, Ordering::Relaxed);
            }
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        });
    }

    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {} {} {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            "syncing".custom_color(crate::colors::GREEN_OK),
            local_dir.display().to_string().custom_color(crate::colors::INK),
            format!("\u{2192} {remote_path}").custom_color(crate::colors::INK_DIM),
        );
        if dry_run {
            println!(
                "  {} {}",
                "mode".custom_color(crate::colors::INK_DIM),
                "dry-run (no file changes; security observations are saved)".custom_color(crate::colors::AMBER),
            );
        }
        println!();
    }

    let sync_start = Instant::now();
    let mut total_bytes: u64 = 0;

    let mut path_keys = OwnedKeyResolver::new();
    let resolved_root = resolve_remote_folder(&api, &master_key, &remote_path, dry_run, &mut path_keys).await;
    let remote_folder_id = resolved_root?;
    // `--rebind`: the user accepts the folder resolved now as this directory's
    // remote root. The old manifest describes the other tree, so it is dropped:
    // nothing that belonged there can be removed on this folder's account, and
    // the local files sync into the new folder as new files.
    if rebind && root_unreconciled(&state, remote_folder_id) {
        state.files.clear();
        state.owned_keys.clear();
        state.dirty_shared_roots.clear();
        state.removed_index_ids.clear();
        state.remote_folder_id = Some(remote_folder_id);
        print_notice("rebound to the new remote folder; the old folder's sync record was dropped");
    }
    // If this directory last synced against a DIFFERENT remote folder (the old
    // one was trashed and re-created, or the remote path changed), the manifest
    // describes files of another tree: nothing may be removed on its account.
    // The old binding is KEPT, so every later pass and the watch loops stay
    // blocked too, until `--rebind` (#83 review: adopting the new folder here
    // made the next pass trust it).
    let root_changed = root_unreconciled(&state, remote_folder_id);
    state.remote_path = Some(remote_path.clone());
    state.remote_folder_id.get_or_insert(remote_folder_id);

    // Scan phase — the dominant "looks frozen" symptom. Show a live spinner so
    // hashing the local tree + fetching the remote tree never looks like a hang.
    let scan_active = ui::is_rich() && std::io::stdout().is_terminal();

    let local_spinner = make_scan_spinner(scan_active, "scanning local", "files");
    let mut local_files = scan_local_files(&local_dir, &state.files, rehash, local_spinner.as_ref())?;
    if let Some(pb) = &local_spinner {
        pb.finish_and_clear();
    }
    let local_folders = walk_local_folders(&local_dir)?;

    let mut keys = sync_keys(&state)?;
    let remote_spinner = make_scan_spinner(scan_active, "scanning remote", "entries");
    let mut remote_files: HashMap<String, RemoteFile> = HashMap::new();
    let mut remote_folders: HashMap<String, Uuid> = HashMap::new();
    let mut listing_health = crate::sync_safety::ListingHealth::default();
    let scan_result = walk_remote(
        &api,
        &master_key,
        remote_folder_id,
        "",
        &mut remote_files,
        &mut remote_folders,
        &mut listing_health,
        &mut keys,
        remote_spinner.as_ref(),
    )
    .await;
    if let Some(pb) = &remote_spinner {
        pb.finish_and_clear();
    }
    scan_result?;

    // Register this device with the server (best-effort, non-fatal)
    let device_info = crate::device::load_or_create();
    let device_resp = if dry_run {
        None
    } else {
        // Task 1922: the device's client key IS its persisted `device_id`, and
        // the hostname leaves the machine only as a field-1 envelope sealed
        // under that key (field 1 takes no device key in the AAD).
        let hostname_envelope = seal_meta(
            &master_key,
            &device_info.device_id,
            None,
            ClientMetaField::DeviceHostname,
            &device_info.hostname,
        )?;
        match api
            .register_device(
                &device_info.device_id,
                &hostname_envelope,
                &device_info.platform,
                env!("CARGO_PKG_VERSION"),
            )
            .await
        {
            Ok(v) => Some(v),
            // A 401 means the server no longer accepts our session: stop now with
            // a distinct exit code so the service manager does not restart us.
            Err(e) if e.message == crate::api::SESSION_EXPIRED_MESSAGE => {
                return Err(crate::exit::sync_error(e.to_string()));
            }
            Err(_) => None,
        }
    };
    let server_device_id = device_resp
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    // Create or reuse a client session for this sync pair. Task 1922: the
    // record key is client-generated and persisted with the manifest; reuse
    // matches that key, and the three text fields leave only as envelopes.
    let session_id: Option<String> = if let Some(ref device_id) = server_device_id {
        let session_client_key = *state.session_client_key.get_or_insert_with(Uuid::new_v4);
        let session_client_key_str = session_client_key.to_string();
        if let Some(ref existing) = state.session_id {
            Some(existing.clone())
        } else {
            // Look for this folder's row by the client-side key. The plaintext
            // `remote_path` is always "" on a 1922 server, so it cannot be the
            // identity even when it happens to be present.
            let existing_session = api
                .list_client_sessions()
                .await
                .ok()
                .and_then(|resp| resp.get("sessions").and_then(|v| v.as_array()).cloned())
                .and_then(|sessions| {
                    sessions.into_iter().find(|s| {
                        s.get("client_key").and_then(|v| v.as_str()) == Some(&session_client_key_str)
                            && s.get("device_id").and_then(|v| v.as_str()) == Some(device_id)
                            && s.get("status").and_then(|v| v.as_str()) != Some("stopped")
                    })
                });

            if let Some(existing) = existing_session {
                let id = existing.get("id").and_then(|v| v.as_str()).map(String::from);
                if let Some(ref id) = id {
                    state.session_id = Some(id.clone());
                }
                id
            } else {
                let default_name = format!(
                    "{}/{}",
                    device_info.hostname,
                    remote_path.trim_start_matches('/').replace('/', "-")
                );
                let name_envelope = seal_meta(
                    &master_key,
                    &session_client_key,
                    Some(&device_info.device_id),
                    ClientMetaField::SessionName,
                    &default_name,
                )?;
                let local_envelope = seal_meta(
                    &master_key,
                    &session_client_key,
                    Some(&device_info.device_id),
                    ClientMetaField::SessionLocalPath,
                    &local_dir.display().to_string(),
                )?;
                let remote_envelope = seal_meta(
                    &master_key,
                    &session_client_key,
                    Some(&device_info.device_id),
                    ClientMetaField::SessionRemotePath,
                    &remote_path,
                )?;
                match api
                    .create_client_session(
                        device_id,
                        &session_client_key,
                        &name_envelope,
                        "sync",
                        &local_envelope,
                        &remote_envelope,
                    )
                    .await
                {
                    Ok(resp) => {
                        let id = resp.get("id").and_then(|v| v.as_str()).map(String::from);
                        if let Some(ref id) = id {
                            state.session_id = Some(id.clone());
                        }
                        id
                    }
                    Err(e) => {
                        if !ui::is_quiet() {
                            eprintln!("  {} session registration: {e}", "!".custom_color(crate::colors::AMBER));
                        }
                        None
                    }
                }
            }
        }
    } else {
        None
    };

    let moved_conflicts = if root_changed {
        HashSet::new()
    } else {
        reconcile_moves(&local_dir, &mut state, &local_files, &remote_files, dry_run)?
    };
    if !dry_run {
        local_files = walk_local_files(&local_dir)?;
    }
    // Which absent files may be treated as deleted? Only those the server itself
    // reports trashed, from a listing that is not empty/partial/unreadable.
    let absence = plan_absence(
        &api,
        &state,
        &local_files,
        &remote_files,
        &listing_health,
        root_changed,
        allow_mass_delete,
    )
    .await;
    if let Some(msg) = untrusted_notice(&absence, &state, &remote_files) {
        print_notice(&msg);
    }

    let mut all_folders: HashSet<String> = HashSet::new();
    all_folders.extend(local_folders.iter().cloned());
    all_folders.extend(remote_folders.keys().cloned());
    let mut sorted_folders: Vec<String> = all_folders.into_iter().collect();
    sorted_folders.sort_by_key(|s| s.matches('/').count());

    for folder_rel in &sorted_folders {
        if !remote_folders.contains_key(folder_rel) && listing_healthy(&listing_health) {
            let parent_id = parent_remote_id(folder_rel, &remote_folders, remote_folder_id);
            let folder_name = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
            if dry_run {
                println!(
                    "  {} {} {}",
                    "+".custom_color(crate::colors::GREEN_OK),
                    "would create remote folder".custom_color(crate::colors::INK_DIM),
                    folder_rel.custom_color(crate::colors::INK),
                );
            } else {
                let new_id = crate::item_id::create_owned_folder(&api, &master_key, folder_name, parent_id).await?;
                remote_folders.insert(folder_rel.clone(), new_id);
                println!(
                    "  {} {} {}",
                    "+".custom_color(crate::colors::GREEN_OK),
                    "remote folder".custom_color(crate::colors::INK_DIM),
                    folder_rel.custom_color(crate::colors::INK),
                );
            }
        }
        if !local_folders.contains(folder_rel) {
            let local_path =
                match crate::safe_path::safe_rel_join(&local_dir, folder_rel, crate::safe_path::Rules::Host) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!(
                            "  {} skipped local folder {folder_rel:?}: {e}",
                            "!".custom_color(crate::colors::AMBER)
                        );
                        continue;
                    }
                };
            if !dry_run {
                std::fs::create_dir_all(&local_path).map_err(|e| format!("mkdir {}: {e}", local_path.display()))?;
            }
            println!(
                "  {} {} {}",
                "+".custom_color(crate::colors::GREEN_OK),
                "local folder".custom_color(crate::colors::INK_DIM),
                folder_rel.custom_color(crate::colors::INK),
            );
        }
    }

    let mut all_files: HashSet<String> = HashSet::new();
    all_files.extend(local_files.keys().cloned());
    all_files.extend(remote_files.keys().cloned());
    // Include manifest keys so a prior-synced file gone from BOTH local and
    // remote (deleted on both sides) is still visited — otherwise its stale
    // entry persists and is silently resurrected on a later round-trip. This
    // makes the `(None, None, prior)` RemoteDelete arm below reachable in the
    // one-shot path (the watch loops reach it via reconcile_remote_deletions).
    all_files.extend(state.files.keys().cloned());
    let mut sorted_files: Vec<String> = all_files.into_iter().collect();
    sorted_files.sort();

    let mut up_count = 0u32;
    let mut down_count = 0u32;
    let mut conflicts = 0u32;
    let mut deletes = 0u32;
    let mut skipped = 0u32;
    // Uploads that errored (not Ctrl-C). Together with `conflicts` this decides
    // whether a one-shot pass may report "synced" and exit 0.
    let mut failed = listing_health.unreadable + u32::from(listing_health.truncated);
    // SF-0002 r3 (K6): local edits of read-only rows, kept here and reported as
    // a notice (not a failure). They still leave the pass incomplete.
    let mut read_only_kept = 0u32;

    // ── Phase 1: classify each file into an action ─────────────────────────
    enum SyncAction {
        Upload {
            rel: String,
            local: LocalFile,
            replace_id: Option<Uuid>,
        },
        Download {
            rel: String,
            remote: RemoteFile,
        },
        Conflict {
            msg: String,
        },
        Delete {
            rel: String,
            remote: RemoteFile,
        },
        MissingLocal {
            rel: String,
        },
        /// A prior-synced file the server reports TRASHED (tombstone). The local
        /// mirror is byte-identical to the manifest, so it is moved to the
        /// recoverable folder + the manifest entry dropped.
        RemoteDelete {
            rel: String,
        },
        /// A prior-synced file gone from BOTH sides: just drop the stale entry.
        ForgetStale {
            rel: String,
        },
        /// Missing remotely, untouched locally, no deletion record: keep it.
        KeepUnconfirmed {
            rel: String,
        },
        Skip,
    }

    let mut actions: Vec<SyncAction> = Vec::with_capacity(sorted_files.len());
    let policy = if force {
        ConflictPolicy::Force
    } else {
        ConflictPolicy::Skip
    };

    for rel in &sorted_files {
        if moved_conflicts.contains(rel) {
            actions.push(SyncAction::Conflict {
                msg: format!("{rel} moved remotely; local edits kept"),
            });
            continue;
        }
        let local = local_files.get(rel).cloned();
        let remote = remote_files.get(rel).cloned();
        let prior = trusted_prior(&state, rel, remote.is_some(), root_changed);

        match (local, remote, prior) {
            (Some(l), Some(r), Some(p)) => {
                let local_changed = local_changed_vs_manifest(&l, &p);
                let remote_changed = remote_changed(&r, &p);
                match decide(local_changed, remote_changed, policy) {
                    Direction::Skip => actions.push(SyncAction::Skip),
                    Direction::Upload => actions.push(SyncAction::Upload {
                        rel: rel.clone(),
                        local: l,
                        replace_id: Some(r.id),
                    }),
                    Direction::Download => actions.push(SyncAction::Download {
                        rel: rel.clone(),
                        remote: r,
                    }),
                    Direction::Conflict => actions.push(SyncAction::Conflict {
                        msg: format!("{rel} (skipped \u{2014} use --force to overwrite)"),
                    }),
                }
            }
            (Some(l), Some(r), None) => {
                if policy == ConflictPolicy::Force {
                    actions.push(SyncAction::Upload {
                        rel: rel.clone(),
                        local: l,
                        replace_id: Some(r.id),
                    });
                } else {
                    actions.push(SyncAction::Conflict {
                        msg: format!("{rel} (exists both sides, no prior sync \u{2014} skipped)"),
                    });
                }
            }
            (Some(l), None, prior) => {
                // Local file present, remote absent. The prior manifest entry
                // distinguishes "user just created this" from "this was synced
                // before and has since been deleted on the server".
                let tombstoned = prior.as_ref().is_some_and(|p| absence.tombstoned(p));
                match classify_remote_absent(Some(&l), prior.as_ref(), tombstoned) {
                    RemoteAbsent::DeleteLocal => {
                        // Synced before + byte-identical to the manifest + the
                        // server reports it trashed: a genuine remote delete.
                        actions.push(SyncAction::RemoteDelete { rel: rel.clone() });
                    }
                    RemoteAbsent::KeepUnconfirmed => {
                        actions.push(SyncAction::KeepUnconfirmed { rel: rel.clone() });
                    }
                    RemoteAbsent::ForgetStale => {
                        actions.push(SyncAction::ForgetStale { rel: rel.clone() });
                    }
                    RemoteAbsent::ReAddLocal => {
                        // Synced before but edited offline → never destroy the
                        // local edit; re-upload it as a new file.
                        actions.push(SyncAction::Upload {
                            rel: rel.clone(),
                            local: l,
                            replace_id: None,
                        });
                    }
                    RemoteAbsent::NotADelete => {
                        // No prior entry → genuinely new local file: upload.
                        actions.push(SyncAction::Upload {
                            rel: rel.clone(),
                            local: l,
                            replace_id: None,
                        });
                    }
                }
            }
            (None, Some(r), prior) => {
                if prior.is_some() {
                    if delete_remote {
                        actions.push(SyncAction::Delete {
                            rel: rel.clone(),
                            remote: r,
                        });
                    } else {
                        actions.push(SyncAction::MissingLocal { rel: rel.clone() });
                    }
                } else {
                    actions.push(SyncAction::Download {
                        rel: rel.clone(),
                        remote: r,
                    });
                }
            }
            (None, None, prior) => {
                if prior.is_some() {
                    // Synced before, now gone from BOTH sides (user deleted it
                    // locally and it's also gone remotely). The local file is
                    // already absent - nothing to touch - but the stale
                    // manifest entry must be dropped so it isn't resurrected.
                    actions.push(SyncAction::ForgetStale { rel: rel.clone() });
                } else {
                    actions.push(SyncAction::Skip);
                }
            }
        }
    }

    // ── Phase 2: print conflicts/skips, collect uploads, execute downloads ─
    // Handle conflicts, missing-local, and skips immediately (no I/O)
    let mut pending_uploads: Vec<(String, LocalFile, Option<Uuid>)> = Vec::new();
    let mut pending_downloads: Vec<(String, RemoteFile)> = Vec::new();
    let mut pending_deletes: Vec<(String, RemoteFile)> = Vec::new();
    // Prior-synced files the server reports trashed: move the local mirror aside.
    let mut pending_remote_deletes: Vec<String> = Vec::new();
    // Prior-synced files gone from both sides: drop the stale manifest entry.
    let mut pending_forget: Vec<String> = Vec::new();
    let mut kept_unconfirmed: Vec<String> = Vec::new();

    for action in actions {
        match action {
            SyncAction::Skip => {
                skipped += 1;
            }
            SyncAction::Conflict { msg } => {
                if !ui::is_json() && !ui::is_quiet() {
                    println!(
                        "  {} {} {}",
                        "\u{26A1}".custom_color(crate::colors::AMBER),
                        "conflict".custom_color(crate::colors::AMBER),
                        msg.custom_color(crate::colors::INK),
                    );
                }
                conflicts += 1;
            }
            SyncAction::MissingLocal { rel } => {
                if !ui::is_json() && !ui::is_quiet() {
                    println!(
                        "  {} {} {}",
                        "?".custom_color(crate::colors::INK_DIM),
                        "missing locally".custom_color(crate::colors::INK_DIM),
                        format!("{rel} (use --delete to trash from vault)").custom_color(crate::colors::INK_DIM),
                    );
                }
            }
            SyncAction::Upload { rel, local, replace_id } => {
                if !listing_healthy(&listing_health) || foreign_missing(&state, &rel, &remote_files) {
                    skipped += 1;
                    continue;
                }
                pending_uploads.push((rel, local, replace_id));
            }
            SyncAction::Download { rel, remote } => {
                pending_downloads.push((rel, remote));
            }
            SyncAction::Delete { rel, remote } => {
                pending_deletes.push((rel, remote));
            }
            SyncAction::RemoteDelete { rel } => {
                pending_remote_deletes.push(rel);
            }
            SyncAction::ForgetStale { rel } => {
                pending_forget.push(rel);
            }
            SyncAction::KeepUnconfirmed { rel } => {
                kept_unconfirmed.push(rel);
                skipped += 1;
            }
        }
    }
    if let Some(msg) = kept_unconfirmed_notice(&kept_unconfirmed) {
        print_notice(&msg);
    }

    // Mass-delete breaker (spec R4): more than max(50, 10 %) removals in one
    // pass is a stop-and-ask, not a sync.
    let mut held_back = 0u32;
    if !allow_mass_delete && crate::sync_safety::breaker_trips(pending_remote_deletes.len(), state.files.len()) {
        print_notice(&breaker_notice(pending_remote_deletes.len(), state.files.len()));
        held_back = pending_remote_deletes.len() as u32;
        pending_remote_deletes.clear();
    }

    // `--delete` against an unreconciled root (#83 review): the manifest's
    // "synced before, now missing locally" entries belong to the other tree, so
    // they say nothing about this folder's same-named files. Trash nothing and
    // fail the pass until `bb sync --rebind`.
    let delete_refused = delete_remote && root_changed;
    if delete_refused {
        print_notice(
            "--delete held back: the remote folder is not the one this directory last synced with, so nothing \
             is trashed. Run `bb sync --rebind` once to accept the new folder, then re-run.",
        );
        // `trusted_prior` already keeps old-tree entries from queuing a Delete;
        // this is the backstop.
        held_back += pending_deletes.len() as u32;
        pending_deletes.clear();
    }

    // ── Phase 3: parallel streaming uploads ────────────────────────────────
    let pending_count = pending_uploads.len();
    let mut interrupted_count = 0u32;
    let upload_summary;
    {
        use futures_util::stream::{self, StreamExt};

        // Task 2333: live lines on a terminal (spec §6.1). Each file is planned as the driver will send it (its
        // resume record's chunk size, else `--concurrency`'s); one chunk at a time per file, `--concurrency` files.
        let progress = transfer_sink(
            crate::transfer_stats::Direction::Upload,
            !dry_run && pending_count > 0,
            verbose,
        );
        for (rel, local, _) in pending_uploads.iter().filter(|_| !dry_run) {
            let pending = crate::resume::resumable_upload_now(&local_dir.join(rel));
            let c = concurrency as u32;
            let chunk_size = crate::upload::planned_chunk_size(local.size, c, c, pending.as_ref());
            let (bytes, chunks) = crate::upload::expected_at_chunk_size(local.size, chunk_size);
            progress.plan_file(bytes, chunks);
        }
        progress.seal_plan();
        // `--concurrency` files at once, each one chunk at a time: the driver sets a file's limit, the sink scales it.
        progress.set_files_at_once(concurrency as u32);
        progress.set_limit(crate::upload::CHUNKS_IN_FLIGHT_PER_FILE);
        // The live lines show each file; the plain `↑` lines are for a run without them.
        let show_bars = scan_active && progress.draws_live();

        let upload_results: Vec<Result<(String, FileEntry, u64), String>> =
            stream::iter(pending_uploads.into_iter().map(|(rel, local, replace_id)| {
                let api = &api;
                let master_key = Arc::clone(&master_key);
                let local_dir = &local_dir;
                let remote_folders = &remote_folders;
                let resolved = remote_files.get(&rel).and_then(|r| r.resolved.clone());
                let progress = &progress;
                let shutdown = Arc::clone(&shutdown);
                async move {
                    if shutdown.load(Ordering::Relaxed) {
                        return Err(crate::upload::INTERRUPTED.to_string());
                    }

                    // Plain status line only when bars are not showing them.
                    if !show_bars && !ui::is_json() && !ui::is_quiet() {
                        let size_str = format_size(local.size);
                        progress.note(
                            &format!(
                                "  {} {} {}",
                                "\u{2191}".custom_color(crate::colors::GREEN_OK),
                                rel.custom_color(crate::colors::INK),
                                format!("({size_str})").custom_color(crate::colors::INK_DIM),
                            ),
                            crate::upload::Stream::Stdout,
                        );
                    }

                    if dry_run {
                        return Ok((
                            rel,
                            FileEntry {
                                remote_id: Uuid::nil(),
                                remote_version: None,
                                last_mtime: local.mtime,
                                last_size: local.size,
                                last_sync: Utc::now(),
                                content_hash: Some(local.content_hash.clone()),
                            },
                            local.size,
                        ));
                    }

                    // SF-0002: ask the write check before touching the remote
                    // row. A read-only row or one the check could not read is
                    // left alone (never trashed) and this edit is not uploaded
                    // this pass; the next pass asks again.
                    let (replacement, trash_old) =
                        match replace_decision(api, &master_key, resolved.as_ref(), replace_id.is_some()).await {
                            Replace::Skip(why) if why == crate::crypto::OWNER_REPAIR_READ_ONLY => {
                                return Err(format!("{READ_ONLY_KEPT}{rel}"));
                            }
                            Replace::Skip(why) => return Err(format!("{rel}: {why}")),
                            Replace::InPlace(r) => (Some(r.clone()), false),
                            Replace::NewFile => (None, true),
                            // L13/A9b: a request row's edit becomes a new owner
                            // item; the request row is neither versioned nor
                            // trashed.
                            Replace::SaveAsNew => (None, false),
                        };
                    // Not a version of the old remote: it is uploaded as a new
                    // file and the old one trashed only AFTER that (L5).
                    let replaced = replace_id.filter(|_| trash_old);

                    let parent_id = match rel.rsplit_once('/') {
                        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
                        None => Some(remote_folder_id),
                    }
                    .or(Some(remote_folder_id));

                    let file_name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                    // L13/N4: a new owner item's id (and its resume tag) comes
                    // from core's mint; a replace keeps the listed row.
                    let (file_id, resume_tag) = match replacement.as_ref() {
                        Some(_) => (replace_id.ok_or("missing replacement id")?, None),
                        None => {
                            let minted = crate::item_id::mint_owner(&master_key)?;
                            (minted.id(), Some(*minted.tag()))
                        }
                    };
                    let spec = crate::upload::UploadSpec {
                        path: local_dir.join(&rel),
                        file_name,
                        file_id,
                        resume_tag,
                        // A version replace only when the old remote's key is
                        // owned; otherwise a new file under a fresh id (the old
                        // one is trashed once it exists).
                        base_version_number: replacement
                            .as_ref()
                            .map(|r| replacement_version(&r.canonical))
                            .transpose()?,
                        replace_pin: replacement
                            .as_ref()
                            .map(|r| {
                                crate::crypto::content_pin_with_key(
                                    &r.key,
                                    r.canonical["id"].as_str().unwrap_or(""),
                                    r.canonical["name_encrypted"].as_str().unwrap_or(""),
                                )
                            })
                            .transpose()?,
                        replace_file_key: replacement.as_ref().map(|r| r.key.clone()),
                        parent_id,
                        concurrency: concurrency as u32,
                        unrecorded_resume_concurrency: concurrency as u32,
                        chunks_in_flight: crate::upload::CHUNKS_IN_FLIGHT_PER_FILE,
                        limits: crate::upload::limits::ChunkLimits::single(),
                        interrupt: None,
                        shutdown,
                        read_row: None,
                    };
                    // Name the file in the error: "upload failed: <server
                    // message>" alone doesn't say which file to look at (flow-6).
                    let outcome = upload_then_trash_replaced(api, master_key, spec, progress, replaced)
                        .await
                        .map_err(|e| {
                            if e == crate::upload::INTERRUPTED {
                                e
                            } else {
                                format!("{rel}: {e}")
                            }
                        })?;

                    let entry = FileEntry {
                        remote_id: outcome.server_id,
                        remote_version: replacement
                            .as_ref()
                            .and_then(|r| crate::api::current_version_number(&r.canonical))
                            .and_then(|v| v.checked_add(1)),
                        last_mtime: local.mtime,
                        last_size: local.size,
                        last_sync: Utc::now(),
                        content_hash: Some(local.content_hash.clone()),
                    };
                    Ok((rel, entry, local.size))
                }
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;

        // The live lines go (the ticker stops) before the summary, any error or the interrupted line prints.
        upload_summary = progress.summary();

        for result in upload_results {
            match result {
                Ok((rel, entry, size)) => {
                    if !dry_run {
                        state.files.insert(rel, entry);
                    }
                    total_bytes += size;
                    up_count += 1;
                }
                Err(e) if e == crate::upload::INTERRUPTED => {
                    interrupted_count += 1;
                }
                Err(e) if e.starts_with(READ_ONLY_KEPT) => {
                    print_notice(&read_only_notice(&e[READ_ONLY_KEPT.len()..]));
                    read_only_kept += 1;
                }
                Err(e) => {
                    eprintln!("  {} upload failed: {}", "!".custom_color(crate::colors::RED_ERR), e);
                    failed += 1;
                }
            }
        }
    }

    // If Ctrl-C interrupted the upload phase, persist what we confirmed and
    // stop cleanly — don't start new downloads/deletes or enter watch mode.
    // A re-run resumes from the saved state.
    if shutdown.load(Ordering::Relaxed) {
        if !dry_run {
            state.last_sync = Some(Utc::now());
            let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
            std::fs::write(&state_path, data).map_err(|e| format!("write state file: {e}"))?;
        }
        let remaining = (pending_count as u32).saturating_sub(up_count);
        if !ui::is_json() {
            println!();
            println!(
                "  {} {} \u{00b7} {} done, {} remaining \u{00b7} re-run to finish",
                "\u{25CF}".custom_color(crate::colors::AMBER),
                "interrupted".custom_color(crate::colors::AMBER),
                up_count,
                remaining.max(interrupted_count),
            );
        }
        // Ctrl-C during the upload phase: the process is exiting without ever
        // entering watch mode, so stop the session this run owns (best-effort)
        // — otherwise the server's heartbeat checker keeps nagging the user.
        if let Some(ref sid) = session_id {
            stop_session_best_effort(&api, sid).await;
        }
        return Ok(());
    }

    // ── Phase 4: sequential downloads ──────────────────────────────────────
    // Task 2333: a second run model, drawn after the upload phase's lines cleared (spec §8).
    let downloads = transfer_sink(
        crate::transfer_stats::Direction::Download,
        !dry_run && !pending_downloads.is_empty(),
        verbose,
    );
    for (_, remote) in pending_downloads.iter().filter(|_| !dry_run) {
        let (bytes, chunks) = download_plan(remote);
        downloads.plan_file(bytes, chunks);
    }
    downloads.seal_plan();
    for (rel, remote) in &pending_downloads {
        let sink = Some(&downloads);
        match do_download(&api, &master_key, &local_dir, rel, remote, &mut state, dry_run, sink).await? {
            DownloadOutcome::Downloaded => down_count += 1,
            // Still uploading elsewhere (or unsafe destination); nothing written.
            DownloadOutcome::Deferred => skipped += 1,
            // The remote copy is untouched; the next pass tries again (task 1916).
            DownloadOutcome::Unreadable => failed += 1,
        }
    }

    let download_summary = downloads.summary();

    // ── Phase 5: sequential deletes ────────────────────────────────────────
    for (rel, remote) in &pending_deletes {
        if !dry_run {
            api.trash_file(&remote.id.to_string()).await?;
            state.files.remove(rel);
            state.removed_index_ids.insert(remote.id.to_string());
        }
        if !ui::is_json() && !ui::is_quiet() {
            println!(
                "  {} {} {}",
                "\u{2717}".custom_color(crate::colors::RED_ERR),
                "trashing remote".custom_color(crate::colors::RED_ERR),
                rel.custom_color(crate::colors::INK),
            );
        }
        deletes += 1;
    }

    // ── Phase 6: remote-delete reconciliation ──────────────────────────────
    // Absence is not deletion (task 1915). A local mirror is removed only when
    // the server itself reports that file's id trashed (a tombstone, checked in
    // `plan_absence`), the file is still byte-identical to the manifest (re-checked
    // right now, not from the scan), and the breaker above did not trip. The
    // file is MOVED to `.bb-trash/` inside the sync folder - never unlinked - so
    // the only live copy can always be put back.
    for rel in &pending_remote_deletes {
        if !dry_run {
            let Some(entry) = state.files.get(rel).cloned() else {
                continue;
            };
            match remove_local_recoverably(&local_dir, rel, &entry) {
                Ok(true) => {
                    state.files.remove(rel);
                    state.removed_index_ids.insert(entry.remote_id.to_string());
                }
                Ok(false) => {
                    print_notice(&format!("{rel} changed since the scan, kept"));
                    continue;
                }
                Err(e) => {
                    print_notice(&e);
                    // Keep the manifest entry so we retry on the next pass.
                    continue;
                }
            }
        }
        if !ui::is_json() && !ui::is_quiet() {
            println!(
                "  {} {} {}",
                "\u{2717}".custom_color(crate::colors::RED_ERR),
                format!("moved to {}/ (deleted remotely)", crate::sync_safety::RECOVERY_DIR)
                    .custom_color(crate::colors::INK_DIM),
                rel.custom_color(crate::colors::INK),
            );
        }
        deletes += 1;
    }
    // Gone from both sides: nothing on disk to touch, just drop the stale entry.
    for rel in &pending_forget {
        if !dry_run {
            state.files.remove(rel);
        }
    }

    state.last_sync = Some(Utc::now());
    if !dry_run && failed == 0 && conflicts == 0 && held_back == 0 && !delete_refused {
        state.account_id = Some(account_id);
    }
    let index_report = if !dry_run {
        let report = finish_owned_sync(&api, &master_key, &mut keys, &mut state, remote_folder_id, root_changed).await;
        let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
        std::fs::write(&state_path, data).map_err(|e| format!("write state file: {e}"))?;
        report
    } else {
        crate::shared_folder_index::IndexRepairReport::default()
    };

    // Send a final heartbeat for --once mode
    if once {
        if let Some(ref sid) = session_id {
            let file_count = local_files.len() as u64;
            let _ = api
                .send_heartbeat(sid, "idle", Some(file_count), None, Some(total_bytes), None, None)
                .await;
        }
    }

    // Whether this invocation will enter the long-lived watch loop below.
    // Everything else — `--once`, `--dry-run`, a Ctrl-C'd pass, or a single
    // json/quiet pass — is about to exit, so its session must be stopped now;
    // the continuous watch path stops its session after the loop unwinds.
    // Computed once and reused for the watch guard so the two stay consistent
    // (a fresh re-check could leave a Ctrl-C'd run as a zombie).
    let will_enter_watch = !once && !dry_run && !shutdown.load(Ordering::Relaxed) && !ui::is_json() && !ui::is_quiet();
    if !will_enter_watch {
        if let Some(ref sid) = session_id {
            stop_session_best_effort(&api, sid).await;
        }
    }

    let elapsed = sync_start.elapsed();

    // L15 = A (P2-11): ONE notice per pass for a write into a shared root. The
    // root the sync writes into is the only destination, so asking it once is
    // enough; a non-shared root prints nothing.
    if !dry_run && up_count > 0 && !ui::is_json() {
        crate::owned_keys::announce_owner_only(&api, &master_key, Some(&remote_folder_id.to_string())).await;
    }

    report_pass(PassReport {
        uploaded: up_count,
        downloaded: down_count,
        conflicts,
        failed,
        skipped,
        deletes,
        held_back,
        read_only_kept,
        total_bytes,
        elapsed,
        index_report: &index_report,
        dry_run,
        will_enter_watch,
        delete_refused,
        upload: upload_summary,
        download: download_summary,
        verbose,
    })?;

    // ── Continuous watch mode ───────────────────────────────────────────────
    if will_enter_watch {
        let use_tui = ui::is_rich() && std::io::stdout().is_terminal();

        let daemonized = if use_tui {
            run_tui_watch(
                &local_dir,
                &remote_path,
                remote_folder_id,
                &mut state,
                &state_path,
                session_id.as_deref(),
                local_files.len() as u64,
                total_bytes,
            )
            .await?
        } else {
            run_plain_watch(
                &local_dir,
                &remote_path,
                remote_folder_id,
                &mut state,
                &state_path,
                session_id.as_deref(),
                local_files.len() as u64,
                total_bytes,
                &shutdown,
            )
            .await?;
            false
        };

        // The watch loop has exited (Ctrl-C, or the user quit the TUI). Stop the
        // session this run owns so the heartbeat checker stops nagging — unless
        // the user daemonized, in which case the background daemon now owns it.
        if !daemonized {
            if let Some(ref sid) = session_id {
                stop_session_best_effort(&api, sid).await;
            }
        }
    }

    Ok(())
}

// ── Plain-text watch loop (non-TTY / json / quiet fallback) ───────────────

// ── TUI watch loop (interactive terminal) ─────────────────────────────────

// ── Client-metadata envelopes (task 1922) ─────────────────────────────────
//
// The server stores and returns device/session text only as `bbcm1.` envelopes.
// Sealing uses the locally persisted record keys; opening uses the master key
// from the config. A row that does not open — a pre-1922 row the server nulled,
// another account's key, a corrupt envelope — is shown as hidden and never
// fails the command.

// ── Session management helpers ─────────────────────────────────────────────

// ── Daemon spawning ────────────────────────────────────────────────────────
