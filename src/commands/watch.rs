use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use colored::Colorize;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use uuid::Uuid;

use crate::commands::{push, watch_remote};

/// Debounce window: batch rapid changes within this period.
const DEBOUNCE_MS: u64 = 500;

pub async fn run(path: PathBuf, parent_id: Option<String>) -> Result<(), String> {
    // Validate auth upfront before starting the watch loop
    {
        let api = crate::api::ApiClient::from_config();
        api.require_auth()?;
    }

    let watch_path = std::fs::canonicalize(&path).map_err(|e| format!("cannot resolve path: {e}"))?;

    if !watch_path.is_dir() {
        return Err(format!(
            "{} is not a directory. Use `bb push` for single files.",
            watch_path.display()
        ));
    }

    // Parse the optional vault parent UUID up front so the remote-event
    // consumer can filter to events under this folder. Bad input bails
    // before we open any connections.
    let watched_parent: Option<Uuid> = match &parent_id {
        Some(s) => Some(Uuid::parse_str(s).map_err(|e| format!("invalid --parent uuid '{s}': {e}"))?),
        None => None,
    };

    // Derive a remote path label for the banner
    let remote_label = match &parent_id {
        Some(id) => format!("/{}", &id[..8.min(id.len())]),
        None => "/".to_string(),
    };
    println!(
        "  {} {} {} {}",
        "\u{25CF}".custom_color(crate::colors::GREEN_OK),
        "watching".custom_color(crate::colors::GREEN_OK),
        watch_path.display().to_string().custom_color(crate::colors::INK),
        format!("\u{2192} {remote_label}").custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {} {}",
        "remote:".custom_color(crate::colors::INK_DIM),
        "SSE stream".custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {} {}",
        "local: ".custom_color(crate::colors::INK_DIM),
        "fsnotify".custom_color(crate::colors::INK_DIM),
    );
    println!("  {}", "press Ctrl+C to stop".custom_color(crate::colors::INK_DIM),);
    println!();

    // Spawn the remote-event consumer on a background tokio task. It owns
    // its own ApiClient (built from the same on-disk config) and exits on
    // `cancel_tx.send(())`.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let remote_handle = {
        let api = crate::api::ApiClient::from_config();
        let mirror = watch_path.clone();
        tokio::spawn(async move {
            if let Err(e) = watch_remote::run(api, watched_parent, mirror, cancel_rx).await {
                eprintln!(
                    "  {} {}",
                    "warn:".custom_color(crate::colors::AMBER),
                    format!("remote watcher exited: {e}").custom_color(crate::colors::INK_DIM),
                );
            }
        })
    };

    // Set up Ctrl+C handler
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    ctrlc_channel(shutdown_tx);

    // Set up file watcher
    let (fs_tx, fs_rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(fs_tx).map_err(|e| format!("failed to create file watcher: {e}"))?;

    watcher
        .watch(&watch_path, RecursiveMode::Recursive)
        .map_err(|e| format!("failed to watch directory: {e}"))?;

    // Main event loop with debouncing
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut last_event: Option<Instant> = None;

    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            println!();
            println!(
                "  {} {} {}",
                "\u{25CB}".custom_color(crate::colors::INK_DIM),
                "stopped".custom_color(crate::colors::GREEN_OK),
                "watch ended gracefully".custom_color(crate::colors::INK_DIM),
            );
            // Tell the remote consumer to stop, then wait briefly for it to
            // close its SSE connection cleanly.
            let _ = cancel_tx.send(());
            let _ = tokio::time::timeout(Duration::from_secs(2), remote_handle).await;
            break;
        }

        // Drain all pending filesystem events (non-blocking)
        while let Ok(event_result) = fs_rx.try_recv() {
            match event_result {
                Ok(event) => {
                    if let Some(paths) = relevant_paths(&event, &watch_path, parent_id.as_deref()) {
                        for p in paths {
                            pending.insert(p);
                        }
                        last_event = Some(Instant::now());
                    }
                }
                Err(e) => {
                    eprintln!(
                        "  {} {}",
                        "warn:".custom_color(crate::colors::AMBER),
                        format!("watch error: {e}").custom_color(crate::colors::INK_DIM),
                    );
                }
            }
        }

        // If we have pending changes and the debounce window has passed, sync
        if !pending.is_empty() {
            if let Some(last) = last_event {
                if last.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                    let batch: Vec<PathBuf> = pending.drain().collect();
                    last_event = None;

                    sync_batch(&batch, &watch_path, parent_id.as_deref()).await;
                }
            }
        }

        // Sleep briefly to avoid busy-waiting
        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

/// Extract file paths from a notify event, filtering to relevant changes.
///
/// Create/Modify events return paths to be queued for upload. Remove events
/// are handled inline: we soft-delete the corresponding server file (best
/// effort, never blocks the watch loop) and return None.
fn relevant_paths(event: &Event, watch_root: &Path, parent_id: Option<&str>) -> Option<Vec<PathBuf>> {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            let paths: Vec<PathBuf> = event
                .paths
                .iter()
                .filter(|p| {
                    // Only regular files, skip directories and hidden files
                    if !p.is_file() {
                        return false;
                    }
                    // Skip hidden files and common temp patterns
                    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with('.') || name.ends_with('~') || name.ends_with(".tmp") {
                            return false;
                        }
                    }
                    // Must be within watch root
                    p.starts_with(watch_root)
                })
                .cloned()
                .collect();

            if paths.is_empty() { None } else { Some(paths) }
        }
        EventKind::Remove(_) => {
            for p in &event.paths {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    // Skip hidden files / common temp swap files; these match
                    // the filter for Create/Modify above so we stay symmetric.
                    if name.starts_with('.') || name.ends_with('~') || name.ends_with(".tmp") {
                        continue;
                    }
                    // Soft-delete the server-side file. Build API client and
                    // master key inside the helper (same approach as sync_batch).
                    // Failures are logged but never block the watch loop.
                    let name_owned = name.to_string();
                    let parent_owned = parent_id.map(|s| s.to_string());
                    let watch_root_owned = watch_root.to_path_buf();
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async {
                            trash_server_file(&name_owned, &watch_root_owned, parent_owned.as_deref()).await;
                        })
                    });
                }
            }
            None
        }
        _ => None,
    }
}

/// Process a batch of changed files: encrypt and upload each one.
async fn sync_batch(paths: &[PathBuf], watch_root: &Path, parent_id: Option<&str>) {
    let count = paths.len();
    let timestamp = chrono_now();

    println!(
        "  {} {} {} {}",
        "\u{2191}".custom_color(crate::colors::GREEN_OK),
        timestamp.custom_color(crate::colors::INK_DIM),
        format!("{count} file{}", if count == 1 { "" } else { "s" }).custom_color(crate::colors::INK),
        "uploading".custom_color(crate::colors::INK_DIM),
    );

    for path in paths {
        let rel = path.strip_prefix(watch_root).unwrap_or(path);

        // Skip files that vanished between event and processing
        if !path.is_file() {
            continue;
        }

        let parent = parent_id.map(String::from);
        match push::run(path.clone(), parent, None, false, false).await {
            Ok(()) => {
                // push::run already prints success
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {} {}",
                    "\u{2717}".custom_color(crate::colors::RED_ERR),
                    chrono_now().custom_color(crate::colors::INK_DIM),
                    rel.display().to_string().custom_color(crate::colors::INK),
                    e.custom_color(crate::colors::INK_DIM),
                );
            }
        }
    }

    // Idle indicator after batch completes
    println!(
        "  {}",
        "\u{00b7} idle \u{00b7} watching for changes...".custom_color(crate::colors::INK_DIM),
    );
}

/// Attempt to soft-delete a file on the server.
/// Looks up the server file ID from the local filename, then calls trash_file.
/// Failures are logged but never propagate — the watch loop must keep running.
async fn trash_server_file(file_name: &str, _watch_root: &Path, parent_id: Option<&str>) {
    let api = crate::api::ApiClient::from_config();
    if api.require_auth().is_err() {
        return;
    }

    let master_key = match push::load_master_key() {
        Ok(mk) => mk,
        Err(_) => return,
    };

    let parent_uuid = parent_id.and_then(|id| Uuid::parse_str(id).ok());

    // find_conflict decrypts server-side names and matches the local filename.
    // Returns Some(uuid) if a matching file exists on the server.
    if let Some(file_uuid) = push::find_conflict(&api, &master_key, file_name, parent_uuid).await {
        let file_id = file_uuid.to_string();
        match api.trash_file(&file_id).await {
            Ok(_) => {
                println!(
                    "  {} {} {} {}",
                    "\u{2717}".custom_color(crate::colors::RED_ERR),
                    chrono_now().custom_color(crate::colors::INK_DIM),
                    file_name.custom_color(crate::colors::INK),
                    "(trashed on server)".custom_color(crate::colors::INK_DIM),
                );
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {} {}",
                    "\u{2717}".custom_color(crate::colors::RED_ERR),
                    chrono_now().custom_color(crate::colors::INK_DIM),
                    file_name.custom_color(crate::colors::INK),
                    e.custom_color(crate::colors::INK_DIM),
                );
            }
        }
    }
    // If no matching file is found on the server (not yet uploaded, or name
    // mismatch from a prior conflict-resolution rename), silently skip.
}

/// Simple timestamp for log lines (HH:MM:SS).
fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs = now % 60;
    let mins = (now / 60) % 60;
    let hours = (now / 3600) % 24;
    format!("{hours:02}:{mins:02}:{secs:02}")
}

/// Set up a Ctrl+C handler that sends on the channel.
fn ctrlc_channel(tx: mpsc::Sender<()>) {
    // Use a simple atomic flag + handler via ctrlc behavior built into tokio
    // Since we're already in a tokio runtime, use tokio's signal handling
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("failed to listen for Ctrl+C");
        let _ = tx.send(());
    });
}
