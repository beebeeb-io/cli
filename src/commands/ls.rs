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
            "empty — no files here".custom_color(crate::colors::INK_DIM),
        );
        return Ok(());
    };

    if files.is_empty() {
        println!(
            "  {}",
            "empty — no files here".custom_color(crate::colors::INK_DIM),
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
        .custom_color(crate::colors::INK_DIM),
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
            format!("{name}/").custom_color(crate::colors::AMBER)
        } else {
            name.to_string().custom_color(crate::colors::INK_WARM)
        };

        println!(
            "  {:<36}  {:>10}  {:<16}  {}",
            name_colored,
            size.custom_color(crate::colors::INK_DIM),
            modified.custom_color(crate::colors::INK_DIM),
            file_type.custom_color(crate::colors::INK_DIM),
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
