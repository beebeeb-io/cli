use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;

use crate::api::ApiClient;

pub async fn run(file_id: String, output: Option<PathBuf>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.yellow} downloading {msg}")
            .unwrap(),
    );
    pb.set_message(file_id.clone());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let bytes = api.download_file(&file_id).await?;

    pb.finish_and_clear();

    let out_path = output.unwrap_or_else(|| PathBuf::from(&file_id));

    std::fs::write(&out_path, &bytes)
        .map_err(|e| format!("failed to write file: {e}"))?;

    let size_str = format_size(bytes.len() as u64);
    println!(
        "  {} {} {} {}",
        "✓".truecolor(143, 193, 139),
        out_path.display().to_string().truecolor(233, 230, 221),
        "·".truecolor(106, 101, 91),
        format!("{size_str} · downloaded").truecolor(106, 101, 91),
    );

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
