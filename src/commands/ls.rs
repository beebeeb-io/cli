use colored::Colorize;

use crate::api::ApiClient;

pub async fn run(path: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    let result = api.list_files(path.as_deref()).await?;

    let files = result
        .as_array()
        .or_else(|| result.get("files").and_then(|f| f.as_array()));

    let Some(files) = files else {
        println!(
            "  {}",
            "empty — no files here".truecolor(106, 101, 91),
        );
        return Ok(());
    };

    if files.is_empty() {
        println!(
            "  {}",
            "empty — no files here".truecolor(106, 101, 91),
        );
        return Ok(());
    }

    // Column header
    println!(
        "  {}",
        format!(
            "{:<36}  {:>10}  {:<16}  {}",
            "name", "size", "modified", "type"
        )
        .truecolor(106, 101, 91),
    );

    for file in files {
        let name = file
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let size = file
            .get("size")
            .and_then(|v| v.as_u64())
            .map(format_size)
            .unwrap_or_else(|| "-".to_string());
        let modified = file
            .get("updated_at")
            .or_else(|| file.get("modified"))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let file_type = file
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("file");

        let is_folder = file_type == "folder" || file_type == "directory";

        let name_colored = if is_folder {
            format!("{name}/").truecolor(245, 184, 0)
        } else {
            name.to_string().truecolor(208, 200, 154)
        };

        println!(
            "  {:<36}  {:>10}  {:<16}  {}",
            name_colored,
            size.truecolor(106, 101, 91),
            modified.truecolor(106, 101, 91),
            file_type.truecolor(106, 101, 91),
        );
    }

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
