use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::owned_keys::{CUSTODY_UNAVAILABLE, OwnedKeyResolver, ResolvedCliKey};
use crate::ui;
use beebeeb_core::client_meta::{ClientMetaField, fit_for_seal, open_client_meta, seal_client_meta};
use beebeeb_core::kdf::MasterKey;
use beebeeb_core::owned_keys::KeyOrigin;

const STATE_FILE: &str = ".bb-sync.json";
// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

/// What `bb sync` should do this invocation. The control modes return before
/// any tree work; `Run` is the one-shot or watch sync.
#[derive(Clone, Debug)]
pub enum SyncCommand {
    Run,
    Status,
    StopSession(String),
    StopAll,
    StopLegacy,
    Daemon,
}

/// The knobs that shape a sync pass, grouped so a call site names the policy
/// instead of a row of bare booleans.
#[derive(Clone, Debug)]
pub struct SyncOptions {
    pub dry_run: bool,
    pub force: bool,
    pub delete_remote: bool,
    pub once: bool,
    pub rehash: bool,
    pub allow_mass_delete: bool,
    pub rebind: bool,
    pub concurrency: usize,
}

impl SyncOptions {
    /// The `bb watch` redirect: a continuous pass with no flags.
    pub fn watch() -> Self {
        Self {
            dry_run: false,
            force: false,
            delete_remote: false,
            once: false,
            rehash: false,
            allow_mass_delete: false,
            rebind: false,
            concurrency: 4,
        }
    }
}

/// Which side a changed file moves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Skip,
    Upload,
    Download,
    Conflict,
}

/// What to do when both sides changed since the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConflictPolicy {
    Skip,
    Force,
}

/// The one conflict rule: a file changed on one side moves that way; changed
/// on both is a conflict unless the policy forces the local copy.
pub(super) fn decide(local_changed: bool, remote_changed: bool, policy: ConflictPolicy) -> Direction {
    match (local_changed, remote_changed) {
        (false, false) => Direction::Skip,
        (true, false) => Direction::Upload,
        (false, true) => Direction::Download,
        (true, true) => match policy {
            ConflictPolicy::Force => Direction::Upload,
            ConflictPolicy::Skip => Direction::Conflict,
        },
    }
}




























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
            return crate::daemon::uninstall_launchagent("sync").map(|removed| {
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
            Err(e) if e.message == crate::api::SESSION_EXPIRED_MESSAGE => return Err(crate::exit::sync_error(e.to_string())),
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
    let policy = if force { ConflictPolicy::Force } else { ConflictPolicy::Skip };

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
    {
        use futures_util::stream::{self, StreamExt};

        // Rich + TTY → byte-accurate bars (one overall + transient per-file).
        // Otherwise a no-op sink, and the plain per-file lines below stay.
        let show_bars = scan_active && !dry_run && pending_count > 0;
        let progress: Box<dyn crate::upload::ChunkProgress> = if show_bars {
            let total_expected: u64 = pending_uploads
                .iter()
                .map(|(_, l, _)| crate::upload::expected_ciphertext_for(l.size, concurrency as u32))
                .sum();
            Box::new(crate::upload::BarProgress::new(pending_count as u64, total_expected))
        } else {
            Box::new(crate::upload::NoopProgress)
        };

        let upload_results: Vec<Result<(String, FileEntry, u64), String>> =
            stream::iter(pending_uploads.into_iter().map(|(rel, local, replace_id)| {
                let api = &api;
                let master_key = Arc::clone(&master_key);
                let local_dir = &local_dir;
                let remote_folders = &remote_folders;
                let resolved = remote_files.get(&rel).and_then(|r| r.resolved.clone());
                let progress = progress.as_ref();
                let shutdown = Arc::clone(&shutdown);
                async move {
                    if shutdown.load(Ordering::Relaxed) {
                        return Err(crate::upload::INTERRUPTED.to_string());
                    }

                    // Plain status line only when bars are not showing them.
                    if !show_bars && !ui::is_json() && !ui::is_quiet() {
                        let size_str = format_size(local.size);
                        println!(
                            "  {} {} {}",
                            "\u{2191}".custom_color(crate::colors::GREEN_OK),
                            rel.custom_color(crate::colors::INK),
                            format!("({size_str})").custom_color(crate::colors::INK_DIM),
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
                    let replacement =
                        match replace_decision(api, &master_key, resolved.as_ref(), replace_id.is_some()).await {
                            Replace::Skip(why) if why == crate::crypto::OWNER_REPAIR_READ_ONLY => {
                                return Err(format!("{READ_ONLY_KEPT}{rel}"));
                            }
                            Replace::Skip(why) => return Err(format!("{rel}: {why}")),
                            Replace::InPlace(r) => Some(r.clone()),
                            Replace::NewFile => None,
                        };
                    // Not a version of the old remote: it is uploaded as a new
                    // file and the old one trashed only AFTER that (L5).
                    let replaced = replace_id.filter(|_| replacement.is_none());

                    let parent_id = match rel.rsplit_once('/') {
                        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
                        None => Some(remote_folder_id),
                    }
                    .or(Some(remote_folder_id));

                    let file_name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                    let spec = crate::upload::UploadSpec {
                        path: local_dir.join(&rel),
                        file_name,
                        file_id: if replacement.is_some() {
                            replace_id.ok_or("missing replacement id")?
                        } else {
                            Uuid::new_v4()
                        },
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
                        shutdown,
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

        // Clear the bars before printing the summary / any errors.
        progress.finish_all();

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
    for (rel, remote) in &pending_downloads {
        match do_download(&api, &master_key, &local_dir, rel, remote, &mut state, dry_run).await? {
            DownloadOutcome::Downloaded => down_count += 1,
            // Still uploading elsewhere (or unsafe destination); nothing written.
            DownloadOutcome::Deferred => skipped += 1,
            // The remote copy is untouched; the next pass tries again (task 1916).
            DownloadOutcome::Unreadable => failed += 1,
        }
    }

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
    let elapsed_secs = elapsed.as_secs_f64();

    // A pass that left uploads failed or conflicts unresolved is not "synced".
    // A one-shot run (`--once`, json/quiet, …) exits 3 so scripts and cron jobs
    // notice (flow-6: it used to print "✓ synced" and exit 0). A dry run only
    // previews, and the watch loop keeps retrying, so neither fails here.
    let incomplete = failed > 0 || conflicts > 0 || held_back > 0 || read_only_kept > 0 || delete_refused;
    let incomplete_err = || {
        crate::exit::with_code(
            crate::exit::INCOMPLETE,
            format!(
                "sync incomplete: {} \u{2014} fix the cause and re-run{}",
                match incomplete_summary(failed, conflicts, held_back, read_only_kept) {
                    s if s.is_empty() && delete_refused => "--delete refused on a changed remote folder".to_string(),
                    s => s,
                },
                if delete_refused {
                    " (`bb sync --rebind` accepts the new remote folder before --delete may trash anything)"
                } else if held_back > 0 {
                    " (`--allow-mass-delete` confirms the held-back removals)"
                } else if conflicts > 0 {
                    " (`--force` overwrites conflicts with the local copy)"
                } else if read_only_kept > 0 {
                    " (save each read-only file's edit under a new name; sync uploads it as a new file)"
                } else {
                    ""
                }
            ),
        )
    };
    let fail_one_shot = incomplete && !dry_run && !will_enter_watch;

    if ui::is_json() {
        let json_out = serde_json::json!({
            "ok": !incomplete,
            "uploaded": up_count,
            "downloaded": down_count,
            "failed": failed,
            "conflicts": conflicts,
            "deleted": deletes,
            "held_back": held_back,
            "read_only_kept": read_only_kept,
            "skipped": skipped,
            "index_written": index_report.written,
            "index_deferred": index_report.deferred,
            "index_partial": index_report.partial,
            "bytes": total_bytes,
            "elapsed_secs": elapsed_secs,
        });
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return if fail_one_shot { Err(incomplete_err()) } else { Ok(()) };
    }

    if ui::is_quiet() {
        println!(
            "{} up {} down {} conflict{}{}",
            up_count,
            down_count,
            conflicts,
            if failed > 0 {
                format!(" {failed} failed")
            } else {
                String::new()
            },
            if deletes > 0 {
                format!(" {deletes} del")
            } else {
                String::new()
            },
        );
        return if fail_one_shot { Err(incomplete_err()) } else { Ok(()) };
    }

    println!();
    // Rich summary: ✓ synced · 2 ↑ 1 ↓ 1 ⚡ · 3.3 MB · avg 45 MB/s · 1.8s
    let avg_speed = if elapsed_secs > 0.0 {
        total_bytes as f64 / elapsed_secs
    } else {
        0.0
    };

    let mut parts: Vec<String> = Vec::new();
    if up_count > 0 {
        parts.push(format!(
            "{}",
            format!("{up_count} \u{2191}").custom_color(crate::colors::GREEN_OK)
        ));
    }
    if down_count > 0 {
        parts.push(format!(
            "{}",
            format!("{down_count} \u{2193}").custom_color(crate::colors::CYAN)
        ));
    }
    if conflicts > 0 {
        parts.push(format!(
            "{}",
            format!("{conflicts} \u{26A1}").custom_color(crate::colors::AMBER)
        ));
    }
    if failed > 0 {
        parts.push(format!(
            "{}",
            format!("{failed} failed").custom_color(crate::colors::RED_ERR)
        ));
    }
    if held_back > 0 {
        parts.push(format!(
            "{}",
            format!("{held_back} held back").custom_color(crate::colors::AMBER)
        ));
    }
    if read_only_kept > 0 {
        parts.push(format!(
            "{}",
            format!("{read_only_kept} read-only").custom_color(crate::colors::AMBER)
        ));
    }
    if deletes > 0 {
        parts.push(format!(
            "{}",
            format!("{deletes} \u{2717}").custom_color(crate::colors::RED_ERR)
        ));
    }

    let counts_str = if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(" ")
    };

    let mut meta_parts: Vec<String> = Vec::new();
    if total_bytes > 0 {
        meta_parts.push(ui::human_size(total_bytes));
    }
    if avg_speed > 0.0 && total_bytes > 0 {
        meta_parts.push(format!("avg {}", ui::human_speed(avg_speed)));
    }
    meta_parts.push(format!("{:.1}s", elapsed_secs));

    let (mark, verdict) = if incomplete {
        (
            "!".custom_color(crate::colors::AMBER),
            "incomplete".custom_color(crate::colors::AMBER),
        )
    } else {
        (
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            "synced".custom_color(crate::colors::GREEN_OK),
        )
    };
    println!(
        "  {} {} {} {} {} {}",
        mark,
        verdict,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        counts_str,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        meta_parts.join(" \u{00b7} ").custom_color(crate::colors::INK_DIM),
    );

    if fail_one_shot {
        return Err(incomplete_err());
    }

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


#[cfg(test)]
mod tests {
    use super::*;

    /// SF-0002 r3 (K6): the notice every sync path prints for a kept edit of a
    /// read-only row, word for word, and the pass summary counts it as kept,
    /// not failed.
    #[test]
    fn a_read_only_edit_is_a_notice_with_guidance() {
        assert_eq!(
            read_only_notice("Scans/receipt.pdf"),
            "Scans/receipt.pdf: This file was saved by an older version of the iOS app. You can open and \
             download it. To change it, save a copy. Your edit stays on this computer: save it under a new name \
             and sync uploads that as a new file."
        );
        assert_eq!(incomplete_summary(0, 0, 0, 1), "1 read-only edit kept");
        assert_eq!(
            incomplete_summary(1, 0, 0, 2),
            "1 failed \u{00b7} 2 read-only edits kept"
        );
    }

    /// Make a unique scratch directory under the system temp dir (no extra
    /// test-only dependency). Caller removes it.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-sync-test-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(dir: &Path, rel: &str, contents: &[u8]) -> LocalFile {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        LocalFile {
            mtime,
            size: meta.len(),
            content_hash: compute_file_hash(&path).unwrap(),
        }
    }

    /// A manifest entry matching the given local file (the post-sync state).
    fn entry_for(local: &LocalFile) -> FileEntry {
        FileEntry {
            remote_id: Uuid::new_v4(),
            remote_version: None,
            last_mtime: local.mtime,
            last_size: local.size,
            last_sync: Utc::now(),
            content_hash: Some(local.content_hash.clone()),
        }
    }

    fn remote_file() -> RemoteFile {
        RemoteFile {
            id: Uuid::new_v4(),
            resolved: None,
            chunk_count: 1,
            updated_at: Utc::now(),
            pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
        }
    }

    /// Listing trusted, and the server reports EVERY manifest entry trashed.
    fn plan_all(state: &SyncState) -> AbsencePlan {
        AbsencePlan {
            tombstones: state.files.values().map(|e| e.remote_id).collect(),
            untrusted: None,
        }
    }

    /// Listing trusted, but the server reports nothing trashed.
    fn plan_none() -> AbsencePlan {
        AbsencePlan {
            tombstones: HashSet::new(),
            untrusted: None,
        }
    }

    /// `n` previously-synced files `f0.txt`.. on disk and in the manifest.
    fn synced_files(dir: &Path, n: usize) -> (SyncState, HashMap<String, LocalFile>) {
        let mut state = SyncState::default();
        let mut local_files = HashMap::new();
        for i in 0..n {
            let rel = format!("f{i}.txt");
            let local = write_file(dir, &rel, format!("content {i}").as_bytes());
            state.files.insert(rel.clone(), entry_for(&local));
            local_files.insert(rel, local);
        }
        (state, local_files)
    }

    fn recovered_names(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![dir.join(crate::sync_safety::RECOVERY_DIR)];
        while let Some(p) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&p) else { continue };
            for e in rd.flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        out.sort();
        out
    }

    // ── pure decision function ─────────────────────────────────────────────

    #[test]
    fn classify_prior_synced_unchanged_with_tombstone_is_remote_delete() {
        let dir = scratch_dir("classify-del");
        let local = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&local);
        assert_eq!(
            classify_remote_absent(Some(&local), Some(&entry), true),
            RemoteAbsent::DeleteLocal,
            "a prior-synced, byte-identical local file the server reports trashed is a remote delete"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_prior_synced_modified_is_readd() {
        let dir = scratch_dir("classify-readd");
        let original = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&original);
        // Simulate an offline edit: same path, different bytes → different hash.
        let edited = write_file(&dir, "a.txt", b"hello, edited offline");
        assert_ne!(edited.content_hash, entry.content_hash.clone().unwrap());
        assert_eq!(
            classify_remote_absent(Some(&edited), Some(&entry), true),
            RemoteAbsent::ReAddLocal,
            "a locally-modified file must NOT be deleted on remote-absence"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_new_local_is_not_a_delete() {
        let dir = scratch_dir("classify-new");
        let local = write_file(&dir, "fresh.txt", b"brand new");
        assert_eq!(
            classify_remote_absent(Some(&local), None, true),
            RemoteAbsent::NotADelete,
            "a genuinely-new local file (no manifest entry) is an upload, not a delete"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn local_matches_entry_uses_hash_then_size_mtime() {
        let dir = scratch_dir("match");
        let local = write_file(&dir, "a.txt", b"data");
        let mut entry = entry_for(&local);
        assert!(local_matches_entry(&local, &entry));
        // Hash mismatch → not a match (even if size/mtime line up).
        entry.content_hash = Some("deadbeef".into());
        assert!(!local_matches_entry(&local, &entry));
        // No stored hash → fall back to (mtime, size).
        entry.content_hash = None;
        assert!(local_matches_entry(&local, &entry));
        entry.last_size += 1;
        assert!(!local_matches_entry(&local, &entry));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── reconcile helper (used by both watch loops) ────────────────────────

    /// (a) prior-synced + remote gone + local byte-identical
    ///     ⇒ local removed, manifest entry removed, nothing re-uploaded.
    #[test]
    fn reconcile_removes_byte_identical_remote_delete() {
        let dir = scratch_dir("reconcile-del");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert_eq!(removed, vec!["a.txt".to_string()]);
        assert!(!dir.join("a.txt").exists(), "local mirror must leave the sync folder");
        assert_eq!(recovered_names(&dir), vec!["a.txt".to_string()], "and be recoverable");
        assert!(!state.files.contains_key("a.txt"), "manifest entry must be dropped");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// (b) prior-synced + remote gone but local MODIFIED offline
    ///     ⇒ NOT deleted, manifest entry retained (re-add path).
    #[test]
    fn reconcile_keeps_offline_modified_file() {
        let dir = scratch_dir("reconcile-keep");
        let original = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&original);
        // Edit offline AFTER the manifest snapshot.
        let edited = write_file(&dir, "a.txt", b"hello, locally edited");

        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry);

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), edited);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert!(removed.is_empty(), "an offline-edited file must NOT be removed");
        assert!(dir.join("a.txt").exists(), "local edit must survive reconciliation");
        assert!(
            state.files.contains_key("a.txt"),
            "manifest entry retained so the upload pass re-adds it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// (c) genuinely-new local file (no manifest entry)
    ///     ⇒ untouched by reconciliation (it is uploaded by the normal pass).
    #[test]
    fn reconcile_ignores_new_local_file() {
        let dir = scratch_dir("reconcile-new");
        let local = write_file(&dir, "new.txt", b"fresh content");

        let mut state = SyncState::default(); // no manifest entry for new.txt
        let mut local_files = HashMap::new();
        local_files.insert("new.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert!(removed.is_empty(), "a new local file is never a remote delete");
        assert!(dir.join("new.txt").exists(), "new local file must remain on disk");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file still present remotely is never touched by reconciliation.
    #[test]
    fn reconcile_leaves_present_remote_alone() {
        let dir = scratch_dir("reconcile-present");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), local);
        let mut remote_files = HashMap::new();
        remote_files.insert("a.txt".to_string(), remote_file()); // still on server

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert!(removed.is_empty());
        assert!(dir.join("a.txt").exists());
        assert!(state.files.contains_key("a.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Manifest entry whose local file is already gone (deleted both sides):
    /// no local unlink needed, but the stale manifest entry is dropped.
    #[test]
    fn reconcile_drops_manifest_when_local_already_gone() {
        let dir = scratch_dir("reconcile-gone");
        // Build an entry for a file that does NOT exist on disk.
        let phantom = LocalFile {
            mtime: 0,
            size: 5,
            content_hash: "00".into(),
        };
        let mut state = SyncState::default();
        state.files.insert("gone.txt".to_string(), entry_for(&phantom));

        let local_files: HashMap<String, LocalFile> = HashMap::new(); // not on disk
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // not on server

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false).removed;

        assert_eq!(removed, vec!["gone.txt".to_string()]);
        assert!(!state.files.contains_key("gone.txt"), "stale manifest entry dropped");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// dry_run must be side-effect-free: no unlink, no manifest mutation.
    #[test]
    fn reconcile_dry_run_is_side_effect_free() {
        let dir = scratch_dir("reconcile-dry");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        let mut local_files = HashMap::new();
        local_files.insert("a.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, true).removed;

        // It still reports the rel, but touches nothing.
        assert_eq!(removed, vec!["a.txt".to_string()]);
        assert!(dir.join("a.txt").exists(), "dry-run must not unlink");
        assert!(state.files.contains_key("a.txt"), "dry-run must not mutate manifest");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── reconcile → upload-snapshot prune (the watch-loop integration) ─────

    /// Models exactly what both watch loops do after a remote delete: capture the
    /// `removed` Vec, prune those keys from the pre-reconcile `resync_local`
    /// snapshot, then run the change-detection that drives the upload loop. The
    /// deleted file must be ABSENT from the pruned snapshot, so no upload is even
    /// considered for it (the BLOCKER: a stale snapshot re-uploaded the unlinked
    /// file every cycle, resurrecting the remote delete and erroring on the
    /// now-missing path).
    #[test]
    fn reconcile_then_prune_drops_deleted_key_from_upload_snapshot() {
        let dir = scratch_dir("prune-snapshot");
        let local = write_file(&dir, "a.txt", b"hello");
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry_for(&local));

        // The pre-reconcile snapshot the upload loop iterates.
        let mut resync_local = HashMap::new();
        resync_local.insert("a.txt".to_string(), local);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &resync_local, &remote_files, &plan, false, false).removed;
        assert_eq!(removed, vec!["a.txt".to_string()]);
        // Local mirror + manifest entry are gone after reconcile.
        assert!(!dir.join("a.txt").exists());
        assert!(!state.files.contains_key("a.txt"));

        // The fix: prune removed keys from the snapshot BEFORE the upload loop.
        for rel in &removed {
            resync_local.remove(rel);
        }
        assert!(
            !resync_local.contains_key("a.txt"),
            "deleted file must be absent from the pruned upload snapshot"
        );

        // Replay the watch upload loop's decision over the pruned snapshot: it
        // must consider ZERO uploads (nothing left to resurrect).
        let mut would_upload: Vec<String> = Vec::new();
        for (rel, l) in &resync_local {
            let remote_present = remote_files.contains_key(rel);
            let changed = match state.files.get(rel) {
                Some(p) => local_changed_vs_manifest(l, p),
                None => !remote_present,
            };
            if changed {
                would_upload.push(rel.clone());
            }
        }
        assert!(
            would_upload.is_empty(),
            "no upload may be attempted for the just-deleted file; got {would_upload:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Data-safety invariant + the HIGH change-detection fix together: a
    /// SAME-SIZE offline edit (identical byte length, different content → different
    /// hash) of a remote-deleted file must (a) survive reconciliation (ReAddLocal,
    /// never deleted) and (b) be detected as changed by `local_changed_vs_manifest`
    /// so the watch upload loop re-uploads it. A bare `(mtime,size)` compare —
    /// the old watch-loop bug — would have missed it and silently lost the edit.
    #[test]
    fn same_size_offline_edit_survives_and_is_re_uploaded() {
        let dir = scratch_dir("same-size-edit");
        // 5-byte original, manifest snapshot taken against it.
        let original = write_file(&dir, "a.txt", b"aaaaa");
        let entry = entry_for(&original);
        // Offline edit: SAME 5-byte size, different content → different hash.
        // Force the mtime to match the manifest so a (mtime,size) compare can't
        // tell them apart — only the content hash distinguishes them.
        let mut edited = write_file(&dir, "a.txt", b"bbbbb");
        edited.mtime = entry.last_mtime;
        assert_eq!(edited.size, original.size, "test premise: identical size");
        assert_eq!(edited.mtime, entry.last_mtime, "test premise: identical mtime");
        assert_ne!(edited.content_hash, original.content_hash, "content differs");

        // (a) reconcile must KEEP it (ReAddLocal), not delete it.
        let mut state = SyncState::default();
        state.files.insert("a.txt".to_string(), entry.clone());
        let mut resync_local = HashMap::new();
        resync_local.insert("a.txt".to_string(), edited.clone());
        let remote_files: HashMap<String, RemoteFile> = HashMap::new(); // remote gone

        let plan = plan_all(&state);
        let removed =
            reconcile_remote_deletions(&dir, &mut state, &resync_local, &remote_files, &plan, false, false).removed;
        assert!(removed.is_empty(), "an offline edit must never be deleted");
        assert!(dir.join("a.txt").exists(), "the local edit must survive on disk");
        assert!(state.files.contains_key("a.txt"), "manifest entry retained for re-add");

        // Pruning removes nothing (it wasn't deleted), so the key stays.
        for rel in &removed {
            resync_local.remove(rel);
        }
        assert!(resync_local.contains_key("a.txt"));

        // (b) the change detector that gates the upload loop must flag it.
        assert!(
            local_changed_vs_manifest(&edited, &entry),
            "a same-size/same-mtime content edit MUST be detected as changed (hash compare)"
        );
        // And the bare (mtime,size) compare the old watch loop used would NOT
        // have — proving the bug this fix closes.
        let old_buggy_compare = edited.mtime != entry.last_mtime || edited.size != entry.last_size;
        assert!(
            !old_buggy_compare,
            "the old (mtime,size)-only watch compare misses this edit — that was the bug"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── tasks 1915/1916: absence is not deletion ───────────────────────────

    #[test]
    fn classify_identical_without_tombstone_is_kept_not_deleted() {
        let dir = scratch_dir("classify-unconfirmed");
        let local = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&local);
        assert_eq!(
            classify_remote_absent(Some(&local), Some(&entry), false),
            RemoteAbsent::KeepUnconfirmed,
            "absent from the listing but no deletion record: keep"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_edited_file_is_readded_whether_or_not_tombstoned() {
        let dir = scratch_dir("classify-edit-both");
        let original = write_file(&dir, "a.txt", b"hello");
        let entry = entry_for(&original);
        let edited = write_file(&dir, "a.txt", b"hello again, different");
        for tomb in [true, false] {
            assert_eq!(
                classify_remote_absent(Some(&edited), Some(&entry), tomb),
                RemoteAbsent::ReAddLocal
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn classify_missing_local_only_forgets_the_manifest_entry() {
        let dir = scratch_dir("classify-forget");
        let phantom = write_file(&dir, "p.txt", b"x");
        let entry = entry_for(&phantom);
        for tomb in [true, false] {
            assert_eq!(
                classify_remote_absent(None, Some(&entry), tomb),
                RemoteAbsent::ForgetStale
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// No tombstone => nothing is touched, however absent the file looks.
    #[test]
    fn reconcile_keeps_a_file_without_a_tombstone() {
        let dir = scratch_dir("reconcile-no-tomb");
        let (mut state, local_files) = synced_files(&dir, 3);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(
            &dir,
            &mut state,
            &local_files,
            &remote_files,
            &plan_none(),
            false,
            false,
        );

        assert!(r.removed.is_empty());
        assert_eq!(state.files.len(), 3, "manifest untouched");
        for i in 0..3 {
            assert!(dir.join(format!("f{i}.txt")).exists());
        }
        assert!(recovered_names(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tombstone removes THAT file only, and into the recoverable folder.
    #[test]
    fn reconcile_moves_only_the_tombstoned_file_to_recovery() {
        let dir = scratch_dir("reconcile-one-tomb");
        let (mut state, local_files) = synced_files(&dir, 3);
        let plan = AbsencePlan {
            tombstones: [state.files["f1.txt"].remote_id].into_iter().collect(),
            untrusted: None,
        };
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert_eq!(r.removed, vec!["f1.txt".to_string()]);
        assert!(!dir.join("f1.txt").exists());
        assert!(dir.join("f0.txt").exists() && dir.join("f2.txt").exists());
        assert!(!state.files.contains_key("f1.txt") && state.files.len() == 2);
        assert_eq!(
            recovered_names(&dir),
            vec!["f1.txt".to_string()],
            "moved aside, not unlinked"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 60 tombstones of 60 tracked is over max(50, 10 %): remove nothing.
    #[test]
    fn reconcile_breaker_holds_back_a_mass_delete() {
        let dir = scratch_dir("reconcile-breaker");
        let (mut state, local_files) = synced_files(&dir, 60);
        let plan = plan_all(&state);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert!(
            r.notices
                .iter()
                .any(|n| n.contains("mass delete held back") && n.contains("--allow-mass-delete")),
            "the trip must be explained: {:?}",
            r.notices
        );
        assert!(r.removed.is_empty());
        assert_eq!(state.files.len(), 60);
        for i in 0..60 {
            assert!(dir.join(format!("f{i}.txt")).exists(), "f{i}.txt must survive");
        }

        // Explicit confirmation lets it through - still recoverably.
        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, true, false);
        assert_eq!(r.removed.len(), 60);
        assert!(!r.notices.iter().any(|n| n.contains("mass delete held back")));
        assert_eq!(recovered_names(&dir).len(), 60);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Exactly the limit (50) is allowed; the breaker trips strictly above it.
    #[test]
    fn reconcile_breaker_allows_exactly_the_limit() {
        let dir = scratch_dir("reconcile-breaker-edge");
        let (mut state, local_files) = synced_files(&dir, 60);
        let plan = AbsencePlan {
            tombstones: (0..50).map(|i| state.files[&format!("f{i}.txt")].remote_id).collect(),
            untrusted: None,
        };
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert_eq!(
            r.removed.len(),
            50,
            "50 tombstones of 60 tracked is within max(50, 10%)"
        );
        assert!(!r.notices.iter().any(|n| n.contains("mass delete held back")));
        assert_eq!(state.files.len(), 10);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The scan can be hours old: a file edited since must be kept, not moved.
    #[test]
    fn reconcile_keeps_a_file_edited_after_the_scan() {
        let dir = scratch_dir("reconcile-edited-since");
        let (mut state, local_files) = synced_files(&dir, 1);
        // `local_files` (the scan) still says identical; the disk has moved on.
        std::fs::write(dir.join("f0.txt"), b"typed after the scan began").unwrap();
        let plan = plan_all(&state);
        let remote_files: HashMap<String, RemoteFile> = HashMap::new();

        let r = reconcile_remote_deletions(&dir, &mut state, &local_files, &remote_files, &plan, false, false);

        assert!(r.removed.is_empty());
        assert_eq!(
            std::fs::read(dir.join("f0.txt")).unwrap(),
            b"typed after the scan began"
        );
        assert!(state.files.contains_key("f0.txt"));
        assert!(recovered_names(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recovery_folder_is_never_sync_input() {
        assert!(is_ignored_path(Path::new(
            "/home/u/Docs/.bb-trash/20261017-120000/a.txt"
        )));
        assert!(is_ignored_path(Path::new("/home/u/Docs/.bb-trash")));
        assert!(!is_ignored_path(Path::new("/home/u/Docs/a.txt")));
    }

    #[test]
    fn root_is_unreconciled_only_when_bound_to_another_folder() {
        let bound = Uuid::new_v4();
        let mut state = SyncState::default();
        assert!(
            !root_unreconciled(&state, bound),
            "an unbound directory binds on its first sync"
        );
        state.remote_folder_id = Some(bound);
        assert!(!root_unreconciled(&state, bound));
        assert!(
            root_unreconciled(&state, Uuid::new_v4()),
            "bound to another folder: removals stay blocked"
        );
    }

    #[test]
    fn decrypt_failures_are_told_apart_from_other_errors() {
        assert!(is_decrypt_failure("decrypt chunk 0: aead error"));
        assert!(is_decrypt_failure("key derivation failed"));
        assert!(!is_decrypt_failure("download failed (500): oops"));
        assert!(!is_decrypt_failure("download truncated: chunk 1/3"));
    }

    #[test]
    fn a_409_is_a_file_still_being_uploaded_and_nothing_else_is() {
        assert!(is_still_uploading(
            "download failed (409 Conflict): replace_in_progress"
        ));
        assert!(is_still_uploading("upload is still in progress"));
        // Random ids in an error text must not decide it (a uuid may contain "409").
        assert!(!is_still_uploading(
            "decrypt chunk 0 of 7b0c1a52-3f4e-4409-9c61-2f8e5a1d0b37: aead error"
        ));
        assert!(!is_still_uploading(
            "download failed (500 Internal Server Error): 409 widgets"
        ));
        assert!(!is_still_uploading("download failed (403 Forbidden)"));
    }

    #[test]
    fn a_typed_download_failure_ends_one_files_turn_not_the_pass() {
        use crate::download::DownloadError;
        // A chunk that never authenticates: the same outcome as a decrypt failure on the API path.
        assert_eq!(
            DownloadOutcome::of_failure(&DownloadError::Unreadable("x".into())),
            Some(DownloadOutcome::Unreadable)
        );
        // The version changed under the download: not this pass; the next one reads the new version.
        assert_eq!(
            DownloadOutcome::of_failure(&DownloadError::Changed("x".into())),
            Some(DownloadOutcome::Deferred)
        );
        // Everything else is the caller's, as before (an expired session must still stop the pass).
        assert_eq!(DownloadOutcome::of_failure(&DownloadError::Other("x".into())), None);
    }

    // ── walk_remote: what the listing could not show us ────────────────────

    /// In-process `GET /api/v1/files` that answers every request with `entries`.
    async fn mock_listing(entries: Vec<serde_json::Value>) -> ApiClient {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/api/v1/files",
            get(move || {
                let entries = entries.clone();
                async move { axum::Json(serde_json::json!({ "files": entries })) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        ApiClient::new_for_test(format!("http://{addr}"))
    }

    fn entry(name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": Uuid::new_v4().to_string(),
            "name_encrypted": name,
            "is_folder": false,
            "chunk_count": 1,
            "updated_at": "2020-01-01T00:00:00+00:00",
        })
    }

    async fn walk(api: &ApiClient) -> (HashMap<String, RemoteFile>, crate::sync_safety::ListingHealth) {
        let mk = beebeeb_core::kdf::MasterKey::from_bytes([7u8; 32]);
        let mut files = HashMap::new();
        let mut folders = HashMap::new();
        let mut health = crate::sync_safety::ListingHealth::default();
        walk_remote(
            api,
            &mk,
            Uuid::new_v4(),
            "",
            &mut files,
            &mut folders,
            &mut health,
            &mut OwnedKeyResolver::new(),
            None,
        )
        .await
        .unwrap();
        (files, health)
    }

    #[tokio::test]
    async fn walk_remote_reports_a_complete_listing_as_healthy() {
        let api = mock_listing(vec![entry("a.txt"), entry("b.txt")]).await;
        let (files, health) = walk(&api).await;
        assert_eq!(files.len(), 2);
        assert_eq!(health, crate::sync_safety::ListingHealth::default());
    }

    #[tokio::test]
    async fn walk_remote_counts_names_it_cannot_read() {
        // `{...` is not a plaintext name and not a valid blob: undecryptable.
        // `../x` and `a/b` are unsafe as local path segments.
        let api = mock_listing(vec![
            entry("ok.txt"),
            entry("{not-a-blob"),
            entry("../escape.txt"),
            entry("a/b.txt"),
        ])
        .await;
        let (files, health) = walk(&api).await;
        assert_eq!(files.keys().cloned().collect::<Vec<_>>(), vec!["ok.txt".to_string()]);
        assert_eq!(health.unreadable, 3, "three entries were skipped");
        assert!(!health.truncated);
    }

    #[tokio::test]
    async fn walk_remote_flags_a_listing_cut_short_by_the_cap() {
        // 50,001 entries in one page: the client's 50,000 safety cap cuts it.
        let entries: Vec<_> = (0..50_001).map(|i| entry(&format!("f{i}.txt"))).collect();
        let api = mock_listing(entries).await;
        let (files, health) = walk(&api).await;
        assert_eq!(files.len(), 50_000);
        assert!(health.truncated, "a capped listing must be marked incomplete");
    }
}

#[cfg(test)]
mod l13_trash_after_upload_tests {
    use super::*;
    use crate::test_server::{Mock, Scratch, Shared, shared, spawn};

    const REL: &str = "payload.bin";

    /// Upload the scratch file through `do_upload` as the NEW copy of the remote
    /// `old` (its key is derived, so it is not versioned: the old one is to be
    /// trashed). SF-0002 r3: `old` is resolved as the walk resolves it, and its
    /// version is served, so the write check proves it writable first.
    async fn upload_over(mut mock: Mock, s: &Scratch, old: Uuid) -> (Result<(), String>, SyncState, Shared) {
        let (path, size, _mtime) = s.file(4096);
        let mtime = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let local = LocalFile {
            mtime,
            size,
            content_hash: compute_file_hash(&path).unwrap(),
        };
        let master = Arc::new(beebeeb_core::kdf::MasterKey::from_bytes([5u8; 32]));
        let old_version = b"the old remote's content";
        let old_key = || beebeeb_core::kdf::derive_file_key(&master, old.to_string().as_bytes());
        mock.downloads.insert(
            old.to_string(),
            beebeeb_core::encrypt::encrypt_chunk_raw(&old_key(), old_version).unwrap(),
        );
        let remote = RemoteFile {
            id: old,
            resolved: Some(ResolvedCliKey {
                key: Arc::new(old_key()),
                origin: KeyOrigin::Derived,
                canonical: serde_json::json!({
                    "id": old.to_string(),
                    "size_bytes": old_version.len(),
                    "chunk_count": 1,
                }),
                pending_owner_wrap: None,
                shared_root_id: None,
                name: REL.to_string(),
                read_only: false,
            }),
            chunk_count: 1,
            updated_at: Utc::now(),
            pin: Ok(beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded),
        };
        let mock = shared(mock);
        let api = ApiClient::new_for_test(spawn(mock.clone()).await);
        let mut state = SyncState::default();
        let result = do_upload(
            &api,
            master,
            &s.dir,
            REL,
            &local,
            Some(remote),
            Uuid::new_v4(),
            &HashMap::new(),
            &mut state,
            false,
        )
        .await;
        (result, state, mock)
    }

    /// L5: the new copy's init echoes another id. The old remote is NOT
    /// trashed, nothing is PUT, and the state keeps no entry for the path.
    #[tokio::test]
    async fn an_init_echo_mismatch_leaves_the_old_remote_untrashed() {
        let s = Scratch::new("l5-init");
        let old = Uuid::new_v4();
        let (result, state, mock) = upload_over(
            Mock {
                init_echo: Some(Uuid::new_v4().to_string()),
                ..Default::default()
            },
            &s,
            old,
        )
        .await;
        let err = result.expect_err("the upload aborts");
        assert!(err.contains(crate::item_id::CHANGED_ID_MESSAGE), "{err}");
        let m = mock.lock().unwrap();
        assert!(m.trashed.is_empty(), "the old remote is not trashed: {:?}", m.trashed);
        assert!(m.chunk_puts.is_empty());
        assert!(!state.files.contains_key(REL));
    }

    /// L5: the completion answer names another item. The old remote is still
    /// not trashed.
    #[tokio::test]
    async fn a_completion_echo_mismatch_leaves_the_old_remote_untrashed() {
        let s = Scratch::new("l5-complete");
        let old = Uuid::new_v4();
        let (result, state, mock) = upload_over(
            Mock {
                complete_echo: Some(Uuid::new_v4().to_string()),
                ..Default::default()
            },
            &s,
            old,
        )
        .await;
        result.expect_err("the upload aborts");
        let m = mock.lock().unwrap();
        assert_eq!(m.completed.len(), 1);
        assert!(m.trashed.is_empty(), "the old remote is not trashed: {:?}", m.trashed);
        assert!(!state.files.contains_key(REL));
    }

    /// Normal flow: the old remote is trashed, once, AFTER the new copy
    /// completed under the client's own id, which the state records.
    #[tokio::test]
    async fn the_old_remote_is_trashed_only_after_the_new_copy_completed() {
        let s = Scratch::new("l5-ok");
        let old = Uuid::new_v4();
        let (result, state, mock) = upload_over(Mock::default(), &s, old).await;
        result.expect("uploads");
        let m = mock.lock().unwrap();
        assert_eq!(m.trashed, vec![old.to_string()]);
        let complete = m
            .events
            .iter()
            .position(|e| e.starts_with("complete:"))
            .expect("completed");
        let trash = m
            .events
            .iter()
            .position(|e| *e == format!("trash:{old}"))
            .expect("trashed");
        assert!(complete < trash, "trash after completion: {:?}", m.events);
        assert_eq!(state.files[REL].remote_id.to_string(), m.inits[0]);
    }
}

mod reconcile;
use reconcile::*;

mod watch;
use watch::*;

mod session;
use session::*;

mod scan;
use scan::*;

mod transfer;
use transfer::*;

#[cfg(test)]
mod conflict_policy_tests {
    use super::{ConflictPolicy, Direction, SyncOptions, decide};

    /// The conflict table: one side changed moves that way; both changed is a
    /// conflict unless the policy forces the local copy.
    #[test]
    fn the_conflict_table() {
        let skip = ConflictPolicy::Skip;
        let force = ConflictPolicy::Force;
        assert_eq!(decide(false, false, skip), Direction::Skip);
        assert_eq!(decide(true, false, skip), Direction::Upload);
        assert_eq!(decide(false, true, skip), Direction::Download);
        assert_eq!(decide(true, true, skip), Direction::Conflict);
        assert_eq!(decide(true, true, force), Direction::Upload);
        // Force does not change the one-sided cases.
        assert_eq!(decide(true, false, force), Direction::Upload);
        assert_eq!(decide(false, true, force), Direction::Download);
    }

    #[test]
    fn watch_options_are_the_plain_continuous_pass() {
        let o = SyncOptions::watch();
        assert!(!o.dry_run && !o.force && !o.delete_remote && !o.once);
        assert!(!o.rehash && !o.allow_mass_delete && !o.rebind);
        assert_eq!(o.concurrency, 4);
    }
}
