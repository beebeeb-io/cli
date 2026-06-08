use base64::Engine;
use beebeeb_types::quota::{Plan, effective_quota, format_storage_si};
use colored::Colorize;
use std::io::{self, Write};
use std::path::PathBuf;
use zeroize::Zeroizing;

use crate::api::ApiClient;
use crate::config::load_config;
use crate::ui;

// Chunk size is now computed dynamically via beebeeb_types::plan_chunks().
// See the adaptive chunk-size ladder in beebeeb-types/src/chunk.rs.

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Load the master key from config, returning a beebeeb_core MasterKey.
pub(crate) fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config.master_key.ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
        b64()
            .decode(&mk_b64)
            .map_err(|e| format!("invalid master key in config: {e}"))?,
    );
    if mk_bytes.len() != 32 {
        return Err(format!("master key must be 32 bytes, got {}", mk_bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    Ok(beebeeb_core::kdf::MasterKey::from_bytes(arr))
}

/// How to handle a filename conflict when uploading.
#[derive(Clone, Copy, PartialEq)]
pub enum ConflictStrategy {
    /// Ask interactively (default when a TTY is attached).
    Prompt,
    /// Replace the existing file by versioning over it.
    Replace,
    /// Keep both — upload under a suffixed name.
    KeepBoth,
}

/// Outcome of conflict resolution for a single file.
enum ConflictResolution {
    /// Upload, version over the existing file with this server ID.
    Replace { existing_id: uuid::Uuid },
    /// Upload under this (possibly modified) filename as a new file.
    Upload { filename: String },
    /// Skip this file entirely.
    Skip,
}

/// Ask the server for all encrypted names in `parent_id`, decrypt them
/// locally, and return the existing file's UUID if `filename` matches.
///
/// Uses the shared `crypto::decrypt_name` which handles all server formats
/// (Rust EncryptedBlob, web-app base64 blob, plaintext) and both UUID key
/// derivations (binary and string).
pub(crate) async fn find_conflict(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    filename: &str,
    parent_uuid: Option<uuid::Uuid>,
) -> Option<uuid::Uuid> {
    let resp = api.check_conflict(parent_uuid).await.ok()?;
    let files = resp.get("files")?.as_array()?;

    for file in files {
        let id_str = file.get("id")?.as_str()?;
        let name_enc_str = file.get("name_encrypted")?.as_str()?;
        let file_uuid: uuid::Uuid = id_str.parse().ok()?;
        if let Some(decrypted) = crate::crypto::decrypt_name(master_key, id_str, name_enc_str) {
            if decrypted == filename {
                return Some(file_uuid);
            }
        }
    }
    None
}

/// Prompt the user interactively for conflict resolution.
/// Returns Ok(resolution) or Err if stdin is not interactive.
fn prompt_conflict(filename: &str) -> ConflictResolution {
    eprint!(
        "  {} {} [R]eplace / [K]eep both / [S]kip: ",
        filename.custom_color(crate::colors::AMBER),
        "already exists.".custom_color(crate::colors::INK_DIM),
    );
    let _ = io::stderr().flush();

    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return ConflictResolution::Skip;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "r" | "replace" => ConflictResolution::Replace {
            existing_id: uuid::Uuid::nil(),
        }, // filled by caller
        "k" | "keep" | "keep both" => ConflictResolution::Upload {
            filename: filename.to_string(),
        },
        _ => ConflictResolution::Skip,
    }
}

pub async fn run(
    path: PathBuf,
    parent_id: Option<String>,
    folder: Option<String>,
    replace: bool,
    keep_both: bool,
) -> Result<(), String> {
    let strategy = if replace {
        ConflictStrategy::Replace
    } else if keep_both {
        ConflictStrategy::KeepBoth
    } else {
        ConflictStrategy::Prompt
    };

    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }

    let api = ApiClient::from_config();
    api.require_auth()?;

    // ── Quota pre-check ────────────────────────────────────────────────────
    // Compute total size of the upload, then check against the user's quota.
    let upload_size = if path.is_dir() {
        dir_total_size(&path)
    } else {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    };
    check_quota(&api, upload_size).await?;

    let parent_id = match (parent_id, folder) {
        (Some(parent_id), None) => Some(parent_id),
        (None, Some(folder)) => Some(resolve_upload_folder(&api, &folder).await?),
        (None, None) => None,
        (Some(_), Some(_)) => return Err("use either --parent or --folder, not both".to_string()),
    };

    if path.is_dir() {
        return push_directory(&api, &path, parent_id, strategy).await;
    }

    // ── Resolve line (single file) ──────────────────────────────────────────
    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if ui::is_rich() {
        println!(
            "  {}",
            format!(
                "1 file \u{00B7} {} \u{00B7} encrypting with AES-256-GCM",
                ui::human_size(file_size),
            )
            .custom_color(crate::colors::INK_DIM),
        );
    }

    let total_start = std::time::Instant::now();
    let result = push_single_file(&api, &path, parent_id, strategy, false).await?;
    let total_elapsed = total_start.elapsed();

    match result {
        Some(ur) => {
            if ui::is_json() {
                let speed = ur.speed_bps();
                let json = serde_json::json!({
                    "files": [{
                        "name": ur.name,
                        "id": ur.id,
                        "size_bytes": ur.size_bytes,
                        "speed_bps": speed as u64,
                        "duration_ms": ur.duration.as_millis() as u64,
                    }],
                    "total_files": 1,
                    "total_bytes": ur.encrypted_bytes,
                    "avg_speed_bps": speed as u64,
                    "total_duration_ms": total_elapsed.as_millis() as u64,
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            } else if ui::is_rich() {
                let speed = ur.speed_bps();
                println!(
                    "  {} {}",
                    "\u{2713}".custom_color(crate::colors::GREEN_OK),
                    format!(
                        "1 file uploaded \u{00B7} {} \u{00B7} avg {} \u{00B7} {:.1}s total",
                        ui::human_size(ur.encrypted_bytes),
                        ui::human_speed(speed),
                        total_elapsed.as_secs_f64(),
                    )
                    .custom_color(crate::colors::INK_DIM),
                );
            }
        }
        None => {
            // File was skipped (conflict resolution)
            if ui::is_json() {
                let json = serde_json::json!({
                    "files": [],
                    "total_files": 0,
                    "total_bytes": 0,
                    "avg_speed_bps": 0,
                    "total_duration_ms": total_elapsed.as_millis() as u64,
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            }
        }
    }

    Ok(())
}

async fn resolve_upload_folder(api: &ApiClient, folder: &str) -> Result<String, String> {
    if folder.trim().is_empty() {
        return Err("--folder must not be empty".to_string());
    }

    if let Ok(folder_id) = folder.parse::<uuid::Uuid>() {
        return Ok(folder_id.to_string());
    }

    let master_key = load_master_key()?;
    let listing = api.list_files(None).await?;
    let files = listing
        .as_array()
        .or_else(|| listing.get("files").and_then(|f| f.as_array()))
        .ok_or("invalid file listing response")?;

    for file in files {
        let is_folder = file.get("is_folder").and_then(|v| v.as_bool()).unwrap_or(false);
        if !is_folder {
            continue;
        }

        let file_id = file
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("folder response missing id")?;
        let name_encrypted = file
            .get("name_encrypted")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("folder response missing name_encrypted for {file_id}"))?;
        let name = crate::crypto::decrypt_name(&master_key, file_id, name_encrypted)
            .ok_or_else(|| format!("failed to decrypt folder name for {file_id}"))?;

        if name == folder {
            return Ok(file_id.to_string());
        }
    }

    let folder_id = uuid::Uuid::new_v4();
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(&master_key, &folder_id.to_string(), folder, None)
        .map_err(|e| format!("failed to encrypt folder name: {e}"))?;

    let created = api.create_folder(&name_encrypted, None, Some(folder_id)).await?;
    let created_id = created
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("server response missing folder id")?;
    Ok(created_id.to_string())
}

/// Result data from a single file upload, used for summary + JSON output.
struct UploadResult {
    name: String,
    id: String,
    size_bytes: u64,
    encrypted_bytes: u64,
    duration: std::time::Duration,
}

impl UploadResult {
    fn speed_bps(&self) -> f64 {
        let secs = self.duration.as_secs_f64();
        if secs > 0.0 {
            self.encrypted_bytes as f64 / secs
        } else {
            0.0
        }
    }
}

/// Upload a single file. When `suppress_output` is true, no per-file line is
/// printed (the caller handles display — used by `push_directory`).
async fn push_single_file(
    api: &ApiClient,
    path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
    suppress_output: bool,
) -> Result<Option<UploadResult>, String> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();

    // The streaming uploader stats + reads the file itself in bounded chunks;
    // no full read here.
    let master_key = load_master_key()?;

    let parent_uuid: Option<uuid::Uuid> = parent_id.as_deref().and_then(|s| s.parse().ok());

    // ── Conflict detection ────────────────────────────────────────────────────
    // Decrypt existing filenames in the folder and check for a match.
    // If found, ask the user (or apply the strategy flag) how to proceed.
    let (file_id, upload_name, replace_file_id) = {
        let existing = find_conflict(api, &master_key, &file_name, parent_uuid).await;

        match existing {
            None => (uuid::Uuid::new_v4(), file_name.clone(), None),
            Some(existing_id) => {
                // Determine resolution
                let resolution = match strategy {
                    ConflictStrategy::Replace => ConflictResolution::Replace { existing_id },
                    ConflictStrategy::KeepBoth => {
                        let (stem, ext) = split_stem_ext(&file_name);
                        let new_name = if ext.is_empty() {
                            format!("{stem} (1)")
                        } else {
                            format!("{stem} (1).{ext}")
                        };
                        ConflictResolution::Upload { filename: new_name }
                    }
                    ConflictStrategy::Prompt => {
                        let r = prompt_conflict(&file_name);
                        // If user chose Replace, inject the actual existing_id
                        match r {
                            ConflictResolution::Replace { .. } => ConflictResolution::Replace { existing_id },
                            ConflictResolution::Upload { .. } => {
                                let (stem, ext) = split_stem_ext(&file_name);
                                let new_name = if ext.is_empty() {
                                    format!("{stem} (1)")
                                } else {
                                    format!("{stem} (1).{ext}")
                                };
                                ConflictResolution::Upload { filename: new_name }
                            }
                            ConflictResolution::Skip => {
                                if ui::is_rich() {
                                    println!(
                                        "  {} {}",
                                        "skip".custom_color(crate::colors::INK_DIM),
                                        file_name.custom_color(crate::colors::INK_DIM),
                                    );
                                }
                                return Ok(None);
                            }
                        }
                    }
                };

                match resolution {
                    ConflictResolution::Replace { existing_id } => {
                        // Re-use the existing UUID so the server auto-versions.
                        // Key is derived from the existing ID (same as when the
                        // file was first uploaded).
                        (existing_id, file_name.clone(), Some(existing_id))
                    }
                    ConflictResolution::Upload { filename } => (uuid::Uuid::new_v4(), filename, None),
                    ConflictResolution::Skip => return Ok(None),
                }
            }
        }
    };

    // ── Streaming, constant-memory upload ─────────────────────────────────────
    // For a replace, `file_id` already equals the existing id, so the file key,
    // the encrypted name, and the server-side version all derive from one id.
    // The v2 upload driver versions by `file_id` + `base_version_number`, so on
    // a replace we fetch the existing file's current `version_number` and pass
    // it as the optimistic-concurrency base (the server returns a stale-version
    // 409 if it no longer matches). On a fresh push, `base_version_number` stays
    // `None` and the absent-`file_id`/new-`file_id` decides "new file".
    let master_key = std::sync::Arc::new(master_key);
    let base_version_number: Option<i32> = match replace_file_id {
        Some(existing_id) => {
            let meta = api.get_file(&existing_id.to_string()).await?;
            // version_number is the file's current version; default 1 if the
            // server omits it (older rows) — the v2 init still proceeds and only
            // a genuine mismatch 409s.
            Some(meta.get("version_number").and_then(|v| v.as_i64()).unwrap_or(1) as i32)
        }
        None => None,
    };
    let display_name = upload_name.clone();

    let parent_uuid: Option<uuid::Uuid> = match &parent_id {
        Some(pid) => Some(pid.parse().map_err(|e| format!("invalid parent ID: {e}"))?),
        None => None,
    };

    let file_start = std::time::Instant::now();
    let spec = crate::upload::UploadSpec {
        path: path.to_path_buf(),
        file_name: upload_name,
        file_id,
        base_version_number,
        parent_id: parent_uuid,
        // bb push uploads one file at a time → full Cli cap (128 MiB).
        concurrency: 1,
        shutdown: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let outcome = crate::upload::stream_encrypt_upload(
        api,
        std::sync::Arc::clone(&master_key),
        spec,
        &crate::upload::NoopProgress,
    )
    .await?;
    let server_id = outcome.server_id.to_string();
    let file_elapsed = file_start.elapsed();
    let file_name = display_name; // possibly-suffixed name for display

    // Mark this upload so a `bb watch` remote-event handler in the same
    // process doesn't redundantly download the file we just sent.
    crate::loopback::mark_uploaded(&server_id);

    let upload_result = UploadResult {
        name: file_name.clone(),
        id: server_id.clone(),
        size_bytes: outcome.plaintext_bytes,
        encrypted_bytes: outcome.ciphertext_bytes,
        duration: file_elapsed,
    };

    // ── Per-file output (only when not suppressed by directory caller) ────
    if !suppress_output {
        if ui::is_quiet() {
            println!("{}", server_id);
        } else if ui::is_rich() {
            let speed = upload_result.speed_bps();
            println!(
                "  {} {:<36}{:>8}  {:>10}  {}",
                "\u{2713}".custom_color(crate::colors::GREEN_OK),
                file_name.custom_color(crate::colors::INK),
                ui::human_size(upload_result.encrypted_bytes).custom_color(crate::colors::INK_DIM),
                ui::human_speed(speed).custom_color(crate::colors::GREEN_OK),
                format!("{}ms", file_elapsed.as_millis()).custom_color(crate::colors::INK_DIM),
            );
        }
        // JSON mode: no per-file output; caller collects results
    } else if ui::is_quiet() {
        // Even in directory mode, quiet still prints IDs
        println!("{}", server_id);
    }

    Ok(Some(upload_result))
}

async fn push_directory(
    api: &ApiClient,
    dir_path: &std::path::Path,
    parent_id: Option<String>,
    strategy: ConflictStrategy,
) -> Result<(), String> {
    let master_key = load_master_key()?;
    let dir_name = dir_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("folder")
        .to_string();

    let mut entries: Vec<(PathBuf, bool)> = Vec::new();
    collect_entries(dir_path, &mut entries)?;

    let file_count = entries.iter().filter(|(_, is_dir)| !*is_dir).count();
    let _folder_count = entries.iter().filter(|(_, is_dir)| *is_dir).count();
    let total_file_size: u64 = entries
        .iter()
        .filter(|(_, is_dir)| !*is_dir)
        .filter_map(|(p, _)| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    // ── Resolve line ────────────────────────────────────────────────────────
    if ui::is_rich() {
        println!(
            "  {}",
            format!(
                "{file_count} files \u{00B7} {} \u{00B7} encrypting with AES-256-GCM",
                ui::human_size(total_file_size),
            )
            .custom_color(crate::colors::INK_DIM),
        );
    }

    let folder_id = uuid::Uuid::new_v4();
    let name_encrypted = beebeeb_core::encrypt::encrypt_name(&master_key, &folder_id.to_string(), &dir_name, None)
        .map_err(|e| format!("failed to encrypt folder name: {e}"))?;

    let parent_uuid = parent_id
        .as_deref()
        .map(|p| p.parse::<uuid::Uuid>())
        .transpose()
        .map_err(|e| format!("invalid parent ID: {e}"))?;

    let root_folder = api.create_folder(&name_encrypted, parent_uuid, Some(folder_id)).await?;
    let root_id: uuid::Uuid = root_folder
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("missing folder id")?
        .parse()
        .map_err(|e| format!("invalid folder id: {e}"))?;

    let mut folder_map: std::collections::HashMap<PathBuf, uuid::Uuid> = std::collections::HashMap::new();
    folder_map.insert(dir_path.to_path_buf(), root_id);

    let total_start = std::time::Instant::now();
    let mut upload_results: Vec<UploadResult> = Vec::new();

    for (entry_path, is_dir) in &entries {
        let entry_parent = entry_path.parent().unwrap_or(dir_path);
        let entry_parent_id = folder_map.get(entry_parent).copied().unwrap_or(root_id);

        if *is_dir {
            let name = entry_path.file_name().and_then(|n| n.to_str()).unwrap_or("folder");

            let sub_id = uuid::Uuid::new_v4();
            let sub_enc = beebeeb_core::encrypt::encrypt_name(&master_key, &sub_id.to_string(), name, None)
                .map_err(|e| format!("failed to encrypt subfolder name: {e}"))?;

            let result = api.create_folder(&sub_enc, Some(entry_parent_id), Some(sub_id)).await?;
            let created_id: uuid::Uuid = result
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("missing folder id")?
                .parse()
                .map_err(|e| format!("invalid id: {e}"))?;

            folder_map.insert(entry_path.clone(), created_id);
        } else {
            let ur = push_single_file(api, entry_path, Some(entry_parent_id.to_string()), strategy, true).await?;

            if let Some(ur) = ur {
                if ui::is_rich() {
                    let speed = ur.speed_bps();
                    let name = &ur.name;
                    let elapsed = ur.duration;
                    let enc_size = ur.encrypted_bytes;
                    println!(
                        "  {} {:<36}{:>8}  {:>10}  {}",
                        "\u{2713}".custom_color(crate::colors::GREEN_OK),
                        name.custom_color(crate::colors::INK),
                        ui::human_size(enc_size).custom_color(crate::colors::INK_DIM),
                        ui::human_speed(speed).custom_color(crate::colors::GREEN_OK),
                        format!("{}ms", elapsed.as_millis()).custom_color(crate::colors::INK_DIM),
                    );
                }
                // quiet mode: push_single_file already printed the ID
                upload_results.push(ur);
            }
        }
    }

    let total_elapsed = total_start.elapsed();
    let total_bytes: u64 = upload_results.iter().map(|r| r.encrypted_bytes).sum();
    let uploaded_count = upload_results.len();

    // ── Summary ─────────────────────────────────────────────────────────────
    if ui::is_json() {
        let files_json: Vec<serde_json::Value> = upload_results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "id": r.id,
                    "size_bytes": r.size_bytes,
                    "speed_bps": r.speed_bps() as u64,
                    "duration_ms": r.duration.as_millis() as u64,
                })
            })
            .collect();
        let avg_speed = if total_elapsed.as_secs_f64() > 0.0 {
            total_bytes as f64 / total_elapsed.as_secs_f64()
        } else {
            0.0
        };
        let json = serde_json::json!({
            "files": files_json,
            "total_files": uploaded_count,
            "total_bytes": total_bytes,
            "avg_speed_bps": avg_speed as u64,
            "total_duration_ms": total_elapsed.as_millis() as u64,
        });
        println!("{}", serde_json::to_string_pretty(&json).unwrap());
    } else if ui::is_rich() {
        let avg_speed = if total_elapsed.as_secs_f64() > 0.0 {
            total_bytes as f64 / total_elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!(
            "  {} {}",
            "\u{2713}".custom_color(crate::colors::GREEN_OK),
            format!(
                "{uploaded_count} files uploaded \u{00B7} {} \u{00B7} avg {} \u{00B7} {:.1}s total",
                ui::human_size(total_bytes),
                ui::human_speed(avg_speed),
                total_elapsed.as_secs_f64(),
            )
            .custom_color(crate::colors::INK_DIM),
        );
    }

    Ok(())
}

fn collect_entries(dir: &std::path::Path, entries: &mut Vec<(PathBuf, bool)>) -> Result<(), String> {
    let read_dir = std::fs::read_dir(dir).map_err(|e| format!("failed to read directory: {e}"))?;
    let mut sorted: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
    sorted.sort_by_key(|e| e.file_name());

    for entry in sorted {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            entries.push((path.clone(), true));
            collect_entries(&path, entries)?;
        } else {
            entries.push((path, false));
        }
    }
    Ok(())
}

/// Split "report.pdf" → ("report", "pdf"); "notes" → ("notes", "").
fn split_stem_ext(filename: &str) -> (&str, &str) {
    match filename.rfind('.') {
        Some(i) if i > 0 => (&filename[..i], &filename[i + 1..]),
        _ => (filename, ""),
    }
}

/// Recursively compute the total size of all files in a directory.
fn dir_total_size(dir: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

/// Fetch the user's subscription and usage, then reject the upload if
/// `used_bytes + upload_size` exceeds the effective quota.
async fn check_quota(api: &ApiClient, upload_size: u64) -> Result<(), String> {
    let (usage_res, sub_res) = tokio::join!(api.get_usage(), api.get_subscription());

    let usage = usage_res.unwrap_or_default();
    let sub = sub_res.unwrap_or_default();

    let used_bytes = usage.get("used_bytes").and_then(|v| v.as_i64()).unwrap_or(0);

    let plan_slug = sub.get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    let plan = Plan::from_slug(plan_slug);
    let extra_tb = sub.get("extra_storage_tb").and_then(|v| v.as_i64()).unwrap_or(0);
    let bonus_bytes = sub.get("bonus_bytes").and_then(|v| v.as_i64()).unwrap_or(0);
    let quota = effective_quota(plan, extra_tb, bonus_bytes);

    if quota > 0 && used_bytes + upload_size as i64 > quota {
        let used_str = format_storage_si(used_bytes);
        let quota_str = format_storage_si(quota);
        let extra_hint = if plan.can_add_storage() {
            " Add more storage at app.beebeeb.io/billing"
        } else {
            ""
        };
        return Err(format!("Storage full ({used_str} / {quota_str}).{extra_hint}"));
    }

    Ok(())
}
