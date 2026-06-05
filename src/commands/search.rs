//! `bb search <query>` — client-side filename search.
//!
//! The server has no blind-search endpoint, so v1 walks the vault tree via
//! `list_files`, decrypts each name locally (zero-knowledge), and matches the
//! query client-side. Default is a case-insensitive substring match; `--regex`
//! switches to a case-insensitive regex. Results are bounded by `--limit`
//! (default 50) and the walk can be anchored to a subtree with `--folder`.

use colored::Colorize;
use regex::RegexBuilder;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::crypto::decrypt_name;
use crate::{colors, path, ui};

/// One search hit.
struct Match {
    id: String,
    name: String,
    path: String,
    size_bytes: u64,
    is_folder: bool,
}

pub async fn run(query: String, regex: bool, limit: usize, folder: Option<String>) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;
    let master_key = load_master_key()?;

    let matcher: Box<dyn Fn(&str) -> bool> = if regex {
        let re = RegexBuilder::new(&query)
            .case_insensitive(true)
            .build()
            .map_err(|e| format!("invalid regex: {e}"))?;
        Box::new(move |n| re.is_match(n))
    } else {
        let needle = query.to_ascii_lowercase();
        Box::new(move |n| n.to_ascii_lowercase().contains(&needle))
    };

    // Anchor the walk (whole vault, or a --folder subtree).
    let (start_id, start_prefix) = match folder.as_deref() {
        Some(p) => {
            let r = path::resolve_path(&api, &master_key, p).await?;
            if !r.is_folder {
                return Err(format!("{p} is a file, not a folder"));
            }
            (r.file_id, format!("/{}", p.trim_matches('/')))
        }
        None => (None, String::new()),
    };

    let mut results: Vec<Match> = Vec::new();
    let mut walked: u64 = 0;
    walk(
        &api,
        &master_key,
        start_id.as_deref(),
        &start_prefix,
        &*matcher,
        limit,
        &mut results,
        &mut walked,
    )
    .await?;

    if walked > 50_000 && folder.is_none() && !ui::is_json() {
        eprintln!(
            "  {} searched {walked} entries; use --folder to scope a large vault",
            "note:".custom_color(colors::INK_DIM),
        );
    }

    print_results(&results, &query, limit);
    Ok(())
}

/// Depth-first walk of the tree, collecting matches until `limit` is reached.
#[allow(clippy::too_many_arguments)]
async fn walk(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    parent_id: Option<&str>,
    prefix: &str,
    matcher: &dyn Fn(&str) -> bool,
    limit: usize,
    out: &mut Vec<Match>,
    walked: &mut u64,
) -> Result<(), String> {
    if out.len() >= limit {
        return Ok(());
    }

    let listing = api.list_files(parent_id).await?;
    let entries = listing
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Collect children first so the borrow on the listing ends before we recurse.
    let mut subfolders: Vec<(String, String)> = Vec::new(); // (id, child_prefix)
    for entry in &entries {
        let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let enc = entry.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or_default();
        let is_folder = entry.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        let size_bytes = entry.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let Some(name) = decrypt_name(master_key, &id, enc) else {
            continue;
        };
        *walked += 1;

        let full_path = format!("{prefix}/{name}");
        if matcher(&name) && out.len() < limit {
            out.push(Match {
                id: id.clone(),
                name: name.clone(),
                path: full_path.clone(),
                size_bytes,
                is_folder,
            });
        }
        if is_folder {
            subfolders.push((id, full_path));
        }
    }

    for (id, child_prefix) in subfolders {
        if out.len() >= limit {
            break;
        }
        Box::pin(walk(
            api,
            master_key,
            Some(&id),
            &child_prefix,
            matcher,
            limit,
            out,
            walked,
        ))
        .await?;
    }

    Ok(())
}

fn print_results(results: &[Match], query: &str, limit: usize) {
    if ui::is_json() {
        let rows: Vec<serde_json::Value> = results
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "name": m.name,
                    "path": m.path,
                    "is_folder": m.is_folder,
                    "size_bytes": m.size_bytes,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "matches": rows })).unwrap()
        );
        return;
    }

    if results.is_empty() {
        if !ui::is_quiet() {
            println!(
                "  {}",
                format!("no matches for \"{query}\"").custom_color(colors::INK_DIM)
            );
        }
        return;
    }

    for m in results {
        if ui::is_quiet() {
            println!("{}", m.path);
            continue;
        }
        let icon = ui::file_icon(&m.name, m.is_folder);
        let size = if m.is_folder {
            "\u{2014}".to_string()
        } else {
            ui::human_size(m.size_bytes)
        };
        let id_short = &m.id[..8.min(m.id.len())];
        println!(
            "  {} {:<46}{:>8}  {}",
            icon,
            m.path.custom_color(colors::INK),
            size.custom_color(colors::INK_DIM),
            id_short.custom_color(colors::INK_DIM),
        );
    }

    if !ui::is_quiet() {
        let suffix = if results.len() >= limit {
            " (limit reached — raise with --limit)"
        } else {
            ""
        };
        println!();
        println!(
            "  {}",
            format!("{} match(es){suffix}", results.len()).custom_color(colors::INK_DIM)
        );
    }
}
