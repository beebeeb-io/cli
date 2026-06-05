use colored::Colorize;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::crypto::decrypt_name;
use crate::{colors, ui};

/// Field to sort a listing by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortField {
    #[default]
    Name,
    Size,
    Modified,
    Created,
}

impl SortField {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "name" => Ok(SortField::Name),
            "size" => Ok(SortField::Size),
            "modified" | "date" => Ok(SortField::Modified),
            "created" => Ok(SortField::Created),
            other => Err(format!("unknown sort field '{other}' (use name|size|modified|created)")),
        }
    }
}

/// Options for `bb ls`. `default()` reproduces the pre-flags behaviour exactly.
#[derive(Debug, Clone)]
pub struct LsOpts {
    /// `-l/--long`: add a CREATED column.
    pub long: bool,
    /// `-a/--all`: also include trashed entries (flagged).
    pub all: bool,
    /// `-R/--recursive`: descend into subfolders.
    pub recursive: bool,
    /// `--depth N`: max recursion depth (only with `--recursive`).
    pub depth: usize,
    /// `--sort <field>`.
    pub sort: SortField,
    /// `-r/--reverse`: reverse the sort order.
    pub reverse: bool,
}

impl Default for LsOpts {
    fn default() -> Self {
        Self {
            long: false,
            all: false,
            recursive: false,
            depth: 3,
            sort: SortField::Name,
            reverse: false,
        }
    }
}

struct DecryptedFile {
    id: String,
    decrypted_name: String,
    is_folder: bool,
    size_bytes: u64,
    modified: String,
    created: String,
    trashed: bool,
}

pub async fn run(path: Option<String>, opts: LsOpts) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    // Resolve the start folder (UUID passthrough or path walk).
    let parent_id = match &path {
        Some(p) if uuid::Uuid::parse_str(p).is_err() => {
            let resolved = crate::path::resolve_path(&api, &master_key, p).await?;
            if !resolved.is_folder {
                return Err(format!(
                    "'{}' is a file, not a folder. Use `bb pull` to download it.",
                    resolved.name
                ));
            }
            resolved.file_id
        }
        Some(p) => Some(p.clone()),
        None => None,
    };

    let mut listing = decrypt_listing(&api, &master_key, parent_id.as_deref(), false).await?;

    // `-a/--all`: append trashed entries (flat — the trash listing is not
    // per-folder), flagged so they're visually distinct.
    if opts.all {
        let mut trashed = decrypt_listing(&api, &master_key, None, true).await?;
        listing.append(&mut trashed);
    }

    sort_listing(&mut listing, opts.sort, opts.reverse);

    if ui::is_json() {
        return print_json(&api, &master_key, parent_id.as_deref(), &listing, &opts).await;
    }

    if listing.is_empty() {
        if !ui::is_quiet() {
            println!("  {}", "empty \u{2014} no files here".custom_color(colors::INK_DIM));
        }
        return Ok(());
    }

    if !ui::is_quiet() {
        print_header(opts.long);
    }

    let mut total_items = 0u64;
    let mut total_bytes = 0u64;
    for f in &listing {
        total_items += 1;
        if !f.is_folder {
            total_bytes += f.size_bytes;
        }
        print_row(f, 0, &opts);
    }

    // Recurse into subfolders (depth-first), if requested.
    if opts.recursive && opts.depth > 0 {
        for f in &listing {
            if f.is_folder && !f.trashed {
                Box::pin(print_recursive(&api, &master_key, f, 1, &opts)).await?;
            }
        }
    }

    if !ui::is_quiet() {
        println!(
            "\n  {}",
            format!(
                "{} items \u{00B7} {} \u{00B7} {}",
                total_items,
                ui::human_size(total_bytes),
                "e2ee".custom_color(colors::GREEN_OK),
            )
            .custom_color(colors::INK_DIM),
        );
    }

    Ok(())
}

async fn print_recursive(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    folder: &DecryptedFile,
    depth: usize,
    opts: &LsOpts,
) -> Result<(), String> {
    let mut children = decrypt_listing(api, master_key, Some(&folder.id), false).await?;
    if children.is_empty() {
        return Ok(());
    }
    sort_listing(&mut children, opts.sort, opts.reverse);

    if !ui::is_quiet() {
        println!(
            "  {}",
            format!("{}{}/", "  ".repeat(depth), folder.decrypted_name).custom_color(colors::PATH)
        );
    }
    for c in &children {
        print_row(c, depth, opts);
    }
    if depth < opts.depth {
        for c in &children {
            if c.is_folder {
                Box::pin(print_recursive(api, master_key, c, depth + 1, opts)).await?;
            }
        }
    }
    Ok(())
}

/// Fetch + decrypt one folder's children (or the flat trash listing).
async fn decrypt_listing(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    parent_id: Option<&str>,
    trashed: bool,
) -> Result<Vec<DecryptedFile>, String> {
    let result = if trashed {
        api.list_trashed().await?
    } else {
        api.list_files(parent_id).await?
    };
    let files = result
        .as_array()
        .cloned()
        .or_else(|| result.get("files").and_then(|f| f.as_array()).cloned())
        .unwrap_or_default();

    let mut request_keys: Option<crate::commands::request::RequestKeyResolver> =
        if !trashed && files.iter().any(crate::commands::request::is_request_upload) {
            Some(crate::commands::request::RequestKeyResolver::load(api).await?)
        } else {
            None
        };

    let mut out = Vec::with_capacity(files.len());
    for file in &files {
        let file_id = file.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name_encrypted = file.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or("");
        let request_name = request_keys
            .as_mut()
            .and_then(|rk| rk.content_key(master_key, file))
            .and_then(|c| crate::crypto::decrypt_name_with_key(&c, name_encrypted));
        let name = request_name
            .or_else(|| decrypt_name(master_key, file_id, name_encrypted))
            .unwrap_or_else(|| format!("(encrypted) {}", &file_id[..8.min(file_id.len())]));

        out.push(DecryptedFile {
            id: file_id.to_string(),
            decrypted_name: name,
            is_folder: file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false),
            size_bytes: file
                .get("size_bytes")
                .or_else(|| file.get("size"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            modified: file
                .get("updated_at")
                .or_else(|| file.get("modified"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            created: file
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            trashed,
        });
    }
    Ok(out)
}

fn sort_listing(files: &mut [DecryptedFile], field: SortField, reverse: bool) {
    files.sort_by(|a, b| {
        let ord = match field {
            SortField::Name => a
                .decrypted_name
                .to_ascii_lowercase()
                .cmp(&b.decrypted_name.to_ascii_lowercase()),
            SortField::Size => a.size_bytes.cmp(&b.size_bytes),
            SortField::Modified => a.modified.cmp(&b.modified),
            SortField::Created => a.created.cmp(&b.created),
        };
        if reverse { ord.reverse() } else { ord }
    });
}

fn print_header(long: bool) {
    if long {
        println!(
            "  {}",
            format!(
                "{:<40}{:>8}  {:<13}{:<13}{}",
                "NAME", "SIZE", "MODIFIED", "CREATED", "ID"
            )
            .custom_color(colors::INK_DIM),
        );
    } else {
        println!(
            "  {}",
            format!("{:<44}{:>8}  {:<14}{}", "NAME", "SIZE", "MODIFIED", "ID").custom_color(colors::INK_DIM),
        );
    }
}

fn print_row(f: &DecryptedFile, indent: usize, opts: &LsOpts) {
    if ui::is_quiet() {
        println!("{}", f.decrypted_name);
        return;
    }
    let pad = "  ".repeat(indent);
    let icon = ui::file_icon(&f.decrypted_name, f.is_folder);
    let mut name = if f.is_folder {
        format!("{}{}/", pad, f.decrypted_name)
            .custom_color(colors::PATH)
            .to_string()
    } else {
        format!("{}{}", pad, f.decrypted_name)
            .custom_color(colors::INK)
            .to_string()
    };
    if f.trashed {
        name = format!("{name} {}", "[trashed]".custom_color(colors::AMBER));
    }
    let size_str = if f.is_folder {
        "\u{2014}".to_string()
    } else {
        ui::human_size(f.size_bytes)
    };
    let modified = ui::relative_time(&f.modified);
    let id_short = &f.id[..8.min(f.id.len())];

    if opts.long {
        let created = ui::relative_time(&f.created);
        println!(
            "  {} {:<38}{:>8}  {:<13}{:<13}{}",
            icon,
            name,
            size_str.custom_color(colors::INK_DIM),
            modified.custom_color(colors::INK_DIM),
            created.custom_color(colors::INK_DIM),
            id_short.custom_color(colors::INK_DIM),
        );
    } else {
        println!(
            "  {} {:<42}{:>8}  {:<14}{}",
            icon,
            name,
            size_str.custom_color(colors::INK_DIM),
            modified.custom_color(colors::INK_DIM),
            id_short.custom_color(colors::INK_DIM),
        );
    }
}

async fn print_json(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    parent_id: Option<&str>,
    listing: &[DecryptedFile],
    opts: &LsOpts,
) -> Result<(), String> {
    fn to_json(f: &DecryptedFile) -> serde_json::Value {
        serde_json::json!({
            "id": f.id,
            "name": f.decrypted_name,
            "is_folder": f.is_folder,
            "size_bytes": f.size_bytes,
            "modified": f.modified,
            "created": f.created,
            "trashed": f.trashed,
        })
    }

    let mut json_files: Vec<serde_json::Value> = listing.iter().map(to_json).collect();

    // Recursive JSON: flatten descendants with a `parent` pointer for context.
    if opts.recursive && opts.depth > 0 {
        let mut stack: Vec<(String, usize)> = listing
            .iter()
            .filter(|f| f.is_folder && !f.trashed)
            .map(|f| (f.id.clone(), 1usize))
            .collect();
        while let Some((fid, d)) = stack.pop() {
            let children = decrypt_listing(api, master_key, Some(&fid), false).await?;
            for c in &children {
                let mut row = to_json(c);
                row["parent"] = serde_json::json!(fid);
                json_files.push(row);
                if c.is_folder && d < opts.depth {
                    stack.push((c.id.clone(), d + 1));
                }
            }
        }
    }
    let _ = parent_id;

    let total_bytes: u64 = listing.iter().filter(|f| !f.is_folder).map(|f| f.size_bytes).sum();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "files": json_files,
            "total_items": listing.len(),
            "total_bytes": total_bytes,
        }))
        .unwrap()
    );
    Ok(())
}
