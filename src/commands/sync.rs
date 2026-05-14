use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;

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
        format!(
            "{} syncing",
            "\u{25CF}".custom_color(crate::colors::AMBER)
        )
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
            &format!(
                "  {:<10}{}",
                "status".custom_color(crate::colors::INK_DIM),
                status
            ),
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
    use sha2::{Sha256, Digest};
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
    local_dir: PathBuf,
    remote_path_arg: Option<String>,
    dry_run: bool,
    force: bool,
    delete_remote: bool,
    once: bool,
    daemon: bool,
    stop: bool,
    concurrency: usize,
) -> Result<(), String> {
    if stop {
        return uninstall_launchagent();
    }
    if daemon {
        return install_launchagent(&local_dir, remote_path_arg.as_deref());
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    if !local_dir.exists() {
        return Err(format!("local directory not found: {}", local_dir.display()));
    }
    if !local_dir.is_dir() {
        return Err(format!("not a directory: {}", local_dir.display()));
    }

    let local_dir = std::fs::canonicalize(&local_dir)
        .map_err(|e| format!("cannot resolve path: {e}"))?;

    let state_path = local_dir.join(STATE_FILE);
    let mut state: SyncState = if state_path.exists() {
        let data = std::fs::read(&state_path).map_err(|e| format!("read state: {e}"))?;
        serde_json::from_slice(&data).map_err(|e| format!("invalid state file: {e}"))?
    } else {
        SyncState::default()
    };

    let remote_path = match remote_path_arg.or_else(|| state.remote_path.clone()) {
        Some(p) => p,
        None => {
            return Err("remote_path is required (no .bb-sync.json found). \
                        Usage: bb sync <local_dir> <remote_path>"
                .to_string());
        }
    };

    let master_key = load_master_key()?;

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

    let remote_folder_id =
        resolve_remote_folder(&api, &master_key, &remote_path, dry_run).await?;
    state.remote_path = Some(remote_path.clone());
    state.remote_folder_id = Some(remote_folder_id);

    let local_files = walk_local_files(&local_dir)?;
    let local_folders = walk_local_folders(&local_dir)?;

    let mut remote_files: HashMap<String, RemoteFile> = HashMap::new();
    let mut remote_folders: HashMap<String, Uuid> = HashMap::new();
    walk_remote(
        &api,
        &master_key,
        remote_folder_id,
        "",
        &mut remote_files,
        &mut remote_folders,
    )
    .await?;

    let mut all_folders: HashSet<String> = HashSet::new();
    all_folders.extend(local_folders.iter().cloned());
    all_folders.extend(remote_folders.keys().cloned());
    let mut sorted_folders: Vec<String> = all_folders.into_iter().collect();
    sorted_folders.sort_by_key(|s| s.matches('/').count());

    for folder_rel in &sorted_folders {
        if !remote_folders.contains_key(folder_rel) {
            let parent_id =
                parent_remote_id(folder_rel, &remote_folders, remote_folder_id);
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
                std::fs::create_dir_all(&local_path)
                    .map_err(|e| format!("mkdir {}: {e}", local_path.display()))?;
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
                        format!("{rel} (use --delete to trash from vault)")
                            .custom_color(crate::colors::INK_DIM),
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

    // ── Phase 3: parallel uploads ──────────────────────────────────────────
    {
        use futures_util::stream::{self, StreamExt};

        let upload_results: Vec<Result<(String, FileEntry, u64), String>> = stream::iter(
            pending_uploads.into_iter().map(|(rel, local, replace_id)| {
                let api = &api;
                let master_key = &master_key;
                let local_dir = &local_dir;
                let remote_folders = &remote_folders;
                async move {
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
                        return Ok((rel, FileEntry {
                            remote_id: Uuid::nil(),
                            last_mtime: local.mtime,
                            last_size: local.size,
                            last_sync: Utc::now(),
                            content_hash: Some(local.content_hash.clone()),
                        }, local.size));
                    }

                    if let Some(old_id) = replace_id {
                        let _ = api.trash_file(&old_id.to_string()).await;
                    }

                    let parent_id = match rel.rsplit_once('/') {
                        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
                        None => Some(remote_folder_id),
                    }
                    .or(Some(remote_folder_id));

                    let file_name = rel.rsplit('/').next().unwrap_or(&rel);
                    let local_path = local_dir.join(&rel);
                    let new_id = upload_file_to(api, master_key, &local_path, file_name, parent_id).await?;

                    let entry = FileEntry {
                        remote_id: new_id,
                        last_mtime: local.mtime,
                        last_size: local.size,
                        last_sync: Utc::now(),
                        content_hash: Some(local.content_hash.clone()),
                    };
                    Ok((rel, entry, local.size))
                }
            }),
        )
        .buffer_unordered(concurrency)
        .collect()
        .await;

        for result in upload_results {
            match result {
                Ok((rel, entry, size)) => {
                    if !dry_run {
                        state.files.insert(rel, entry);
                    }
                    total_bytes += size;
                    up_count += 1;
                }
                Err(e) => {
                    eprintln!(
                        "  {} upload failed: {}",
                        "!".custom_color(crate::colors::RED_ERR),
                        e
                    );
                }
            }
        }
    }

    // ── Phase 4: sequential downloads ──────────────────────────────────────
    for (rel, remote) in &pending_downloads {
        do_download(&api, &master_key, &local_dir, rel, remote, &mut state, dry_run)
            .await?;
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
        let data = serde_json::to_vec_pretty(&state)
            .map_err(|e| format!("serialize state: {e}"))?;
        std::fs::write(&state_path, data)
            .map_err(|e| format!("write state file: {e}"))?;
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
            up_count, down_count, conflicts,
            if deletes > 0 { format!(" {deletes} del") } else { String::new() },
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
    if !once && !dry_run {
        let local_display = local_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| local_dir.display().to_string());

        let dashboard = Arc::new(SyncDashboard {
            local_path: local_display,
            remote_path: remote_path.clone(),
            file_count: AtomicU64::new(local_files.len() as u64),
            total_bytes: AtomicU64::new(total_bytes),
            is_synced: AtomicBool::new(true),
        });

        println!();
        print_dashboard(&dashboard);

        use notify::RecursiveMode;
        use notify_debouncer_full::new_debouncer;

        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(
            std::time::Duration::from_millis(500),
            None,
            tx,
        )
        .map_err(|e| format!("watcher: {e}"))?;

        debouncer
            .watch(&local_dir, RecursiveMode::Recursive)
            .map_err(|e| format!("watch: {e}"))?;

        eprintln!(
            "  {} watching for changes \u{00b7} press Ctrl+C to stop",
            "\u{00b7}".custom_color(crate::colors::INK_DIM)
        );

        loop {
            match rx.recv() {
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
                        let resync_master = load_master_key()?;
                        let resync_local = walk_local_files(&local_dir)?;
                        let resync_folders = walk_local_folders(&local_dir)?;
                        let mut resync_remote_files: HashMap<String, RemoteFile> = HashMap::new();
                        let mut resync_remote_folders: HashMap<String, Uuid> = HashMap::new();
                        walk_remote(
                            &resync_api,
                            &resync_master,
                            remote_folder_id,
                            "",
                            &mut resync_remote_files,
                            &mut resync_remote_folders,
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
                                let pid = parent_remote_id(
                                    folder_rel,
                                    &resync_remote_folders,
                                    remote_folder_id,
                                );
                                let fname = folder_rel
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or(folder_rel);
                                let new_id = create_folder(
                                    &resync_api,
                                    &resync_master,
                                    fname,
                                    pid,
                                )
                                .await?;
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
                                        &resync_master,
                                        &local_dir,
                                        rel,
                                        &l,
                                        remote.map(|r| r.id),
                                        remote_folder_id,
                                        &resync_remote_folders,
                                        &mut state,
                                        false,
                                    )
                                    .await?;
                                    resync_bytes += l.size;
                                }
                            }
                        }

                        // Save state
                        state.last_sync = Some(Utc::now());
                        let data = serde_json::to_vec_pretty(&state)
                            .map_err(|e| format!("serialize state: {e}"))?;
                        std::fs::write(&state_path, data)
                            .map_err(|e| format!("write state file: {e}"))?;

                        // Update dashboard
                        let new_local = walk_local_files(&local_dir)?;
                        dashboard
                            .file_count
                            .store(new_local.len() as u64, Ordering::Relaxed);
                        dashboard.total_bytes.fetch_add(resync_bytes, Ordering::Relaxed);
                        dashboard.is_synced.store(true, Ordering::Relaxed);
                    }
                }
                Ok(Err(errs)) => {
                    for e in errs {
                        eprintln!(
                            "  {} watch error: {}",
                            "!".custom_color(crate::colors::RED_ERR),
                            e
                        );
                    }
                }
                Err(_) => break,
            }
        }
    }

    Ok(())
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config
        .master_key
        .ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes = b64()
        .decode(&mk_b64)
        .map_err(|e| format!("invalid master key in config: {e}"))?;
    if mk_bytes.len() != 32 {
        return Err(format!(
            "master key must be 32 bytes, got {}",
            mk_bytes.len()
        ));
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
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
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

fn parent_remote_id(
    rel: &str,
    folders: &HashMap<String, Uuid>,
    root_id: Uuid,
) -> Option<Uuid> {
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
    let segments: Vec<&str> = remote_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return Err("remote_path cannot be empty or root".to_string());
    }

    let mut current_parent: Option<Uuid> = None;

    for seg in segments {
        let listing = api
            .list_files(current_parent.map(|u| u.to_string()).as_deref())
            .await?;
        let items = listing
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or("invalid file listing")?;

        let mut found: Option<Uuid> = None;
        for item in items {
            let is_folder = item
                .get("is_folder")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
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
            let name_enc = item
                .get("name_encrypted")
                .and_then(|v| v.as_str())
                .unwrap_or("");
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
                    return Err(format!(
                        "remote folder '{seg}' does not exist (dry-run, cannot create)"
                    ));
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
    let name_enc =
        beebeeb_core::encrypt::encrypt_name(master_key, &new_id.to_string(), name, None)
            .map_err(|e| format!("encrypt folder name: {e}"))?;
    let result = api
        .create_folder(&name_enc, parent_id, Some(new_id))
        .await?;
    let id_str = result
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("missing folder id")?;
    id_str.parse().map_err(|e| format!("invalid folder id: {e}"))
}

async fn walk_remote(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    folder_id: Uuid,
    prefix: &str,
    files: &mut HashMap<String, RemoteFile>,
    folders: &mut HashMap<String, Uuid>,
) -> Result<(), String> {
    let listing = api.list_files(Some(&folder_id.to_string())).await?;
    let items = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing")?;

    for item in items {
        let id_str = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("missing id")?;
        let id: Uuid = id_str.parse().map_err(|e| format!("invalid id: {e}"))?;
        let is_folder = item
            .get("is_folder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let name_enc = item
            .get("name_encrypted")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Use the shared crypto module which handles all server formats
        // (Rust EncryptedBlob, web-app base64 blob, plaintext) and both
        // UUID key derivations (binary and string).
        let name = match crate::crypto::decrypt_name(master_key, id_str, name_enc) {
            Some(n) => n,
            None => continue,
        };

        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        if is_folder {
            folders.insert(rel.clone(), id);
            Box::pin(walk_remote(api, master_key, id, &rel, files, folders)).await?;
        } else {
            let updated_at_str = item
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let updated_at = DateTime::parse_from_rfc3339(updated_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let chunk_count = item
                .get("chunk_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(1) as u32;
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
    master_key: &beebeeb_core::kdf::MasterKey,
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
    download_to(api, master_key, remote.id, remote.chunk_count, &out_path).await?;

    let meta = std::fs::metadata(&out_path)
        .map_err(|e| format!("stat {}: {e}", out_path.display()))?;
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

async fn upload_file_to(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_path: &Path,
    file_name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let file_bytes =
        std::fs::read(file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;

    let file_id = Uuid::new_v4();
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_id.to_string().as_bytes());

    let mime = beebeeb_core::media::guess_mime_type(file_name);
    let name_encrypted =
        beebeeb_core::encrypt::encrypt_name(master_key, &file_id.to_string(), file_name, mime)
            .map_err(|e| format!("encrypt name: {e}"))?;

    let plan = beebeeb_types::plan_chunks(file_bytes.len() as u64, beebeeb_types::ChunkProfile::Desktop);
    let chunk_size = plan.chunk_size_bytes as usize;

    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut total_enc: i64 = 0;

    if file_bytes.is_empty() {
        let bytes = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, &[])
            .map_err(|e| format!("encrypt chunk: {e}"))?;
        total_enc += bytes.len() as i64;
        chunks.push((0, bytes));
    } else {
        for (i, chunk) in file_bytes.chunks(chunk_size).enumerate() {
            let bytes = beebeeb_core::encrypt::encrypt_chunk_raw(&file_key, chunk)
                .map_err(|e| format!("encrypt chunk {i}: {e}"))?;
            total_enc += bytes.len() as i64;
            chunks.push((i as u32, bytes));
        }
    }

    let metadata = serde_json::json!({
        "name_encrypted": name_encrypted,
        "parent_id": parent_id,
        "file_id": file_id,
        "mime_type": serde_json::Value::Null,
        "size_bytes": total_enc,
        "is_media": beebeeb_core::media::is_media(mime),
    });
    let metadata_json =
        serde_json::to_string(&metadata).map_err(|e| format!("serialize metadata: {e}"))?;

    let result = api.upload_encrypted(&metadata_json, &chunks).await?;
    let id_str = result
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("server response missing file id")?;
    id_str.parse().map_err(|e| format!("invalid file id: {e}"))
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

    let plaintext = crate::crypto::decrypt_file_chunks(
        master_key,
        &file_id_str,
        &encrypted_bytes,
        chunk_count,
    )?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(out_path, &plaintext)
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;

    Ok(())
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

// ── LaunchAgent daemon ──────────────────────────────────────────────────────

fn install_launchagent(local_dir: &Path, remote_path: Option<&str>) -> Result<(), String> {
    let plist_dir = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join("Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir).map_err(|e| format!("create dir: {e}"))?;
    let plist_path = plist_dir.join("io.beebeeb.sync.plist");

    let bb_path = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut args = vec![
        bb_path.to_string_lossy().to_string(),
        "sync".to_string(),
        local_dir.to_string_lossy().to_string(),
    ];
    if let Some(rp) = remote_path {
        args.push(rp.to_string());
    }

    let args_xml: String = args
        .iter()
        .map(|a| format!("        <string>{}</string>", a))
        .collect::<Vec<_>>()
        .join("\n");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.beebeeb.sync</string>
    <key>ProgramArguments</key>
    <array>
{args_xml}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/beebeeb-sync.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/beebeeb-sync.err</string>
</dict>
</plist>"#
    );

    std::fs::write(&plist_path, plist).map_err(|e| format!("write plist: {e}"))?;

    let status = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .status()
        .map_err(|e| format!("launchctl: {e}"))?;

    if !status.success() {
        return Err("launchctl load failed".to_string());
    }

    println!(
        "  {} sync daemon installed \u{00b7} starts on login",
        "\u{2713}".custom_color(crate::colors::GREEN_OK)
    );
    println!(
        "  {} {}",
        "folder".custom_color(crate::colors::INK_DIM),
        local_dir
            .display()
            .to_string()
            .custom_color(crate::colors::INK),
    );
    println!(
        "  {} bb sync --stop",
        "stop".custom_color(crate::colors::INK_DIM)
    );
    Ok(())
}

fn uninstall_launchagent() -> Result<(), String> {
    let plist_path = dirs::home_dir()
        .ok_or("cannot find home directory")?
        .join("Library/LaunchAgents/io.beebeeb.sync.plist");

    if !plist_path.exists() {
        return Err("no sync daemon installed".to_string());
    }

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();

    std::fs::remove_file(&plist_path).map_err(|e| format!("remove plist: {e}"))?;

    println!(
        "  {} sync daemon removed",
        "\u{2713}".custom_color(crate::colors::GREEN_OK)
    );
    Ok(())
}
