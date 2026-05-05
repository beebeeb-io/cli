use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use colored::Colorize;
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::commands::push;

/// Debounce window: batch rapid changes within this period.
const DEBOUNCE_MS: u64 = 500;

pub async fn run(path: PathBuf, parent_id: Option<String>) -> Result<(), String> {
    // Validate auth upfront before starting the watch loop
    {
        let api = crate::api::ApiClient::from_config();
        api.require_auth()?;
    }

    let watch_path = std::fs::canonicalize(&path)
        .map_err(|e| format!("cannot resolve path: {e}"))?;

    if !watch_path.is_dir() {
        return Err(format!(
            "{} is not a directory. Use `bb push` for single files.",
            watch_path.display()
        ));
    }

    println!(
        "  {} {}",
        "watching".custom_color(crate::colors::GREEN_OK),
        watch_path.display().to_string().custom_color(crate::colors::INK),
    );
    println!(
        "  {}",
        "local changes will be encrypted and uploaded automatically"
            .custom_color(crate::colors::INK_DIM),
    );
    println!(
        "  {}",
        "press Ctrl+C to stop".custom_color(crate::colors::INK_DIM),
    );
    println!();

    // Set up Ctrl+C handler
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    ctrlc_channel(shutdown_tx);

    // Set up file watcher
    let (fs_tx, fs_rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher = notify::recommended_watcher(fs_tx)
        .map_err(|e| format!("failed to create file watcher: {e}"))?;

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
                "  {} {}",
                "stopped".custom_color(crate::colors::GREEN_OK),
                "watch ended gracefully".custom_color(crate::colors::INK_DIM),
            );
            break;
        }

        // Drain all pending filesystem events (non-blocking)
        while let Ok(event_result) = fs_rx.try_recv() {
            match event_result {
                Ok(event) => {
                    if let Some(paths) = relevant_paths(&event, &watch_path) {
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
fn relevant_paths(event: &Event, watch_root: &Path) -> Option<Vec<PathBuf>> {
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

            if paths.is_empty() {
                None
            } else {
                Some(paths)
            }
        }
        EventKind::Remove(_) => {
            // Log deletions but don't act on them (server delete not implemented)
            for p in &event.paths {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if !name.starts_with('.') {
                        let rel = p.strip_prefix(watch_root).unwrap_or(p);
                        eprintln!(
                            "  {} {} {}",
                            "skip".custom_color(crate::colors::INK_DIM),
                            rel.display().to_string().custom_color(crate::colors::INK),
                            "(deleted — server-side delete not yet implemented)"
                                .custom_color(crate::colors::INK_DIM),
                        );
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Process a batch of changed files: encrypt and upload each one.
async fn sync_batch(
    paths: &[PathBuf],
    watch_root: &Path,
    parent_id: Option<&str>,
) {
    let count = paths.len();
    let timestamp = chrono_now();

    println!(
        "  {} {} {}",
        "sync".custom_color(crate::colors::AMBER),
        format!("{count} file{}", if count == 1 { "" } else { "s" })
            .custom_color(crate::colors::INK),
        timestamp.custom_color(crate::colors::INK_DIM),
    );

    for path in paths {
        let rel = path.strip_prefix(watch_root).unwrap_or(path);

        // Skip files that vanished between event and processing
        if !path.is_file() {
            continue;
        }

        let parent = parent_id.map(String::from);
        match push::run(path.clone(), parent, false, false).await {
            Ok(()) => {
                // push::run already prints success
            }
            Err(e) => {
                eprintln!(
                    "  {} {} {}",
                    "fail".custom_color(crate::colors::RED_ERR),
                    rel.display().to_string().custom_color(crate::colors::INK),
                    e.custom_color(crate::colors::INK_DIM),
                );
            }
        }
    }
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
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for Ctrl+C");
        let _ = tx.send(());
    });
}
