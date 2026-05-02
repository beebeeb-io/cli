use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine;
use beebeeb_types::EncryptedBlob;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::api::ApiClient;
use crate::config::load_config;

const STATE_FILE: &str = ".bb-sync.json";
const CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Serialize, Deserialize, Default, Clone)]
struct SyncState {
    remote_path: Option<String>,
    remote_folder_id: Option<Uuid>,
    last_sync: Option<DateTime<Utc>>,
    #[serde(default)]
    files: HashMap<String, FileEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct FileEntry {
    remote_id: Uuid,
    last_mtime: i64,
    last_size: u64,
    last_sync: DateTime<Utc>,
}

#[derive(Clone)]
struct LocalFile {
    mtime: i64,
    size: u64,
}

#[derive(Clone)]
struct RemoteFile {
    id: Uuid,
    chunk_count: u32,
    updated_at: DateTime<Utc>,
}

pub async fn run(
    local_dir: PathBuf,
    remote_path_arg: Option<String>,
    dry_run: bool,
    force: bool,
    delete_remote: bool,
) -> Result<(), String> {
    let api = ApiClient::from_config();
    api.require_auth()?;

    if !local_dir.exists() {
        return Err(format!("local directory not found: {}", local_dir.display()));
    }
    if !local_dir.is_dir() {
        return Err(format!("not a directory: {}", local_dir.display()));
    }

    let local_dir = std::fs::canonicalize(&local_dir)
        .map_err(|e| format!("cannot resolve path: {e}"))?;

    let state_path = local_dir.join(STATE_FILE);
    let mut state: SyncState = if state_path.exists() {
        let data = std::fs::read(&state_path).map_err(|e| format!("read state: {e}"))?;
        serde_json::from_slice(&data).map_err(|e| format!("invalid state file: {e}"))?
    } else {
        SyncState::default()
    };

    let remote_path = match remote_path_arg.or_else(|| state.remote_path.clone()) {
        Some(p) => p,
        None => {
            return Err("remote_path is required (no .bb-sync.json found). \
                        Usage: bb sync <local_dir> <remote_path>"
                .to_string());
        }
    };

    let master_key = load_master_key()?;

    println!(
        "  {} {}",
        "syncing".truecolor(143, 193, 139),
        local_dir.display().to_string().truecolor(233, 230, 221),
    );
    println!(
        "  {} {}",
        "remote".truecolor(106, 101, 91),
        remote_path.truecolor(233, 230, 221),
    );
    if dry_run {
        println!(
            "  {} {}",
            "mode".truecolor(106, 101, 91),
            "dry-run (no changes will be written)".truecolor(245, 184, 0),
        );
    }
    println!();

    let remote_folder_id =
        resolve_remote_folder(&api, &master_key, &remote_path, dry_run).await?;
    state.remote_path = Some(remote_path.clone());
    state.remote_folder_id = Some(remote_folder_id);

    let local_files = walk_local_files(&local_dir)?;
    let local_folders = walk_local_folders(&local_dir)?;

    let mut remote_files: HashMap<String, RemoteFile> = HashMap::new();
    let mut remote_folders: HashMap<String, Uuid> = HashMap::new();
    walk_remote(
        &api,
        &master_key,
        remote_folder_id,
        "",
        &mut remote_files,
        &mut remote_folders,
    )
    .await?;

    let mut all_folders: HashSet<String> = HashSet::new();
    all_folders.extend(local_folders.iter().cloned());
    all_folders.extend(remote_folders.keys().cloned());
    let mut sorted_folders: Vec<String> = all_folders.into_iter().collect();
    sorted_folders.sort_by_key(|s| s.matches('/').count());

    for folder_rel in &sorted_folders {
        if !remote_folders.contains_key(folder_rel) {
            let parent_id =
                parent_remote_id(folder_rel, &remote_folders, remote_folder_id);
            let folder_name = folder_rel.rsplit('/').next().unwrap_or(folder_rel);
            if dry_run {
                println!(
                    "  {} {} {}",
                    "+".truecolor(143, 193, 139),
                    "would create remote folder".truecolor(106, 101, 91),
                    folder_rel.truecolor(233, 230, 221),
                );
            } else {
                let new_id = create_folder(&api, &master_key, folder_name, parent_id).await?;
                remote_folders.insert(folder_rel.clone(), new_id);
                println!(
                    "  {} {} {}",
                    "+".truecolor(143, 193, 139),
                    "remote folder".truecolor(106, 101, 91),
                    folder_rel.truecolor(233, 230, 221),
                );
            }
        }
        if !local_folders.contains(folder_rel) {
            let local_path = local_dir.join(folder_rel);
            if !dry_run {
                std::fs::create_dir_all(&local_path)
                    .map_err(|e| format!("mkdir {}: {e}", local_path.display()))?;
            }
            println!(
                "  {} {} {}",
                "+".truecolor(143, 193, 139),
                "local folder".truecolor(106, 101, 91),
                folder_rel.truecolor(233, 230, 221),
            );
        }
    }

    let mut all_files: HashSet<String> = HashSet::new();
    all_files.extend(local_files.keys().cloned());
    all_files.extend(remote_files.keys().cloned());
    let mut sorted_files: Vec<String> = all_files.into_iter().collect();
    sorted_files.sort();

    let mut up_count = 0u32;
    let mut down_count = 0u32;
    let mut conflicts = 0u32;
    let mut deletes = 0u32;
    let mut skipped = 0u32;

    for rel in &sorted_files {
        let local = local_files.get(rel).cloned();
        let remote = remote_files.get(rel).cloned();
        let prior = state.files.get(rel).cloned();

        match (local, remote, prior) {
            (Some(l), Some(r), Some(p)) => {
                let local_changed = l.mtime != p.last_mtime || l.size != p.last_size;
                let remote_changed = r.updated_at > p.last_sync + chrono::Duration::seconds(1);
                match (local_changed, remote_changed) {
                    (false, false) => {
                        skipped += 1;
                    }
                    (true, false) => {
                        do_upload(
                            &api,
                            &master_key,
                            &local_dir,
                            rel,
                            &l,
                            Some(r.id),
                            remote_folder_id,
                            &remote_folders,
                            &mut state,
                            dry_run,
                        )
                        .await?;
                        up_count += 1;
                    }
                    (false, true) => {
                        do_download(&api, &master_key, &local_dir, rel, &r, &mut state, dry_run)
                            .await?;
                        down_count += 1;
                    }
                    (true, true) => {
                        if force {
                            do_upload(
                                &api,
                                &master_key,
                                &local_dir,
                                rel,
                                &l,
                                Some(r.id),
                                remote_folder_id,
                                &remote_folders,
                                &mut state,
                                dry_run,
                            )
                            .await?;
                            up_count += 1;
                        } else {
                            println!(
                                "  {} {} {}",
                                "⚡".truecolor(245, 184, 0),
                                "conflict".truecolor(245, 184, 0),
                                format!("{rel} (skipped — use --force to overwrite)")
                                    .truecolor(233, 230, 221),
                            );
                            conflicts += 1;
                        }
                    }
                }
            }
            (Some(l), Some(r), None) => {
                if force {
                    do_upload(
                        &api,
                        &master_key,
                        &local_dir,
                        rel,
                        &l,
                        Some(r.id),
                        remote_folder_id,
                        &remote_folders,
                        &mut state,
                        dry_run,
                    )
                    .await?;
                    up_count += 1;
                } else {
                    println!(
                        "  {} {} {}",
                        "⚡".truecolor(245, 184, 0),
                        "conflict".truecolor(245, 184, 0),
                        format!("{rel} (exists both sides, no prior sync — skipped)")
                            .truecolor(233, 230, 221),
                    );
                    conflicts += 1;
                }
            }
            (Some(l), None, _) => {
                do_upload(
                    &api,
                    &master_key,
                    &local_dir,
                    rel,
                    &l,
                    None,
                    remote_folder_id,
                    &remote_folders,
                    &mut state,
                    dry_run,
                )
                .await?;
                up_count += 1;
            }
            (None, Some(r), prior) => {
                if prior.is_some() {
                    if delete_remote {
                        if !dry_run {
                            api.trash_file(&r.id.to_string()).await?;
                            state.files.remove(rel);
                        }
                        println!(
                            "  {} {} {}",
                            "x".truecolor(224, 122, 106),
                            "trashing remote".truecolor(224, 122, 106),
                            rel.truecolor(233, 230, 221),
                        );
                        deletes += 1;
                    } else {
                        println!(
                            "  {} {} {}",
                            "?".truecolor(106, 101, 91),
                            "missing locally".truecolor(106, 101, 91),
                            format!("{rel} (use --delete to trash from vault)")
                                .truecolor(106, 101, 91),
                        );
                    }
                } else {
                    do_download(&api, &master_key, &local_dir, rel, &r, &mut state, dry_run)
                        .await?;
                    down_count += 1;
                }
            }
            (None, None, _) => {}
        }
    }

    state.last_sync = Some(Utc::now());
    if !dry_run {
        let data = serde_json::to_vec_pretty(&state)
            .map_err(|e| format!("serialize state: {e}"))?;
        std::fs::write(&state_path, data)
            .map_err(|e| format!("write state file: {e}"))?;
    }

    println!();
    let summary = format!(
        "{} up, {} down, {} conflicts{}{}",
        up_count,
        down_count,
        conflicts,
        if deletes > 0 {
            format!(", {deletes} deleted")
        } else {
            String::new()
        },
        if skipped > 0 {
            format!(", {skipped} unchanged")
        } else {
            String::new()
        },
    );
    let total = up_count + down_count + conflicts + deletes + skipped;
    println!(
        "  {} {} {} {}",
        "OK".truecolor(143, 193, 139),
        format!("synced {total} file{}", if total == 1 { "" } else { "s" })
            .truecolor(233, 230, 221),
        "·".truecolor(106, 101, 91),
        summary.truecolor(106, 101, 91),
    );

    Ok(())
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn load_master_key() -> Result<beebeeb_core::kdf::MasterKey, String> {
    let config = load_config();
    let mk_b64 = config
        .master_key
        .ok_or("No master key found. Run `bb login` first.")?;
    let mk_bytes = b64()
        .decode(&mk_b64)
        .map_err(|e| format!("invalid master key in config: {e}"))?;
    if mk_bytes.len() != 32 {
        return Err(format!(
            "master key must be 32 bytes, got {}",
            mk_bytes.len()
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&mk_bytes);
    Ok(beebeeb_core::kdf::MasterKey::from_bytes(arr))
}

fn rel_path_str(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| c.as_os_str().to_str().map(String::from))
        .collect::<Vec<_>>()
        .join("/")
}

fn walk_local_files(root: &Path) -> Result<HashMap<String, LocalFile>, String> {
    let mut out = HashMap::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let meta = std::fs::metadata(path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.insert(
            rel_path_str(rel),
            LocalFile {
                mtime,
                size: meta.len(),
            },
        );
    }
    Ok(out)
}

fn walk_local_folders(root: &Path) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() || path == root {
            continue;
        }
        let rel = match path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if rel
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        out.insert(rel_path_str(rel));
    }
    Ok(out)
}

fn parent_remote_id(
    rel: &str,
    folders: &HashMap<String, Uuid>,
    root_id: Uuid,
) -> Option<Uuid> {
    match rel.rsplit_once('/') {
        Some((parent, _)) => folders.get(parent).copied().or(Some(root_id)),
        None => Some(root_id),
    }
}

async fn resolve_remote_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    remote_path: &str,
    dry_run: bool,
) -> Result<Uuid, String> {
    let segments: Vec<&str> = remote_path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return Err("remote_path cannot be empty or root".to_string());
    }

    let mut current_parent: Option<Uuid> = None;

    for seg in segments {
        let listing = api
            .list_files(current_parent.map(|u| u.to_string()).as_deref())
            .await?;
        let items = listing
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or("invalid file listing")?;

        let mut found: Option<Uuid> = None;
        for item in items {
            let is_folder = item
                .get("is_folder")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !is_folder {
                continue;
            }
            let id_str = match item.get("id").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let id: Uuid = match id_str.parse() {
                Ok(u) => u,
                Err(_) => continue,
            };
            let name_enc = item
                .get("name_encrypted")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let item_key = beebeeb_core::kdf::derive_file_key(master_key, id.as_bytes());
            let name = serde_json::from_str::<EncryptedBlob>(name_enc)
                .ok()
                .and_then(|b| beebeeb_core::encrypt::decrypt_metadata(&item_key, &b).ok());
            if name.as_deref() == Some(seg) {
                found = Some(id);
                break;
            }
        }

        match found {
            Some(id) => current_parent = Some(id),
            None => {
                if dry_run {
                    return Err(format!(
                        "remote folder '{seg}' does not exist (dry-run, cannot create)"
                    ));
                }
                let new_id = create_folder(api, master_key, seg, current_parent).await?;
                println!(
                    "  {} {} {}",
                    "+".truecolor(143, 193, 139),
                    "remote folder".truecolor(106, 101, 91),
                    seg.truecolor(233, 230, 221),
                );
                current_parent = Some(new_id);
            }
        }
    }

    current_parent.ok_or_else(|| "path resolution failed".to_string())
}

async fn create_folder(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let new_id = Uuid::new_v4();
    let folder_key = beebeeb_core::kdf::derive_file_key(master_key, new_id.as_bytes());
    let blob = beebeeb_core::encrypt::encrypt_metadata(&folder_key, name)
        .map_err(|e| format!("encrypt folder name: {e}"))?;
    let name_enc = serde_json::to_string(&blob)
        .map_err(|e| format!("serialize name: {e}"))?;
    let result = api
        .create_folder(&name_enc, parent_id, Some(new_id))
        .await?;
    let id_str = result
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("missing folder id")?;
    id_str.parse().map_err(|e| format!("invalid folder id: {e}"))
}

async fn walk_remote(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    folder_id: Uuid,
    prefix: &str,
    files: &mut HashMap<String, RemoteFile>,
    folders: &mut HashMap<String, Uuid>,
) -> Result<(), String> {
    let listing = api.list_files(Some(&folder_id.to_string())).await?;
    let items = listing
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or("invalid file listing")?;

    for item in items {
        let id_str = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or("missing id")?;
        let id: Uuid = id_str.parse().map_err(|e| format!("invalid id: {e}"))?;
        let is_folder = item
            .get("is_folder")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let name_enc = item
            .get("name_encrypted")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let item_key = beebeeb_core::kdf::derive_file_key(master_key, id.as_bytes());
        let name = match serde_json::from_str::<EncryptedBlob>(name_enc)
            .ok()
            .and_then(|b| beebeeb_core::encrypt::decrypt_metadata(&item_key, &b).ok())
        {
            Some(n) => n,
            None => continue,
        };

        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        if is_folder {
            folders.insert(rel.clone(), id);
            Box::pin(walk_remote(api, master_key, id, &rel, files, folders)).await?;
        } else {
            let updated_at_str = item
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let updated_at = DateTime::parse_from_rfc3339(updated_at_str)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let chunk_count = item
                .get("chunk_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(1) as u32;
            files.insert(
                rel,
                RemoteFile {
                    id,
                    chunk_count,
                    updated_at,
                },
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn do_upload(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    local_dir: &Path,
    rel: &str,
    local: &LocalFile,
    replace_remote_id: Option<Uuid>,
    root_remote_id: Uuid,
    remote_folders: &HashMap<String, Uuid>,
    state: &mut SyncState,
    dry_run: bool,
) -> Result<(), String> {
    let size_str = format_size(local.size);
    println!(
        "  {} {} {}",
        "↑".truecolor(245, 184, 0),
        "uploading".truecolor(143, 193, 139),
        format!("{rel} ({size_str})").truecolor(233, 230, 221),
    );

    if dry_run {
        return Ok(());
    }

    if let Some(old_id) = replace_remote_id {
        let _ = api.trash_file(&old_id.to_string()).await;
    }

    let parent_id = match rel.rsplit_once('/') {
        Some((parent_rel, _)) => remote_folders.get(parent_rel).copied(),
        None => Some(root_remote_id),
    }
    .or(Some(root_remote_id));

    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    let local_path = local_dir.join(rel);
    let new_id = upload_file_to(api, master_key, &local_path, file_name, parent_id).await?;

    state.files.insert(
        rel.to_string(),
        FileEntry {
            remote_id: new_id,
            last_mtime: local.mtime,
            last_size: local.size,
            last_sync: Utc::now(),
        },
    );

    Ok(())
}

async fn do_download(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    local_dir: &Path,
    rel: &str,
    remote: &RemoteFile,
    state: &mut SyncState,
    dry_run: bool,
) -> Result<(), String> {
    println!(
        "  {} {} {}",
        "↓".truecolor(143, 193, 139),
        "downloading".truecolor(143, 193, 139),
        rel.truecolor(233, 230, 221),
    );

    if dry_run {
        return Ok(());
    }

    let out_path = local_dir.join(rel);
    download_to(api, master_key, remote.id, remote.chunk_count, &out_path).await?;

    let meta = std::fs::metadata(&out_path)
        .map_err(|e| format!("stat {}: {e}", out_path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    state.files.insert(
        rel.to_string(),
        FileEntry {
            remote_id: remote.id,
            last_mtime: mtime,
            last_size: meta.len(),
            last_sync: Utc::now(),
        },
    );

    Ok(())
}

async fn upload_file_to(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_path: &Path,
    file_name: &str,
    parent_id: Option<Uuid>,
) -> Result<Uuid, String> {
    let file_bytes =
        std::fs::read(file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;

    let file_id = Uuid::new_v4();
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_id.as_bytes());

    let name_blob = beebeeb_core::encrypt::encrypt_metadata(&file_key, file_name)
        .map_err(|e| format!("encrypt name: {e}"))?;
    let name_encrypted =
        serde_json::to_string(&name_blob).map_err(|e| format!("serialize name: {e}"))?;

    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut total_enc: i64 = 0;

    if file_bytes.is_empty() {
        let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, &[])
            .map_err(|e| format!("encrypt chunk: {e}"))?;
        let bytes = serde_json::to_vec(&blob).map_err(|e| format!("serialize chunk: {e}"))?;
        total_enc += bytes.len() as i64;
        chunks.push((0, bytes));
    } else {
        for (i, chunk) in file_bytes.chunks(CHUNK_SIZE).enumerate() {
            let blob = beebeeb_core::encrypt::encrypt_chunk(&file_key, chunk)
                .map_err(|e| format!("encrypt chunk {i}: {e}"))?;
            let bytes = serde_json::to_vec(&blob)
                .map_err(|e| format!("serialize chunk {i}: {e}"))?;
            total_enc += bytes.len() as i64;
            chunks.push((i as u32, bytes));
        }
    }

    let metadata = serde_json::json!({
        "name_encrypted": name_encrypted,
        "parent_id": parent_id,
        "mime_type": guess_mime_type(file_name),
        "size_bytes": total_enc,
    });
    let metadata_json =
        serde_json::to_string(&metadata).map_err(|e| format!("serialize metadata: {e}"))?;

    let result = api.upload_encrypted(&metadata_json, &chunks).await?;
    let id_str = result
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("server response missing file id")?;
    id_str.parse().map_err(|e| format!("invalid file id: {e}"))
}

async fn download_to(
    api: &ApiClient,
    master_key: &beebeeb_core::kdf::MasterKey,
    file_id: Uuid,
    chunk_count: u32,
    out_path: &Path,
) -> Result<(), String> {
    let file_key = beebeeb_core::kdf::derive_file_key(master_key, file_id.as_bytes());
    let encrypted_bytes = api.download_file(&file_id.to_string()).await?;

    let mut plaintext = Vec::new();
    let mut offset = 0;
    for i in 0..chunk_count {
        if offset >= encrypted_bytes.len() {
            return Err(format!("unexpected end of data at chunk {i}"));
        }
        let remaining = &encrypted_bytes[offset..];
        let mut de = serde_json::Deserializer::from_slice(remaining)
            .into_iter::<EncryptedBlob>();
        let blob = match de.next() {
            Some(Ok(b)) => b,
            Some(Err(e)) => return Err(format!("parse chunk {i}: {e}")),
            None => return Err(format!("no data for chunk {i}")),
        };
        offset += de.byte_offset();
        let decrypted = beebeeb_core::encrypt::decrypt_chunk(&file_key, &blob)
            .map_err(|e| format!("decrypt chunk {i}: {e}"))?;
        plaintext.extend_from_slice(&decrypted);
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    std::fs::write(out_path, &plaintext)
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;

    Ok(())
}

fn guess_mime_type(filename: &str) -> Option<String> {
    let ext = filename.rsplit('.').next()?.to_lowercase();
    let mime = match ext.as_str() {
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        "tar" => "application/x-tar",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "md" => "text/markdown",
        "rs" => "text/x-rust",
        "toml" => "application/toml",
        _ => "application/octet-stream",
    };
    Some(mime.to_string())
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
