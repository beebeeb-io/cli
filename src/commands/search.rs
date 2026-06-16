//! `bb search <query>` — client-side filename search.
//!
//! The server has no blind-search endpoint (zero-knowledge), so the client
//! searches locally. **v2 (task 0810):** fetch the whole vault in ONE request
//! (`GET /api/v1/files/index`), batch-decrypt every name in parallel, build the
//! tree in memory, then DFS it — instead of v1's one `list_files` request per
//! folder (which also only read each folder's first page, silently missing
//! children in folders with >200 entries). Default is a case-insensitive
//! substring match; `--regex` switches to a case-insensitive regex. Results are
//! bounded by `--limit` (default 50); the search can be anchored to a subtree
//! with `--folder`. Children are visited in the same order `list_files` returns
//! them (`is_folder DESC, created_at DESC, id DESC`) so output is unchanged.

use std::collections::HashMap;

use colored::Colorize;
use regex::RegexBuilder;

use crate::api::ApiClient;
use crate::commands::push::load_master_key;
use crate::{colors, path, ui};

/// One search hit.
struct Match {
    id: String,
    name: String,
    path: String,
    size_bytes: u64,
    is_folder: bool,
}

/// A decrypted vault node, built from the whole-vault `/files/index` (task 0810).
struct Node {
    id: String,
    parent: String,
    name: String,
    is_folder: bool,
    size_bytes: u64,
    created_at: String,
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

    // Anchor the search (whole vault, or a --folder subtree).
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

    // Fetch the WHOLE vault in ONE request (v1 did one list_files per folder),
    // then batch-decrypt every name in parallel (task 0810).
    let index = api.files_index().await?;
    let files = index
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let batch_items: Vec<(&str, &str)> = files
        .iter()
        .map(|f| {
            (
                f.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                f.get("name_encrypted").and_then(|v| v.as_str()).unwrap_or(""),
            )
        })
        .collect();
    let names = crate::crypto::decrypt_names(&master_key, &batch_items);

    // Build decrypted nodes (skipping undecryptable entries — exactly the old
    // walk's `else continue`) + a parent_id -> children index. "" = vault root.
    let mut nodes: Vec<Node> = Vec::with_capacity(files.len());
    for (file, name) in files.iter().zip(names) {
        let Some(name) = name else { continue };
        nodes.push(Node {
            id: file.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            parent: file.get("parent_id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            name,
            is_folder: file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false),
            size_bytes: file.get("size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
            created_at: file
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    let mut children: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, n) in nodes.iter().enumerate() {
        children.entry(n.parent.clone()).or_default().push(idx);
    }
    // Visit each folder's children in list_files' order: is_folder DESC,
    // created_at DESC, id DESC (ISO timestamps sort lexically = chronologically).
    for kids in children.values_mut() {
        kids.sort_by(|&a, &b| {
            let (na, nb) = (&nodes[a], &nodes[b]);
            nb.is_folder
                .cmp(&na.is_folder)
                .then_with(|| nb.created_at.cmp(&na.created_at))
                .then_with(|| nb.id.cmp(&na.id))
        });
    }

    let mut results: Vec<Match> = Vec::new();
    let mut walked: u64 = 0;
    let root_key = start_id.unwrap_or_default();
    walk_mem(
        &nodes,
        &children,
        &root_key,
        &start_prefix,
        &*matcher,
        limit,
        &mut results,
        &mut walked,
    );

    if walked > 50_000 && folder.is_none() && !ui::is_json() {
        eprintln!(
            "  {} searched {walked} entries; use --folder to scope a large vault",
            "note:".custom_color(colors::INK_DIM),
        );
    }

    print_results(&results, &query, limit);
    Ok(())
}

/// In-memory depth-first walk of the prebuilt tree (task 0810) — mirrors the old
/// per-folder `walk` exactly: at each folder, emit matching children in order,
/// then recurse into subfolders in order, stopping once `limit` matches collect.
#[allow(clippy::too_many_arguments)]
fn walk_mem(
    nodes: &[Node],
    children: &HashMap<String, Vec<usize>>,
    parent_key: &str,
    prefix: &str,
    matcher: &dyn Fn(&str) -> bool,
    limit: usize,
    out: &mut Vec<Match>,
    walked: &mut u64,
) {
    if out.len() >= limit {
        return;
    }
    let Some(kids) = children.get(parent_key) else {
        return;
    };

    let mut subfolders: Vec<(usize, String)> = Vec::new();
    for &idx in kids {
        let n = &nodes[idx];
        *walked += 1;
        let full_path = format!("{prefix}/{}", n.name);
        if matcher(&n.name) && out.len() < limit {
            out.push(Match {
                id: n.id.clone(),
                name: n.name.clone(),
                path: full_path.clone(),
                size_bytes: n.size_bytes,
                is_folder: n.is_folder,
            });
        }
        if n.is_folder {
            subfolders.push((idx, full_path));
        }
    }

    for (idx, child_prefix) in subfolders {
        if out.len() >= limit {
            break;
        }
        walk_mem(
            nodes,
            children,
            &nodes[idx].id,
            &child_prefix,
            matcher,
            limit,
            out,
            walked,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: &str, name: &str, is_folder: bool) -> Node {
        Node {
            id: id.into(),
            parent: parent.into(),
            name: name.into(),
            is_folder,
            size_bytes: 0,
            created_at: String::new(),
        }
    }

    #[test]
    fn walk_mem_dfs_order_paths_and_walked() {
        // root: folder "Music" (m), file "root.txt" (r)
        // Music: file "NF_song.mp3" (s), folder "Live" (l)
        // Live:  file "NF_live.flac" (f)
        let nodes = vec![
            node("m", "", "Music", true),
            node("r", "", "root.txt", false),
            node("s", "m", "NF_song.mp3", false),
            node("l", "m", "Live", true),
            node("f", "l", "NF_live.flac", false),
        ];
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        children.insert(String::new(), vec![0, 1]);
        children.insert("m".into(), vec![2, 3]);
        children.insert("l".into(), vec![4]);

        let matcher = |n: &str| n.to_ascii_lowercase().contains("nf");
        let mut out = Vec::new();
        let mut walked = 0u64;
        walk_mem(&nodes, &children, "", "", &matcher, 50, &mut out, &mut walked);

        // Matches in DFS order: this-level matches first, then descend in order.
        let paths: Vec<&str> = out.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, vec!["/Music/NF_song.mp3", "/Music/Live/NF_live.flac"]);
        assert_eq!(walked, 5, "every decryptable node in the subtree is counted");
    }

    #[test]
    fn walk_mem_stops_at_limit() {
        let nodes = vec![
            node("a", "", "match-1", false),
            node("b", "", "match-2", false),
            node("c", "", "match-3", false),
        ];
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        children.insert(String::new(), vec![0, 1, 2]);

        let matcher = |n: &str| n.contains("match");
        let mut out = Vec::new();
        let mut walked = 0u64;
        walk_mem(&nodes, &children, "", "", &matcher, 2, &mut out, &mut walked);
        assert_eq!(out.len(), 2, "results bounded by --limit");
    }

    #[test]
    fn walk_mem_anchored_subtree_only() {
        // Anchoring at "m" must only walk Music's subtree, not the root sibling.
        let nodes = vec![
            node("m", "", "Music", true),
            node("other", "", "match-root.txt", false),
            node("s", "m", "match-song.mp3", false),
        ];
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        children.insert(String::new(), vec![0, 1]);
        children.insert("m".into(), vec![2]);

        let matcher = |n: &str| n.contains("match");
        let mut out = Vec::new();
        let mut walked = 0u64;
        walk_mem(&nodes, &children, "m", "/Music", &matcher, 50, &mut out, &mut walked);
        let paths: Vec<&str> = out.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["/Music/match-song.mp3"],
            "only the anchored subtree is searched"
        );
    }
}
