//! `watch` items split out of `sync.rs`.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_plain_watch(
    local_dir: &Path,
    remote_path: &str,
    remote_folder_id: Uuid,
    state: &mut SyncState,
    state_path: &Path,
    session_id: Option<&str>,
    file_count: u64,
    total_bytes: u64,
    shutdown: &Arc<AtomicBool>,
) -> Result<(), String> {
    let local_display = local_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local_dir.display().to_string());

    let dashboard = Arc::new(SyncDashboard {
        local_path: local_display,
        remote_path: remote_path.to_string(),
        file_count: AtomicU64::new(file_count),
        total_bytes: AtomicU64::new(total_bytes),
        is_synced: AtomicBool::new(true),
    });

    println!();
    print_dashboard(&dashboard);

    // Spawn a background heartbeat every 60s while watching
    if let Some(sid) = session_id {
        let hb_api = ApiClient::from_config();
        let hb_sid = sid.to_string();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let _ = hb_api
                    .send_heartbeat(&hb_sid, "watching", None, None, None, None, None)
                    .await;
            }
        });
    }

    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;

    let (tx, rx) = std::sync::mpsc::channel();
    let mut debouncer =
        new_debouncer(std::time::Duration::from_millis(500), None, tx).map_err(|e| format!("watcher: {e}"))?;

    debouncer
        .watch(local_dir, RecursiveMode::Recursive)
        .map_err(|e| format!("watch: {e}"))?;

    eprintln!(
        "  {} watching for changes \u{00b7} press Ctrl+C to stop",
        "\u{00b7}".custom_color(crate::colors::INK_DIM)
    );

    loop {
        // Poll the watch channel with a timeout so a Ctrl-C (which sets the
        // shutdown flag) breaks the loop promptly instead of blocking forever.
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(Ok(events)) => {
                let changed: Vec<_> = events
                    .iter()
                    .filter_map(|e| e.paths.first().cloned())
                    .filter(|p| !is_ignored_path(p))
                    .collect();

                if !changed.is_empty() {
                    dashboard.is_synced.store(false, Ordering::Relaxed);
                    let now = chrono::Local::now().format("%H:%M:%S");
                    for path in &changed {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        eprintln!(
                            "  {} {} {}",
                            now.to_string().custom_color(crate::colors::INK_DIM),
                            "\u{2191}".custom_color(crate::colors::GREEN_OK),
                            name.custom_color(crate::colors::INK),
                        );
                    }

                    // Re-run the sync to upload changes
                    let resync_api = ApiClient::from_config();
                    check_sync_account(&resync_api, state).await?;
                    let resync_master = Arc::new(load_master_key()?);
                    crate::temp_record::sweep();
                    let mut resync_local = walk_local_files(local_dir)?;
                    let resync_folders = walk_local_folders(local_dir)?;
                    let mut resync_remote_files: HashMap<String, RemoteFile> = HashMap::new();
                    let mut resync_remote_folders: HashMap<String, Uuid> = HashMap::new();
                    let mut resync_health = crate::sync_safety::ListingHealth::default();
                    let mut keys = sync_keys(state)?;
                    let scan_result = walk_remote(
                        &resync_api,
                        &resync_master,
                        remote_folder_id,
                        "",
                        &mut resync_remote_files,
                        &mut resync_remote_folders,
                        &mut resync_health,
                        &mut keys,
                        None,
                    )
                    .await;
                    scan_result?;

                    let blocked = refresh_watch_reads(
                        &resync_api,
                        &resync_master,
                        local_dir,
                        state,
                        &mut resync_local,
                        &resync_remote_files,
                        remote_folder_id,
                    )
                    .await?;
                    // Ensure remote folders exist
                    let mut all_dirs: HashSet<String> = HashSet::new();
                    all_dirs.extend(resync_folders.iter().cloned());
                    all_dirs.extend(resync_remote_folders.keys().cloned());
                    let mut sorted_dirs: Vec<String> = all_dirs.into_iter().collect();
                    sorted_dirs.sort_by_key(|s| s.matches('/').count());
                    for folder_rel in &sorted_dirs {
                        if !resync_remote_folders.contains_key(folder_rel) && listing_healthy(&resync_health) {
                            let pid = parent_remote_id(folder_rel, &resync_remote_folders, remote_folder_id);
                            let fname = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
                            let new_id =
                                crate::item_id::create_owned_folder(&resync_api, &resync_master, fname, pid).await?;
                            resync_remote_folders.insert(folder_rel.clone(), new_id);
                        }
                    }

                    // Reconcile remote deletions FIRST: any prior-synced file no
                    // longer present remotely + byte-identical locally is removed
                    // (shared helper — same decision as the one-shot path). This
                    // must run before the upload loop so a remotely-deleted file
                    // is not re-uploaded (resurrected) by the change scan below.
                    // The watch loop never mass-deletes: the breaker has no
                    // confirmation channel here, so `allow_mass_delete` is false.
                    // A root change the first pass saw is still unreconciled here.
                    let absence = plan_absence(
                        &resync_api,
                        state,
                        &resync_local,
                        &resync_remote_files,
                        &resync_health,
                        root_unreconciled(state, remote_folder_id),
                        false,
                    )
                    .await;
                    if let Some(msg) = untrusted_notice(&absence, state, &resync_remote_files) {
                        print_notice(&msg);
                    }
                    let reconciled = reconcile_remote_deletions(
                        local_dir,
                        state,
                        &resync_local,
                        &resync_remote_files,
                        &absence,
                        false,
                        false,
                    );
                    for msg in &reconciled.notices {
                        print_notice(msg);
                    }
                    let removed = reconciled.removed;
                    let now_del = chrono::Local::now().format("%H:%M:%S");
                    for rel in &removed {
                        eprintln!(
                            "  {} {} {}",
                            now_del.to_string().custom_color(crate::colors::INK_DIM),
                            "\u{2717}".custom_color(crate::colors::RED_ERR),
                            rel.custom_color(crate::colors::INK),
                        );
                    }
                    // Drop the just-removed keys from the pre-reconcile snapshot.
                    // The upload loop below iterates this snapshot; without this,
                    // a removed key (local=Some in the stale snapshot, remote=None,
                    // prior=None after the manifest drop) would look like a fresh
                    // change and re-upload the file we just unlinked (resurrecting
                    // a remote delete, and erroring on the missing file).
                    for rel in &removed {
                        resync_local.remove(rel);
                    }

                    // Upload changed files
                    let mut resync_bytes: u64 = 0;
                    let resync_keys: Vec<String> = resync_local.keys().cloned().collect();
                    for rel in &resync_keys {
                        if !listing_healthy(&resync_health)
                            || blocked.contains(rel)
                            || foreign_missing(state, rel, &resync_remote_files)
                        {
                            continue;
                        }
                        let local = resync_local.get(rel).cloned();
                        let remote = resync_remote_files.get(rel).cloned();
                        let prior =
                            trusted_prior(state, rel, remote.is_some(), root_unreconciled(state, remote_folder_id));

                        if let Some(l) = local {
                            // Use the SAME content-hash-aware change detection as
                            // the one-shot path (local_changed_vs_manifest). A
                            // same-size/same-mtime offline edit that was remote-
                            // deleted is kept by the reconcile above (ReAddLocal)
                            // and MUST still be re-uploaded here — a bare
                            // (mtime,size) compare would miss it.
                            let local_changed = match &prior {
                                Some(p) => local_changed_vs_manifest(&l, p),
                                None => remote.is_none(),
                            };
                            if local_changed {
                                match do_upload(
                                    &resync_api,
                                    Arc::clone(&resync_master),
                                    local_dir,
                                    rel,
                                    &l,
                                    remote,
                                    remote_folder_id,
                                    &resync_remote_folders,
                                    state,
                                    false,
                                )
                                .await
                                {
                                    // SF-0002: a read-only or unchecked row is left as it is and
                                    // asked again next pass; it does not end the watch.
                                    Err(e) if e == crate::crypto::OWNER_REPAIR_READ_ONLY => {
                                        print_notice(&read_only_notice(rel));
                                    }
                                    Err(e) if crate::owned_keys::is_write_refusal(&e) => {
                                        eprintln!("  {} {rel}: {e}", "!".custom_color(crate::colors::AMBER));
                                    }
                                    other => {
                                        other?;
                                        resync_bytes += l.size;
                                    }
                                }
                            }
                        }
                    }

                    finish_owned_sync(
                        &resync_api,
                        &resync_master,
                        &mut keys,
                        state,
                        remote_folder_id,
                        root_unreconciled(state, remote_folder_id),
                    )
                    .await;
                    state.last_sync = Some(Utc::now());
                    let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
                    std::fs::write(state_path, data).map_err(|e| format!("write state file: {e}"))?;

                    // Update dashboard (the sweep already ran at the top of this pass)
                    let new_local = walk_local_files(local_dir)?;
                    dashboard.file_count.store(new_local.len() as u64, Ordering::Relaxed);
                    dashboard.total_bytes.fetch_add(resync_bytes, Ordering::Relaxed);
                    dashboard.is_synced.store(true, Ordering::Relaxed);
                }
            }
            Ok(Err(errs)) => {
                for e in errs {
                    eprintln!("  {} watch error: {}", "!".custom_color(crate::colors::RED_ERR), e);
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_tui_watch(
    local_dir: &Path,
    remote_path: &str,
    remote_folder_id: Uuid,
    state: &mut SyncState,
    state_path: &Path,
    session_id: Option<&str>,
    file_count: u64,
    total_bytes: u64,
) -> Result<bool, String> {
    use crate::tui::events::{TuiAction, TuiEvent};
    use crate::tui::state::{FileEventStatus, SyncFileEvent};

    let local_display = local_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| local_dir.display().to_string());

    let session_name = format!("{local_display} \u{2192} {remote_path}");

    let (tui_tx, tui_rx) = tokio::sync::mpsc::unbounded_channel::<TuiEvent>();

    // Spawn heartbeat task.
    if let Some(sid) = session_id {
        let hb_api = ApiClient::from_config();
        let hb_sid = sid.to_string();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let _ = hb_api
                    .send_heartbeat(&hb_sid, "watching", None, None, None, None, None)
                    .await;
            }
        });
    }

    // Spawn session refresh task (every 30s, sends SessionsRefresh events).
    {
        let tx = tui_tx.clone();
        tokio::spawn(async move {
            let refresh_api = ApiClient::from_config();
            // Session text is ciphertext to the server; the dashboard needs the
            // master key to show it (task 1922).
            let Ok(master_key) = load_master_key() else { return };
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if let Ok(resp) = refresh_api.list_client_sessions().await {
                    let sessions = parse_sessions_from_json(&resp, &master_key);
                    let _ = tx.send(TuiEvent::SessionsRefresh(sessions));
                }
            }
        });
    }

    // Do an immediate session refresh so the dashboard has data.
    {
        let tx = tui_tx.clone();
        tokio::spawn(async move {
            let api = ApiClient::from_config();
            let Ok(master_key) = load_master_key() else { return };
            if let Ok(resp) = api.list_client_sessions().await {
                let sessions = parse_sessions_from_json(&resp, &master_key);
                let _ = tx.send(TuiEvent::SessionsRefresh(sessions));
            }
        });
    }

    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;

    let (watch_tx, watch_rx) = std::sync::mpsc::channel();
    let mut debouncer =
        new_debouncer(std::time::Duration::from_millis(500), None, watch_tx).map_err(|e| format!("watcher: {e}"))?;
    debouncer
        .watch(local_dir, RecursiveMode::Recursive)
        .map_err(|e| format!("watch: {e}"))?;

    // Spawn the sync worker that processes file system events and sends TUI updates.
    let sync_tx = tui_tx.clone();
    let sync_local_dir = local_dir.to_path_buf();
    let _sync_remote_path = remote_path.to_string();
    let sync_state_path = state_path.to_path_buf();
    // We need to share SyncState between the sync task and the caller.
    // Use a mutex for simplicity since contention is minimal.
    let shared_state = Arc::new(tokio::sync::Mutex::new(state.clone()));
    let sync_shared_state = Arc::clone(&shared_state);

    tokio::spawn(async move {
        loop {
            // Block on the watcher channel (this is in a tokio task, but the
            // recv is blocking — that's fine because the TUI runs on
            // spawn_blocking and the sync worker is on a separate tokio task).
            let events = match watch_rx.recv() {
                Ok(Ok(events)) => events,
                Ok(Err(_)) => continue,
                Err(_) => break,
            };

            let changed: Vec<_> = events
                .iter()
                .filter_map(|e| e.paths.first().cloned())
                .filter(|p| !is_ignored_path(p))
                .collect();

            if changed.is_empty() {
                continue;
            }

            let now = chrono::Local::now().format("%H:%M:%S").to_string();

            // Without this the TUI's spinner would stay idle during a long sync.
            let _ = sync_tx.send(TuiEvent::SyncStarted(changed.len() as u32));

            for path in &changed {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                    status: FileEventStatus::Uploading,
                    filename: name,
                    size,
                    timestamp: now.clone(),
                }));
            }

            let resync_api = ApiClient::from_config();
            let binding_state = sync_shared_state.lock().await.clone();
            if let Err(e) = check_sync_account(&resync_api, &binding_state).await {
                let _ = sync_tx.send(TuiEvent::SyncError(e));
                break;
            }
            let resync_master = match load_master_key() {
                Ok(k) => Arc::new(k),
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            crate::temp_record::sweep();
            let mut resync_local = match walk_local_files(&sync_local_dir) {
                Ok(f) => f,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            let resync_folders = match walk_local_folders(&sync_local_dir) {
                Ok(f) => f,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };

            let mut resync_remote_files: HashMap<String, RemoteFile> = HashMap::new();
            let mut resync_remote_folders: HashMap<String, Uuid> = HashMap::new();
            let mut resync_health = crate::sync_safety::ListingHealth::default();
            let mut keys = match sync_keys(&binding_state) {
                Ok(k) => k,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            let scan_result = walk_remote(
                &resync_api,
                &resync_master,
                remote_folder_id,
                "",
                &mut resync_remote_files,
                &mut resync_remote_folders,
                &mut resync_health,
                &mut keys,
                None,
            )
            .await;
            if let Err(e) = scan_result {
                let _ = sync_tx.send(TuiEvent::SyncError(e));
                continue;
            }

            // Ensure remote folders exist.
            let mut all_dirs: HashSet<String> = HashSet::new();
            all_dirs.extend(resync_folders.iter().cloned());
            all_dirs.extend(resync_remote_folders.keys().cloned());
            let mut sorted_dirs: Vec<String> = all_dirs.into_iter().collect();
            sorted_dirs.sort_by_key(|s| s.matches('/').count());
            for folder_rel in &sorted_dirs {
                if !resync_remote_folders.contains_key(folder_rel) && listing_healthy(&resync_health) {
                    let pid = parent_remote_id(folder_rel, &resync_remote_folders, remote_folder_id);
                    let fname = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
                    match crate::item_id::create_owned_folder(&resync_api, &resync_master, fname, pid).await {
                        Ok(new_id) => {
                            resync_remote_folders.insert(folder_rel.clone(), new_id);
                        }
                        Err(e) => {
                            let _ = sync_tx.send(TuiEvent::SyncError(e));
                        }
                    }
                }
            }

            // Upload changed files.
            let mut sync_state = sync_shared_state.lock().await;
            let blocked = match refresh_watch_reads(
                &resync_api,
                &resync_master,
                &sync_local_dir,
                &mut sync_state,
                &mut resync_local,
                &resync_remote_files,
                remote_folder_id,
            )
            .await
            {
                Ok(b) => b,
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };

            // Reconcile remote deletions FIRST (shared helper — identical
            // decision to the one-shot path and the plain-watch loop): a
            // prior-synced file gone from the remote + byte-identical locally is
            // removed, so the upload loop below cannot resurrect it. Offline-
            // edited files are left for the upload pass to re-add.
            let absence = plan_absence(
                &resync_api,
                &sync_state,
                &resync_local,
                &resync_remote_files,
                &resync_health,
                root_unreconciled(&sync_state, remote_folder_id),
                false,
            )
            .await;
            if let Some(msg) = untrusted_notice(&absence, &sync_state, &resync_remote_files) {
                let _ = sync_tx.send(TuiEvent::SyncError(msg));
            }
            let reconciled = reconcile_remote_deletions(
                &sync_local_dir,
                &mut sync_state,
                &resync_local,
                &resync_remote_files,
                &absence,
                false,
                false,
            );
            for msg in &reconciled.notices {
                let _ = sync_tx.send(TuiEvent::SyncError(msg.clone()));
            }
            let removed = reconciled.removed;
            for rel in &removed {
                let file_name = rel.rsplit('/').next().unwrap_or(rel).to_string();
                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                    status: FileEventStatus::Done,
                    filename: format!("removed {file_name}"),
                    size: 0,
                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                }));
            }
            // Drop the just-removed keys from the pre-reconcile snapshot so the
            // upload loop below cannot re-upload (resurrect) a file we just
            // unlinked — otherwise that key (stale local=Some, remote=None,
            // prior=None) looks like a fresh change and emits a spurious error
            // event when do_upload fails on the missing file.
            for rel in &removed {
                resync_local.remove(rel);
            }

            let resync_keys: Vec<String> = resync_local.keys().cloned().collect();
            for rel in &resync_keys {
                if !listing_healthy(&resync_health)
                    || blocked.contains(rel)
                    || foreign_missing(&sync_state, rel, &resync_remote_files)
                {
                    continue;
                }
                let local = resync_local.get(rel).cloned();
                let remote = resync_remote_files.get(rel).cloned();
                let prior = trusted_prior(
                    &sync_state,
                    rel,
                    remote.is_some(),
                    root_unreconciled(&sync_state, remote_folder_id),
                );

                if let Some(l) = local {
                    // Same content-hash-aware detection as the one-shot + plain-
                    // watch paths (local_changed_vs_manifest): a same-size offline
                    // edit that survived the reconcile (ReAddLocal) is re-uploaded.
                    let local_changed = match &prior {
                        Some(p) => local_changed_vs_manifest(&l, p),
                        None => remote.is_none(),
                    };
                    if local_changed {
                        let file_name = rel.rsplit('/').next().unwrap_or(rel).to_string();
                        match do_upload(
                            &resync_api,
                            Arc::clone(&resync_master),
                            &sync_local_dir,
                            rel,
                            &l,
                            remote,
                            remote_folder_id,
                            &resync_remote_folders,
                            &mut sync_state,
                            false,
                        )
                        .await
                        {
                            Ok(()) => {
                                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                                    status: FileEventStatus::Done,
                                    filename: file_name,
                                    size: l.size,
                                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                }));
                            }
                            // SF-0002 r3 (K6): a read-only row's edit is kept, not failed.
                            Err(e) if e == crate::crypto::OWNER_REPAIR_READ_ONLY => {
                                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                                    status: FileEventStatus::Skipped,
                                    filename: file_name,
                                    size: l.size,
                                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                }));
                                let _ = sync_tx.send(TuiEvent::SyncNotice(read_only_notice(rel)));
                            }
                            Err(e) => {
                                let _ = sync_tx.send(TuiEvent::SyncUpdate(SyncFileEvent {
                                    status: FileEventStatus::Error,
                                    filename: file_name,
                                    size: l.size,
                                    timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
                                }));
                                let _ = sync_tx.send(TuiEvent::SyncError(e));
                            }
                        }
                    }
                }
            }

            let root_changed = root_unreconciled(&sync_state, remote_folder_id);
            finish_owned_sync(
                &resync_api,
                &resync_master,
                &mut keys,
                &mut sync_state,
                remote_folder_id,
                root_changed,
            )
            .await;
            sync_state.last_sync = Some(Utc::now());
            if let Ok(data) = serde_json::to_vec_pretty(&*sync_state) {
                let _ = std::fs::write(&sync_state_path, data);
            }

            // Compute updated totals (the sweep already ran at the top of this pass).
            let new_local = walk_local_files(&sync_local_dir).unwrap_or_default();
            let new_count = new_local.len() as u64;
            let new_bytes: u64 = new_local.values().map(|f| f.size).sum();
            drop(sync_state);

            let _ = sync_tx.send(TuiEvent::SyncFinished {
                file_count: new_count,
                total_bytes: new_bytes,
            });
        }
    });

    // Run the TUI on a blocking thread (it does terminal I/O).
    let tui_session_name = session_name.clone();
    let tui_result =
        tokio::task::spawn_blocking(move || crate::tui::app::run(tui_rx, tui_session_name, file_count, total_bytes))
            .await
            .map_err(|e| format!("TUI task panicked: {e}"))??;

    // Copy shared state back to caller's state.
    let final_state = shared_state.lock().await;
    *state = final_state.clone();

    let daemonized = match tui_result {
        TuiAction::Quit => {
            // Normal exit — the caller should stop the session.
            false
        }
        TuiAction::Daemonize => {
            // User pressed 'd' — spawn daemon and exit. The daemon process now
            // owns the session, so the caller must NOT stop it.
            let remote_str = remote_path.to_string();
            spawn_sync_daemon(local_dir, Some(&remote_str))?;
            true
        }
    };

    Ok(daemonized)
}

/// Spawn a sync daemon and optionally install platform auto-start.
///
/// Builds a session name from the local directory and remote path, computes
/// a filesystem slug, spawns `bb sync <local> <remote>` as a detached
/// process, writes a PID file, and installs a LaunchAgent (macOS) or
/// systemd user unit (Linux) for auto-start on login.
pub(super) fn spawn_sync_daemon(local_dir: &Path, remote_path: Option<&str>) -> Result<(), String> {
    let local_dir = std::fs::canonicalize(local_dir).map_err(|e| format!("cannot resolve path: {e}"))?;

    let remote = remote_path.unwrap_or("/");
    let remote_normalized = normalize_remote_path(remote);

    // Build a session-like name for the slug
    let device_info = crate::device::load_or_create();
    let session_name = format!(
        "{}/{}",
        device_info.hostname,
        remote_normalized.trim_start_matches('/').replace('/', "-")
    );
    let slug = crate::daemon::slugify(&session_name);

    if let Some(pid) = crate::daemon::is_daemon_running(&slug)? {
        println!(
            "  {} daemon already running (PID {})",
            "\u{25CF}".custom_color(crate::colors::GREEN_OK),
            pid,
        );
        println!("  {} bb sync --status", "status".custom_color(crate::colors::INK_DIM),);
        return Ok(());
    }

    let pid = crate::daemon::spawn_daemon(&local_dir, &remote_normalized, &slug)?;

    // Install platform auto-start (best-effort, non-fatal)
    if let Err(e) = crate::daemon::install_launchagent(&local_dir, &remote_normalized, &slug) {
        if !ui::is_quiet() {
            eprintln!("  {} LaunchAgent install: {e}", "!".custom_color(crate::colors::AMBER),);
        }
    }
    if let Err(e) = crate::daemon::install_systemd_unit(&local_dir, &remote_normalized, &slug) {
        if !ui::is_quiet() {
            eprintln!("  {} systemd unit install: {e}", "!".custom_color(crate::colors::AMBER),);
        }
    }

    // Success output
    println!(
        "  {} {} running as daemon (PID {})",
        "\u{25CF}".custom_color(crate::colors::GREEN_OK),
        session_name.custom_color(crate::colors::INK),
        pid,
    );
    println!(
        "  {} {} \u{2192} {}",
        "folder".custom_color(crate::colors::INK_DIM),
        local_dir.display().to_string().custom_color(crate::colors::INK),
        remote_normalized.custom_color(crate::colors::INK_DIM),
    );

    let log_dir = crate::daemon::log_dir()?;
    println!(
        "  {} {}",
        "logs".custom_color(crate::colors::INK_DIM),
        log_dir
            .join(format!("{slug}.log"))
            .display()
            .to_string()
            .custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {} bb sync --status to check, bb sync --stop-session \"{}\" to stop",
        "hint".custom_color(crate::colors::INK_DIM),
        session_name,
    );

    Ok(())
}
