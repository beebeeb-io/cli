use std::collections::{HashMap, HashSet};
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
use crate::ui;

const STATE_FILE: &str = ".bb-sync.json";
// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

struct SyncDashboard {
    local_path: String,
    remote_path: String,
    file_count: AtomicU64,
    total_bytes: AtomicU64,
    is_synced: AtomicBool,
}

fn print_dashboard(dash: &SyncDashboard) {
    let w = 50;
    let status = if dash.is_synced.load(Ordering::Relaxed) {
        format!(
            "{} synced \u{00b7} watching",
            "\u{25CF}".custom_color(crate::colors::GREEN_OK)
        )
    } else {
        format!("{} syncing", "\u{25CF}".custom_color(crate::colors::AMBER))
    };
    let files = dash.file_count.load(Ordering::Relaxed);
    let bytes = dash.total_bytes.load(Ordering::Relaxed);

    println!("{}", crate::ui::box_header("SYNC", w));
    println!(
        "{}",
        crate::ui::box_line(
            &format!(
                "  {:<10}{} \u{2194} {}",
                "folder".custom_color(crate::colors::INK_DIM),
                dash.local_path,
                dash.remote_path
            ),
            w,
        )
    );
    println!(
        "{}",
        crate::ui::box_line(
            &format!("  {:<10}{}", "status".custom_color(crate::colors::INK_DIM), status),
            w,
        )
    );
    println!(
        "{}",
        crate::ui::box_line(
            &format!(
                "  {:<10}{} tracked \u{00b7} {}",
                "files".custom_color(crate::colors::INK_DIM),
                files,
                crate::ui::human_size(bytes)
            ),
            w,
        )
    );
    println!("{}", crate::ui::box_footer(w));
    println!();
}

fn is_ignored_path(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == ".DS_Store"
        || name.starts_with("._")
        || name == ".bb-sync.json"
        || name == "Thumbs.db"
        || name.starts_with(".Spotlight-")
        || name == ".Trashes"
        || name == ".fseventsd"
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct SyncState {
    remote_path: Option<String>,
    remote_folder_id: Option<Uuid>,
    last_sync: Option<DateTime<Utc>>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    files: HashMap<String, FileEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct FileEntry {
    remote_id: Uuid,
    last_mtime: i64,
    last_size: u64,
    last_sync: DateTime<Utc>,
    #[serde(default)]
    content_hash: Option<String>,
}

#[derive(Clone)]
struct LocalFile {
    mtime: i64,
    size: u64,
    content_hash: String,
}

fn compute_file_hash(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Clone)]
struct RemoteFile {
    id: Uuid,
    chunk_count: u32,
    updated_at: DateTime<Utc>,
}

pub async fn run(
    local_dir: Option<PathBuf>,
    remote_path_arg: Option<String>,
    dry_run: bool,
    force: bool,
    delete_remote: bool,
    once: bool,
    daemon: bool,
    stop_legacy: bool,
    status: bool,
    stop_session: Option<String>,
    stop_all: bool,
    concurrency: usize,
    rehash: bool,
) -> Result<(), String> {
    // ── --status: show all active sync sessions ────────────────────────────
    if status || stop_session.is_some() || stop_all {
        let api = ApiClient::from_config();
        api.require_auth()?;

        if let Some(ref name) = stop_session {
            return stop_session_by_name(&api, name).await;
        }
        if stop_all {
            return stop_all_sessions(&api).await;
        }
        // --status
        return print_session_status(&api).await;
    }

    // If no local_dir and no control flags, show status + hint (Task 4)
    let local_dir = match local_dir {
        Some(d) => d,
        None => {
            let api = ApiClient::from_config();
            api.require_auth()?;
            print_session_status(&api).await?;
            println!();
            println!(
                "  {} {}",
                "hint".custom_color(crate::colors::INK_DIM),
                "Run bb sync <local> <remote> to start a new sync".custom_color(crate::colors::INK_DIM),
            );
            return Ok(());
        }
    };

    if stop_legacy {
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
    if daemon {
        return spawn_sync_daemon(&local_dir, remote_path_arg.as_deref());
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    // Register this device with the server (best-effort, non-fatal)
    let device_info = crate::device::load_or_create();
    let device_resp = api
        .register_device(&device_info.hostname, &device_info.platform, env!("CARGO_PKG_VERSION"))
        .await
        .ok();
    let server_device_id = device_resp
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);

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
                "dry-run (no changes will be written)".custom_color(crate::colors::AMBER),
            );
        }
        println!();
    }

    let sync_start = Instant::now();
    let mut total_bytes: u64 = 0;

    let remote_folder_id = resolve_remote_folder(&api, &master_key, &remote_path, dry_run).await?;
    state.remote_path = Some(remote_path.clone());
    state.remote_folder_id = Some(remote_folder_id);

    // Create or reuse a client session for this sync pair
    let session_id: Option<String> = if let Some(ref device_id) = server_device_id {
        if let Some(ref existing) = state.session_id {
            Some(existing.clone())
        } else {
            // Check if a session already exists for this remote_path on this device
            let existing_session = api
                .list_client_sessions()
                .await
                .ok()
                .and_then(|resp| resp.get("sessions").and_then(|v| v.as_array()).cloned())
                .and_then(|sessions| {
                    sessions.into_iter().find(|s| {
                        s.get("remote_path").and_then(|v| v.as_str()) == Some(&remote_path)
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
                match api
                    .create_client_session(
                        device_id,
                        &default_name,
                        "sync",
                        Some(&local_dir.display().to_string()),
                        &remote_path,
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

    // Scan phase — the dominant "looks frozen" symptom. Show a live spinner so
    // hashing the local tree + fetching the remote tree never looks like a hang.
    let scan_active = ui::is_rich() && std::io::stdout().is_terminal();

    let local_spinner = make_scan_spinner(scan_active, "scanning local", "files");
    let local_files = scan_local_files(&local_dir, &state.files, rehash, local_spinner.as_ref())?;
    if let Some(pb) = &local_spinner {
        pb.finish_and_clear();
    }
    let local_folders = walk_local_folders(&local_dir)?;

    let remote_spinner = make_scan_spinner(scan_active, "scanning remote", "entries");
    let mut remote_files: HashMap<String, RemoteFile> = HashMap::new();
    let mut remote_folders: HashMap<String, Uuid> = HashMap::new();
    walk_remote(
        &api,
        &master_key,
        remote_folder_id,
        "",
        &mut remote_files,
        &mut remote_folders,
        remote_spinner.as_ref(),
    )
    .await?;
    if let Some(pb) = &remote_spinner {
        pb.finish_and_clear();
    }

    let mut all_folders: HashSet<String> = HashSet::new();
    all_folders.extend(local_folders.iter().cloned());
    all_folders.extend(remote_folders.keys().cloned());
    let mut sorted_folders: Vec<String> = all_folders.into_iter().collect();
    sorted_folders.sort_by_key(|s| s.matches('/').count());

    for folder_rel in &sorted_folders {
        if !remote_folders.contains_key(folder_rel) {
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
                let new_id = create_folder(&api, &master_key, folder_name, parent_id).await?;
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
            let local_path = local_dir.join(folder_rel);
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
    let mut sorted_files: Vec<String> = all_files.into_iter().collect();
    sorted_files.sort();

    let mut up_count = 0u32;
    let mut down_count = 0u32;
    let mut conflicts = 0u32;
    let mut deletes = 0u32;
    let mut skipped = 0u32;

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
        Skip,
    }

    let mut actions: Vec<SyncAction> = Vec::with_capacity(sorted_files.len());

    for rel in &sorted_files {
        let local = local_files.get(rel).cloned();
        let remote = remote_files.get(rel).cloned();
        let prior = state.files.get(rel).cloned();

        match (local, remote, prior) {
            (Some(l), Some(r), Some(p)) => {
                let local_changed = match &p.content_hash {
                    Some(h) => *h != l.content_hash,
                    None => l.mtime != p.last_mtime || l.size != p.last_size,
                };
                let remote_changed = r.updated_at > p.last_sync + chrono::Duration::seconds(1);
                match (local_changed, remote_changed) {
                    (false, false) => {
                        actions.push(SyncAction::Skip);
                    }
                    (true, false) => {
                        actions.push(SyncAction::Upload {
                            rel: rel.clone(),
                            local: l,
                            replace_id: Some(r.id),
                        });
                    }
                    (false, true) => {
                        actions.push(SyncAction::Download {
                            rel: rel.clone(),
                            remote: r,
                        });
                    }
                    (true, true) => {
                        if force {
                            actions.push(SyncAction::Upload {
                                rel: rel.clone(),
                                local: l,
                                replace_id: Some(r.id),
                            });
                        } else {
                            actions.push(SyncAction::Conflict {
                                msg: format!("{rel} (skipped \u{2014} use --force to overwrite)"),
                            });
                        }
                    }
                }
            }
            (Some(l), Some(r), None) => {
                if force {
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
            (Some(l), None, _) => {
                actions.push(SyncAction::Upload {
                    rel: rel.clone(),
                    local: l,
                    replace_id: None,
                });
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
            (None, None, _) => {
                actions.push(SyncAction::Skip);
            }
        }
    }

    // ── Phase 2: print conflicts/skips, collect uploads, execute downloads ─
    // Handle conflicts, missing-local, and skips immediately (no I/O)
    let mut pending_uploads: Vec<(String, LocalFile, Option<Uuid>)> = Vec::new();
    let mut pending_downloads: Vec<(String, RemoteFile)> = Vec::new();
    let mut pending_deletes: Vec<(String, RemoteFile)> = Vec::new();

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
                pending_uploads.push((rel, local, replace_id));
            }
            SyncAction::Download { rel, remote } => {
                pending_downloads.push((rel, remote));
            }
            SyncAction::Delete { rel, remote } => {
                pending_deletes.push((rel, remote));
            }
        }
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
                .map(|(_, l, _)| crate::upload::expected_ciphertext_for(l.size))
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
                                last_mtime: local.mtime,
                                last_size: local.size,
                                last_sync: Utc::now(),
                                content_hash: Some(local.content_hash.clone()),
                            },
                            local.size,
                        ));
                    }

                    if let Some(old_id) = replace_id {
                        let _ = api.trash_file(&old_id.to_string()).await;
                    }

                    let parent_id = match rel.rsplit_once('/') {
                        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
                        None => Some(remote_folder_id),
                    }
                    .or(Some(remote_folder_id));

                    let file_name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                    let spec = crate::upload::UploadSpec {
                        path: local_dir.join(&rel),
                        file_name,
                        file_id: Uuid::new_v4(),
                        parent_id,
                        shutdown,
                    };
                    let outcome = crate::upload::stream_encrypt_upload(api, master_key, spec, progress).await?;

                    let entry = FileEntry {
                        remote_id: outcome.server_id,
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
                Err(e) => {
                    eprintln!("  {} upload failed: {}", "!".custom_color(crate::colors::RED_ERR), e);
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
        return Ok(());
    }

    // ── Phase 4: sequential downloads ──────────────────────────────────────
    for (rel, remote) in &pending_downloads {
        do_download(&api, &master_key, &local_dir, rel, remote, &mut state, dry_run).await?;
        down_count += 1;
    }

    // ── Phase 5: sequential deletes ────────────────────────────────────────
    for (rel, remote) in &pending_deletes {
        if !dry_run {
            api.trash_file(&remote.id.to_string()).await?;
            state.files.remove(rel);
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

    state.last_sync = Some(Utc::now());
    if !dry_run {
        let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
        std::fs::write(&state_path, data).map_err(|e| format!("write state file: {e}"))?;
    }

    // Send a final heartbeat for --once mode
    if once {
        if let Some(ref sid) = session_id {
            let file_count = local_files.len() as u64;
            let _ = api
                .send_heartbeat(sid, "idle", Some(file_count), None, Some(total_bytes), None, None, None)
                .await;
        }
    }

    let elapsed = sync_start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    if ui::is_json() {
        let json_out = serde_json::json!({
            "uploaded": up_count,
            "downloaded": down_count,
            "conflicts": conflicts,
            "deleted": deletes,
            "skipped": skipped,
            "bytes": total_bytes,
            "elapsed_secs": elapsed_secs,
        });
        println!("{}", serde_json::to_string_pretty(&json_out).unwrap_or_default());
        return Ok(());
    }

    if ui::is_quiet() {
        println!(
            "{} up {} down {} conflict{}",
            up_count,
            down_count,
            conflicts,
            if deletes > 0 {
                format!(" {deletes} del")
            } else {
                String::new()
            },
        );
        return Ok(());
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

    println!(
        "  {} {} {} {} {} {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        "synced".custom_color(crate::colors::GREEN_OK),
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        counts_str,
        "\u{00b7}".custom_color(crate::colors::INK_DIM),
        meta_parts.join(" \u{00b7} ").custom_color(crate::colors::INK_DIM),
    );

    // ── Continuous watch mode ───────────────────────────────────────────────
    if !once && !dry_run && !shutdown.load(Ordering::Relaxed) {
        let use_tui = ui::is_rich() && std::io::stdout().is_terminal();

        if use_tui {
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
            .await?;
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
        }
    }

    Ok(())
}

// ── Plain-text watch loop (non-TTY / json / quiet fallback) ───────────────

#[allow(clippy::too_many_arguments)]
async fn run_plain_watch(
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
                    .send_heartbeat(&hb_sid, "watching", None, None, None, None, None, None)
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
                    let resync_master = Arc::new(load_master_key()?);
                    let resync_local = walk_local_files(local_dir)?;
                    let resync_folders = walk_local_folders(local_dir)?;
                    let mut resync_remote_files: HashMap<String, RemoteFile> = HashMap::new();
                    let mut resync_remote_folders: HashMap<String, Uuid> = HashMap::new();
                    walk_remote(
                        &resync_api,
                        &resync_master,
                        remote_folder_id,
                        "",
                        &mut resync_remote_files,
                        &mut resync_remote_folders,
                        None,
                    )
                    .await?;

                    // Ensure remote folders exist
                    let mut all_dirs: HashSet<String> = HashSet::new();
                    all_dirs.extend(resync_folders.iter().cloned());
                    all_dirs.extend(resync_remote_folders.keys().cloned());
                    let mut sorted_dirs: Vec<String> = all_dirs.into_iter().collect();
                    sorted_dirs.sort_by_key(|s| s.matches('/').count());
                    for folder_rel in &sorted_dirs {
                        if !resync_remote_folders.contains_key(folder_rel) {
                            let pid = parent_remote_id(folder_rel, &resync_remote_folders, remote_folder_id);
                            let fname = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
                            let new_id = create_folder(&resync_api, &resync_master, fname, pid).await?;
                            resync_remote_folders.insert(folder_rel.clone(), new_id);
                        }
                    }

                    // Upload changed files
                    let mut resync_bytes: u64 = 0;
                    for rel in resync_local.keys() {
                        let local = resync_local.get(rel).cloned();
                        let remote = resync_remote_files.get(rel).cloned();
                        let prior = state.files.get(rel).cloned();

                        if let Some(l) = local {
                            let local_changed = match &prior {
                                Some(p) => l.mtime != p.last_mtime || l.size != p.last_size,
                                None => remote.is_none(),
                            };
                            if local_changed {
                                do_upload(
                                    &resync_api,
                                    Arc::clone(&resync_master),
                                    local_dir,
                                    rel,
                                    &l,
                                    remote.map(|r| r.id),
                                    remote_folder_id,
                                    &resync_remote_folders,
                                    state,
                                    false,
                                )
                                .await?;
                                resync_bytes += l.size;
                            }
                        }
                    }

                    // Save state
                    state.last_sync = Some(Utc::now());
                    let data = serde_json::to_vec_pretty(&state).map_err(|e| format!("serialize state: {e}"))?;
                    std::fs::write(state_path, data).map_err(|e| format!("write state file: {e}"))?;

                    // Update dashboard
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

// ── TUI watch loop (interactive terminal) ─────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_tui_watch(
    local_dir: &Path,
    remote_path: &str,
    remote_folder_id: Uuid,
    state: &mut SyncState,
    state_path: &Path,
    session_id: Option<&str>,
    file_count: u64,
    total_bytes: u64,
) -> Result<(), String> {
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
                    .send_heartbeat(&hb_sid, "watching", None, None, None, None, None, None)
                    .await;
            }
        });
    }

    // Spawn session refresh task (every 30s, sends SessionsRefresh events).
    {
        let tx = tui_tx.clone();
        tokio::spawn(async move {
            let refresh_api = ApiClient::from_config();
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                if let Ok(resp) = refresh_api.list_client_sessions().await {
                    let sessions = parse_sessions_from_json(&resp);
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
            if let Ok(resp) = api.list_client_sessions().await {
                let sessions = parse_sessions_from_json(&resp);
                let _ = tx.send(TuiEvent::SessionsRefresh(sessions));
            }
        });
    }

    // Set up the file watcher.
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

            // Notify TUI that sync started.
            let _ = sync_tx.send(TuiEvent::SyncStarted(changed.len() as u32));

            // Send an event per changed file.
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

            // Re-run the sync logic.
            let resync_api = ApiClient::from_config();
            let resync_master = match load_master_key() {
                Ok(k) => Arc::new(k),
                Err(e) => {
                    let _ = sync_tx.send(TuiEvent::SyncError(e));
                    continue;
                }
            };
            let resync_local = match walk_local_files(&sync_local_dir) {
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
            if let Err(e) = walk_remote(
                &resync_api,
                &resync_master,
                remote_folder_id,
                "",
                &mut resync_remote_files,
                &mut resync_remote_folders,
                None,
            )
            .await
            {
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
                if !resync_remote_folders.contains_key(folder_rel) {
                    let pid = parent_remote_id(folder_rel, &resync_remote_folders, remote_folder_id);
                    let fname = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
                    match create_folder(&resync_api, &resync_master, fname, pid).await {
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
            for rel in resync_local.keys() {
                let local = resync_local.get(rel).cloned();
                let remote = resync_remote_files.get(rel).cloned();
                let prior = sync_state.files.get(rel).cloned();

                if let Some(l) = local {
                    let local_changed = match &prior {
                        Some(p) => l.mtime != p.last_mtime || l.size != p.last_size,
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
                            remote.map(|r| r.id),
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

            // Save state.
            sync_state.last_sync = Some(Utc::now());
            if let Ok(data) = serde_json::to_vec_pretty(&*sync_state) {
                let _ = std::fs::write(&sync_state_path, data);
            }

            // Compute updated totals.
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

    match tui_result {
        TuiAction::Quit => {
            // Normal exit.
        }
        TuiAction::Daemonize => {
            // User pressed 'd' — spawn daemon and exit.
            let remote_str = remote_path.to_string();
            spawn_sync_daemon(local_dir, Some(&remote_str))?;
        }
    }

    Ok(())
}

/// Parse the JSON response from list_client_sessions into SessionInfo structs.
fn parse_sessions_from_json(resp: &serde_json::Value) -> Vec<crate::tui::state::SessionInfo> {
    use crate::tui::state::SessionInfo;
    let sessions = match resp.get("sessions").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    sessions
        .iter()
        .map(|s| {
            let heartbeat_ago = s
                .get("last_heartbeat")
                .and_then(|v| v.as_str())
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|dt| {
                    let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                    if elapsed.num_seconds() < 60 {
                        format!("{}s ago", elapsed.num_seconds())
                    } else if elapsed.num_minutes() < 60 {
                        format!("{}m ago", elapsed.num_minutes())
                    } else {
                        format!("{}h ago", elapsed.num_hours())
                    }
                });

            SessionInfo {
                name: s.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string(),
                device_name: s.get("device_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                remote_path: s.get("remote_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                status: s
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                files_synced: s.get("files_synced").and_then(|v| v.as_u64()).unwrap_or(0),
                bytes_synced: s.get("bytes_synced").and_then(|v| v.as_u64()).unwrap_or(0),
                last_heartbeat: heartbeat_ago,
                current_file: s.get("current_file").and_then(|v| v.as_str()).map(String::from),
                speed_bps: s.get("speed_bps").and_then(|v| v.as_u64()),
            }
        })
        .collect()
}

// ── Session management helpers ─────────────────────────────────────────────

async fn print_session_status(api: &ApiClient) -> Result<(), String> {
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    if sessions.is_empty() {
        println!(
            "  {} {}",
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            "no active sync sessions".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    }

    let w = 62;
    println!("{}", crate::ui::box_header("SESSIONS", w));

    for session in sessions {
        let name = session.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");
        let session_status = session.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
        let local_path = session.get("local_path").and_then(|v| v.as_str()).unwrap_or("");
        let remote_path = session.get("remote_path").and_then(|v| v.as_str()).unwrap_or("");
        let device_name = session.get("device_name").and_then(|v| v.as_str()).unwrap_or("");
        let files_synced = session.get("files_synced").and_then(|v| v.as_u64()).unwrap_or(0);
        let bytes_synced = session.get("bytes_synced").and_then(|v| v.as_u64()).unwrap_or(0);
        let current_file = session.get("current_file").and_then(|v| v.as_str());
        let speed_bps = session.get("speed_bps").and_then(|v| v.as_u64());

        // Compute time since last heartbeat
        let heartbeat_ago = session
            .get("last_heartbeat")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| {
                let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                if elapsed.num_seconds() < 60 {
                    format!("{}s ago", elapsed.num_seconds())
                } else if elapsed.num_minutes() < 60 {
                    format!("{}m ago", elapsed.num_minutes())
                } else {
                    format!("{}h ago", elapsed.num_hours())
                }
            });

        // Status indicator and color
        let (indicator, status_text, status_color) = match session_status {
            "watching" | "idle" => ("\u{25CF}", "watching", crate::colors::GREEN_OK),
            "syncing" | "uploading" | "downloading" => ("\u{25C6}", session_status, crate::colors::AMBER),
            "stopped" => ("\u{25CB}", "stopped", crate::colors::INK_DIM),
            "error" => ("\u{25CF}", "error", crate::colors::RED_ERR),
            _ => ("\u{25CF}", session_status, crate::colors::INK_DIM),
        };

        // Line 1: indicator + name + device → remote + status
        let path_display = if !local_path.is_empty() {
            format!("{remote_path}")
        } else {
            remote_path.to_string()
        };

        let line1 = if !device_name.is_empty() {
            format!(
                "  {} {}  {} \u{2192} {}  {}",
                indicator.custom_color(status_color),
                name.custom_color(crate::colors::INK),
                device_name.custom_color(crate::colors::INK_DIM),
                path_display.custom_color(crate::colors::INK_DIM),
                status_text.custom_color(status_color),
            )
        } else {
            format!(
                "  {} {}  \u{2192} {}  {}",
                indicator.custom_color(status_color),
                name.custom_color(crate::colors::INK),
                path_display.custom_color(crate::colors::INK_DIM),
                status_text.custom_color(status_color),
            )
        };
        println!("{}", crate::ui::box_line(&line1, w));

        // Line 2: stats
        let mut detail_parts: Vec<String> = Vec::new();
        if files_synced > 0 {
            detail_parts.push(format!("{} files", format_count(files_synced)));
        }
        if bytes_synced > 0 {
            detail_parts.push(crate::ui::human_size(bytes_synced));
        }
        if let Some(ref ago) = heartbeat_ago {
            detail_parts.push(format!("last heartbeat {ago}"));
        }

        // If syncing, show current file + speed
        if let Some(file) = current_file {
            let speed_str = speed_bps
                .map(|s| format!(" \u{00b7} {}", crate::ui::human_speed(s as f64)))
                .unwrap_or_default();
            let line2 = format!(
                "    {} {file}{speed_str}",
                "uploading".custom_color(crate::colors::INK_DIM),
            );
            println!(
                "{}",
                crate::ui::box_line(&line2.custom_color(crate::colors::INK_DIM).to_string(), w,)
            );
        } else if !detail_parts.is_empty() {
            let line2 = format!("    {}", detail_parts.join(" \u{00b7} "));
            println!(
                "{}",
                crate::ui::box_line(&line2.custom_color(crate::colors::INK_DIM).to_string(), w,)
            );
        }
    }

    println!("{}", crate::ui::box_footer(w));
    Ok(())
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        format!("{n}")
    }
}

async fn stop_session_by_name(api: &ApiClient, name: &str) -> Result<(), String> {
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    let name_lower = name.to_lowercase();
    let matching: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|s| {
            s.get("name")
                .and_then(|v| v.as_str())
                .map(|n| {
                    let n_lower = n.to_lowercase();
                    n_lower == name_lower
                        || n_lower.ends_with(&format!("/{name_lower}"))
                        || n_lower.contains(&name_lower)
                })
                .unwrap_or(false)
        })
        .collect();

    if matching.is_empty() {
        return Err(format!("no session found with name \"{name}\""));
    }

    for session in matching {
        let id = session.get("id").and_then(|v| v.as_str()).ok_or("session missing id")?;
        let session_name = session.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

        // Stop the server-side session
        api.stop_client_session(id).await?;

        // Kill the local daemon process + remove auto-start
        let slug = crate::daemon::slugify(session_name);
        let _ = crate::daemon::stop_daemon(&slug);

        println!(
            "  {} stopped {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            session_name.custom_color(crate::colors::INK),
        );
    }

    Ok(())
}

async fn stop_all_sessions(api: &ApiClient) -> Result<(), String> {
    let device_info = crate::device::load_or_create();
    let resp = api.list_client_sessions().await?;
    let sessions = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or("invalid session listing")?;

    // Filter to sessions on this device (match by device_name == hostname)
    let device_sessions: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|s| {
            s.get("device_name")
                .and_then(|v| v.as_str())
                .map(|n| n == device_info.hostname)
                .unwrap_or(false)
        })
        .collect();

    if device_sessions.is_empty() {
        println!(
            "  {} {}",
            "\u{00b7}".custom_color(crate::colors::INK_DIM),
            "no active sessions on this device".custom_color(crate::colors::INK_DIM),
        );
        // Still stop any orphaned local daemons
        let _ = crate::daemon::stop_all_daemons();
        return Ok(());
    }

    let mut stopped = 0u32;
    for session in &device_sessions {
        let id = session.get("id").and_then(|v| v.as_str()).ok_or("session missing id")?;
        let name = session.get("name").and_then(|v| v.as_str()).unwrap_or("unnamed");

        // Stop server-side session
        api.stop_client_session(id).await?;

        // Kill local daemon + remove auto-start
        let slug = crate::daemon::slugify(name);
        let _ = crate::daemon::stop_daemon(&slug);

        println!(
            "  {} stopped {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            name.custom_color(crate::colors::INK),
        );
        stopped += 1;
    }

    // Also stop any orphaned daemons (PID files with no matching server session)
    let _ = crate::daemon::stop_all_daemons();

    println!(
        "\n  {} {} session{} stopped on {}",
        "\u{2713}".custom_color(crate::colors::GREEN_OK),
        stopped,
        if stopped == 1 { "" } else { "s" },
        device_info.hostname.custom_color(crate::colors::INK),
    );

    Ok(())
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config.master_key.ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
        b64()
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key in config: {e}"))?,
    );
    if mk_bytes.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    Ok(beebeeb_core::kdf::MasterKey::from_bytes(arr))
}

fn rel_path_str(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect::<Vec<_>>()
        .join("/")
}

/// Build an amber-accented scan spinner with a live `{pos}`-driven counter,
/// or `None` when progress is suppressed (non-TTY / json / quiet). Draws to
/// stderr so it never corrupts stdout output.
fn make_scan_spinner(active: bool, label: &str, unit: &str) -> Option<indicatif::ProgressBar> {
    if !active {
        return None;
    }
    let pb = indicatif::ProgressBar::new_spinner();
    let template = format!("  {{spinner:.214}} {label} · {{pos}} {unit}");
    pb.set_style(
        indicatif::ProgressStyle::with_template(&template)
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_spinner()),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    Some(pb)
}

/// Walk the local tree like [`walk_local_files`], but with two scan-phase
/// optimisations: a live spinner counter, and skip-hashing.
///
/// Skip-hashing: hashing every file with SHA-256 is the bulk of the scan cost.
/// When `rehash` is false and a file's `(mtime, size)` match the prior sync
/// state, the stored `content_hash` is reused and the read+hash is skipped.
/// `--rehash` forces a full hash of every file (catches same-size edits).
fn scan_local_files(
    root: &Path,
    prior: &HashMap<String, FileEntry>,
    rehash: bool,
    scan: Option<&indicatif::ProgressBar>,
) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size = meta.len();
        let rel_str = rel_path_str(rel);

        // Reuse the stored hash when unchanged (unless --rehash forces a read).
        let reused = if rehash {
            None
        } else {
            prior.get(&rel_str).and_then(|p| {
                if p.last_mtime == mtime && p.last_size == size {
                    p.content_hash.clone()
                } else {
                    None
                }
            })
        };
        let content_hash = match reused {
            Some(h) => h,
            None => compute_file_hash(path)?,
        };

        if let Some(pb) = scan {
            pb.inc(1);
        }
        out.insert(
            rel_str,
            LocalFile {
                mtime,
                size,
                content_hash,
            },
        );
    }
    Ok(out)
}

fn walk_local_files(root: &Path) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let content_hash = compute_file_hash(path)?;
        out.insert(
            rel_path_str(rel),
            LocalFile {
                mtime,
                size: meta.len(),
                content_hash,
            },
        );
    }
    Ok(out)
}

fn walk_local_folders(root: &Path) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() || path == root {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        out.insert(rel_path_str(rel));
    }
    Ok(out)
}

fn parent_remote_id(rel: &str, folders: &HashMap<String, Uuid>, root_id: Uuid) -> Option<Uuid> {
    match rel.rsplit_once('/') {
        Some((parent, _)) => folders.get(parent).copied().or(Some(root_id)),
        None => Some(root_id),
    }
}

async fn resolve_remote_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    remote_path: &str,
    dry_run: bool,
) -> Result<Uuid, String> {
    let segments: Vec<&str> = remote_path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Err("remote_path cannot be empty or root".to_string());
    }

    let mut current_parent: Option<Uuid> = None;

    for seg in segments {
        let listing = api.list_files(current_parent.map(|u| u.to_string()).as_deref()).await?;
        let items = listing
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or("invalid file listing")?;

        let mut found: Option<Uuid> = None;
        for item in items {
            let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
            if !is_folder {
                continue;
            }
            let id_str = match item.get("id").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let id: Uuid = match id_str.parse() {
                Ok(u) => u,
                Err(_) => continue,
            };
            let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");
            let name = crate::crypto::decrypt_name(master_key, id_str, name_enc);
            if name.as_deref() == Some(seg) {
                found = Some(id);
                break;
            }
        }

        match found {
            Some(id) => current_parent = Some(id),
            None => {
                if dry_run {
                    return Err(format!("remote folder '{seg}' does not exist (dry-run, cannot create)"));
                }
                let new_id = create_folder(api, master_key, seg, current_parent).await?;
                println!(
                    "  {} {} {}",
                    "+".custom_color(crate::colors::GREEN_OK),
                    "remote folder".custom_color(crate::colors::INK_DIM),
                    seg.custom_color(crate::colors::INK),
                );
                current_parent = Some(new_id);
            }
        }
    }

    current_parent.ok_or_else(|| "path resolution failed".to_string())
}

async fn create_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let new_id = Uuid::new_v4();
    let name_enc = beebeeb_core::encrypt::encrypt_name(master_key, &new_id.to_string(), name, None)
        .map_err(|e| format!("encrypt folder name: {e}"))?;
    let result = api.create_folder(&name_enc, parent_id, Some(new_id)).await?;
    let id_str = result.get("id").and_then(|v| v.as_str()).ok_or("missing folder id")?;
    id_str.parse().map_err(|e| format!("invalid folder id: {e}"))
}

#[allow(clippy::too_many_arguments)]
async fn walk_remote(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    folder_id: Uuid,
    prefix: &str,
    files: &mut HashMap<String, RemoteFile>,
    folders: &mut HashMap<String, Uuid>,
    scan: Option<&indicatif::ProgressBar>,
) -> Result<(), String> {
    let listing = api.list_files(Some(&folder_id.to_string())).await?;
    let items = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing")?;

    for item in items {
        let id_str = item.get("id").and_then(|v| v.as_str()).ok_or("missing id")?;
        let id: Uuid = id_str.parse().map_err(|e| format!("invalid id: {e}"))?;
        let is_folder = item.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let name_enc = item.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");

        // Use the shared crypto module which handles all server formats
        // (Rust EncryptedBlob, web-app base64 blob, plaintext) and both
        // UUID key derivations (binary and string).
        let name = match crate::crypto::decrypt_name(master_key, id_str, name_enc) {
            Some(n) => n,
            None => continue,
        };

        if let Some(pb) = scan {
            pb.inc(1);
        }

        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        if is_folder {
            folders.insert(rel.clone(), id);
            Box::pin(walk_remote(api, master_key, id, &rel, files, folders, scan)).await?;
        } else {
            let updated_at_str = item.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
            let updated_at = DateTime::parse_from_rfc3339(updated_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let chunk_count = item.get("chunk_count").and_then(|v| v.as_i64()).unwrap_or(1) as u32;
            files.insert(
                rel,
                RemoteFile {
                    id,
                    chunk_count,
                    updated_at,
                },
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn do_upload(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    local_dir: &Path,
    rel: &str,
    local: &LocalFile,
    replace_remote_id: Option<Uuid>,
    root_remote_id: Uuid,
    remote_folders: &HashMap<String, Uuid>,
    state: &mut SyncState,
    dry_run: bool,
) -> Result<(), String> {
    let size_str = format_size(local.size);
    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {} {}",
            "\u{2191}".custom_color(crate::colors::GREEN_OK),
            rel.custom_color(crate::colors::INK),
            format!("({size_str})").custom_color(crate::colors::INK_DIM),
        );
    }

    if dry_run {
        return Ok(());
    }

    if let Some(old_id) = replace_remote_id {
        let _ = api.trash_file(&old_id.to_string()).await;
    }

    let parent_id = match rel.rsplit_once('/') {
        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
        None => Some(root_remote_id),
    }
    .or(Some(root_remote_id));

    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    let local_path = local_dir.join(rel);
    let new_id = upload_file_to(api, master_key, &local_path, file_name, parent_id).await?;

    state.files.insert(
        rel.to_string(),
        FileEntry {
            remote_id: new_id,
            last_mtime: local.mtime,
            last_size: local.size,
            last_sync: Utc::now(),
            content_hash: Some(local.content_hash.clone()),
        },
    );

    Ok(())
}

async fn do_download(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    local_dir: &Path,
    rel: &str,
    remote: &RemoteFile,
    state: &mut SyncState,
    dry_run: bool,
) -> Result<(), String> {
    if !ui::is_json() && !ui::is_quiet() {
        println!(
            "  {} {}",
            "\u{2193}".custom_color(crate::colors::CYAN),
            rel.custom_color(crate::colors::INK),
        );
    }

    if dry_run {
        return Ok(());
    }

    let out_path = local_dir.join(rel);
    match download_to(api, master_key, remote.id, remote.chunk_count, &out_path).await {
        Ok(()) => {}
        Err(e) if e.contains("still in progress") || e.contains("409") => {
            if !ui::is_quiet() {
                use colored::Colorize;
                eprintln!(
                    "  {} {} — clearing stuck upload, will re-sync next run",
                    "!".custom_color(crate::colors::AMBER),
                    rel.custom_color(crate::colors::INK_DIM),
                );
            }
            let _ = api.trash_file(&remote.id.to_string()).await;
            return Ok(());
        }
        Err(e) if e.contains("key derivation") || e.contains("decrypt") => {
            if !ui::is_quiet() {
                use colored::Colorize;
                eprintln!(
                    "  {} {} — corrupt remote copy, trashing to re-upload from local",
                    "!".custom_color(crate::colors::AMBER),
                    rel.custom_color(crate::colors::INK_DIM),
                );
            }
            let _ = api.trash_file(&remote.id.to_string()).await;
            return Ok(());
        }
        Err(e) => return Err(e),
    }

    let meta = std::fs::metadata(&out_path).map_err(|e| format!("stat {}: {e}", out_path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let content_hash = compute_file_hash(&out_path).ok();
    state.files.insert(
        rel.to_string(),
        FileEntry {
            remote_id: remote.id,
            last_mtime: mtime,
            last_size: meta.len(),
            last_sync: Utc::now(),
            content_hash,
        },
    );

    Ok(())
}

/// Streaming, constant-memory upload of a single file. Thin wrapper over
/// [`crate::upload::stream_encrypt_upload`] used by the watch loops; the main
/// sync path calls the driver directly so it can attach progress bars. Uploads
/// here are not tied to the main Ctrl-C flag (watch-mode Ctrl-C hard-exits), so
/// a fresh, never-tripped shutdown flag is used.
async fn upload_file_to(
    api: &ApiClient,
    master_key: Arc<beebeeb_core::kdf::MasterKey>,
    file_path: &Path,
    file_name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let spec = crate::upload::UploadSpec {
        path: file_path.to_path_buf(),
        file_name: file_name.to_string(),
        file_id: Uuid::new_v4(),
        parent_id,
        shutdown: Arc::new(AtomicBool::new(false)),
    };
    let outcome = crate::upload::stream_encrypt_upload(api, master_key, spec, &crate::upload::NoopProgress).await?;
    Ok(outcome.server_id)
}

async fn download_to(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: Uuid,
    chunk_count: u32,
    out_path: &Path,
) -> Result<(), String> {
    let file_id_str = file_id.to_string();
    let encrypted_bytes = api.download_file(&file_id_str).await?;

    let plaintext = crate::crypto::decrypt_file_chunks(master_key, &file_id_str, &encrypted_bytes, chunk_count)?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(out_path, &plaintext).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    Ok(())
}

fn normalize_remote_path(raw: &str) -> String {
    let path = raw.trim_end_matches('/');
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home_str.as_ref()) {
            let rest = rest.trim_start_matches('/');
            return if rest.is_empty() {
                "/".to_string()
            } else {
                format!("/{rest}")
            };
        }
    }
    if path.is_empty() || path == "/" {
        return "/".to_string();
    }
    if !path.starts_with('/') {
        format!("/{path}")
    } else {
        path.to_string()
    }
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} kB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ── Daemon spawning ────────────────────────────────────────────────────────

/// Spawn a sync daemon and optionally install platform auto-start.
///
/// Builds a session name from the local directory and remote path, computes
/// a filesystem slug, spawns `bb sync <local> <remote>` as a detached
/// process, writes a PID file, and installs a LaunchAgent (macOS) or
/// systemd user unit (Linux) for auto-start on login.
fn spawn_sync_daemon(local_dir: &Path, remote_path: Option<&str>) -> Result<(), String> {
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

    // Check if already running
    if let Some(pid) = crate::daemon::is_daemon_running(&slug)? {
        println!(
            "  {} daemon already running (PID {})",
            "\u{25CF}".custom_color(crate::colors::GREEN_OK),
            pid,
        );
        println!("  {} bb sync --status", "status".custom_color(crate::colors::INK_DIM),);
        return Ok(());
    }

    // Spawn the daemon process
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
