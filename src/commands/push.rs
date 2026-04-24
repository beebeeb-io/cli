use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;

use crate::api::ApiClient;

pub async fn run(path: PathBuf, parent_id: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let file_size = std::fs::metadata(&path)
        .map_err(|e| format!("failed to read file metadata: {e}"))?
        .len();

    let pb = ProgressBar::new(file_size);
    pb.set_style(
        ProgressStyle::with_template(
            "  {spinner:.yellow} {msg}  {bytes}/{total_bytes}  {bytes_per_sec}  {bar:24.yellow/dark_gray}",
        )
        .unwrap()
        .progress_chars("━━─"),
    );
    pb.set_message(file_name.clone());

    // Simulate progress during upload (the actual upload is a single request)
    pb.set_position(0);
    let result = api
        .upload_file(&path, parent_id.as_deref())
        .await;

    pb.set_position(file_size);
    pb.finish_and_clear();

    match result {
        Ok(_resp) => {
            let size_str = format_size(file_size);
            println!(
                "  {} {} {} {}",
                "✓".truecolor(143, 193, 139),
                file_name.truecolor(233, 230, 221),
                "·".truecolor(106, 101, 91),
                format!("{size_str} · uploaded").truecolor(106, 101, 91),
            );
            Ok(())
        }
        Err(e) => Err(format!("upload failed: {e}")),
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
