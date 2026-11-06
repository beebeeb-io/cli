//! The server side of `bb backup`: the Backups root, this machine's device
//! folder and its manifest, a category folder, and one category's tree.
//!
//! Every call goes through the backup client (`ApiClient::for_backup`), so it
//! carries `X-Beebeeb-Origin: backup`. Names are sealed and opened here; the
//! server sees ids only.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use beebeeb_core::backup::{
    DeviceFolderCandidate, DeviceFolderChoice, DeviceManifest, DeviceSelf, FolderManifest, FolderState, MANIFEST_NAME,
    MANIFEST_VERSION, ROOT_NAME, RemoteEntry, RootCandidate, RootPlan, find_device_folder, parse_manifest,
    resolve_root, serialize_manifest,
};
use beebeeb_core::kdf::MasterKey;
use beebeeb_types::backup::SystemRole;
use serde_json::Value;
use uuid::Uuid;

use crate::api::ApiClient;
use crate::upload::ChunkProgress;

fn rows(listing: &Value) -> Vec<Value> {
    listing
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn id_of(row: &Value) -> Option<Uuid> {
    row.get("id")
        .and_then(Value::as_str)
        .and_then(|t| Uuid::parse_str(t).ok())
}

fn is_folder(row: &Value) -> bool {
    row.get("is_folder").and_then(Value::as_bool).unwrap_or(false)
}

fn is_trashed(row: &Value) -> bool {
    row.get("is_trashed").and_then(Value::as_bool).unwrap_or(false)
}

fn time_ms(row: &Value, field: &str) -> i64 {
    row.get(field)
        .and_then(Value::as_str)
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map_or(0, |t| t.timestamp_millis())
}

/// A row's name as this account opens it (`None`: it cannot be read now).
async fn name_of(api: &ApiClient, master_key: &MasterKey, row: &Value) -> Option<String> {
    crate::owned_keys::OwnedKeyResolver::new()
        .resolve(api, master_key, row)
        .await
        .ok()
        .map(|r| r.name)
}

/// The account's Backups root: the flagged folder, else core's survivor of the
/// root-level "Backups" folders claimed, else a new one created, all through
/// `PUT /backups/root` (spec §5.2). Duplicate roots are never merged here:
/// the CLI only claims core's survivor, and moves nothing of the person's.
pub(crate) async fn backups_root(api: &ApiClient, master_key: &MasterKey) -> Result<Uuid, String> {
    let listing = api.list_files(None).await?;
    let mut candidates = Vec::new();
    for row in rows(&listing).iter().filter(|r| is_folder(r)) {
        let Some(id) = id_of(row) else { continue };
        let system_role = row
            .get("system_role")
            .and_then(Value::as_str)
            .and_then(SystemRole::parse);
        // Only a name that could be the root's is opened.
        let name = if system_role.is_some() {
            None
        } else {
            name_of(api, master_key, row).await
        };
        candidates.push(RootCandidate {
            id,
            name,
            is_trashed: is_trashed(row),
            created_at_ms: time_ms(row, "created_at"),
            system_role,
        });
    }
    let answer = match resolve_root(&candidates) {
        RootPlan::Use(id) => return Ok(id),
        RootPlan::Claim { id, .. } => api.put_backups_root(id, None).await?,
        RootPlan::Create => {
            let minted = crate::item_id::mint_owner(master_key)?;
            let sealed = minted.seal_name(
                master_key,
                ROOT_NAME,
                None,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            )?;
            let answer = api.put_backups_root(minted.id(), Some(&sealed)).await?;
            // L13: a root this client created carries the id it minted.
            if answer.get("outcome").and_then(Value::as_str) == Some("created") {
                crate::item_id::verify_minted_echo(minted.id(), crate::item_id::echoed_id(&answer))
                    .map_err(|e| e.to_string())?;
            }
            answer
        }
    };
    answer
        .get("id")
        .and_then(Value::as_str)
        .and_then(|t| Uuid::parse_str(t).ok())
        .ok_or_else(|| "the server did not name the Backups folder".to_string())
}

/// A folder's live state: `Some(true)` live in `parent`, `Some(false)` trashed,
/// gone or moved elsewhere.
async fn live_in(api: &ApiClient, id: Uuid, parent: Uuid) -> Result<bool, String> {
    match api.get_file(id.to_string()).await {
        Ok(row) => Ok(!is_trashed(&row)
            && row
                .get("parent_id")
                .and_then(Value::as_str)
                .and_then(|t| Uuid::parse_str(t).ok())
                == Some(parent)),
        Err(e) if e.status == 404 => Ok(false),
        Err(e) => Err(e.message),
    }
}

/// Where this machine backs up into.
pub(crate) enum DeviceFolder {
    Live(Uuid),
    /// The folder this machine used was deleted elsewhere (spec §4.2 rule 11).
    Deleted,
}

/// This machine's device folder under `root`: the one it used before while it
/// is live, else core's choice among the folders there (found by the manifest's
/// device id), made (with its manifest) when none is this machine's.
pub(crate) async fn device_folder(
    api: &ApiClient,
    master_key: &Arc<MasterKey>,
    root: Uuid,
    me: &DeviceSelf,
    known: Option<Uuid>,
) -> Result<(DeviceFolder, bool), String> {
    if let Some(id) = known {
        let live = live_in(api, id, root).await?;
        return Ok((
            if live {
                DeviceFolder::Live(id)
            } else {
                DeviceFolder::Deleted
            },
            false,
        ));
    }
    let mut candidates = Vec::new();
    for row in rows(&api.list_files(Some(&root.to_string())).await?) {
        let Some(folder_id) = id_of(&row).filter(|_| is_folder(&row)) else {
            continue;
        };
        let Some(name) = name_of(api, master_key, &row).await else {
            continue;
        };
        let manifest = read_manifest(api, master_key, folder_id).await;
        candidates.push(DeviceFolderCandidate {
            folder_id,
            name,
            manifest,
        });
    }
    match find_device_folder(me, &candidates) {
        DeviceFolderChoice::Existing(id) => Ok((DeviceFolder::Live(id), false)),
        // The manifest is written at once by the caller (`fresh` = true).
        DeviceFolderChoice::AdoptLegacy(id) => Ok((DeviceFolder::Live(id), true)),
        DeviceFolderChoice::Create { name } => {
            let id = crate::item_id::create_owned_folder(api, master_key, &name, Some(root)).await?;
            Ok((DeviceFolder::Live(id), true))
        }
    }
}

/// The `.device.json` row of a device folder, if it has one.
async fn manifest_row(api: &ApiClient, master_key: &MasterKey, device: Uuid) -> Result<Option<Value>, String> {
    for row in rows(&api.list_files(Some(&device.to_string())).await?) {
        if !is_folder(&row) && name_of(api, master_key, &row).await.as_deref() == Some(MANIFEST_NAME) {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

/// A device folder's manifest, decrypted and parsed. Any failure to read it is
/// `Unreadable` (never mine, never adopted), not an error of the whole choice.
async fn read_manifest(api: &ApiClient, master_key: &Arc<MasterKey>, device: Uuid) -> FolderManifest {
    let row = match manifest_row(api, master_key, device).await {
        Ok(Some(row)) => row,
        Ok(None) => return FolderManifest::Absent,
        Err(_) => return FolderManifest::Unreadable,
    };
    let Some(id) = id_of(&row) else {
        return FolderManifest::Unreadable;
    };
    let name_encrypted = row.get("name_encrypted").and_then(Value::as_str).unwrap_or_default();
    let Ok(pin) = crate::crypto::content_pin(master_key, &id.to_string(), name_encrypted) else {
        return FolderManifest::Unreadable;
    };
    let chunk_count = row.get("chunk_count").and_then(Value::as_u64).unwrap_or(1) as u32;
    let Ok(dir) = private_temp_dir() else {
        return FolderManifest::Unreadable;
    };
    let out = dir.join("manifest.json");
    let read = async {
        crate::download::stream_download_decrypt(
            api,
            master_key,
            &id.to_string(),
            pin,
            chunk_count,
            &out,
            &crate::upload::NoopProgress,
            MANIFEST_NAME,
        )
        .await
        .map_err(|e| e.to_string())?;
        std::fs::read_to_string(&out).map_err(|e| e.to_string())
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    match read.ok().and_then(|text| parse_manifest(&text).ok()) {
        Some(m) => FolderManifest::Parsed(m),
        None => FolderManifest::Unreadable,
    }
}

/// A new folder in the temp dir that only this user can open (0700 on Unix): a manifest in it is plaintext
/// (the device name and the custom folder names).
fn private_temp_dir() -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("bb-backup-manifest-{}", Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&dir)?;
    Ok(dir)
}

/// Write this machine's manifest into its device folder: a new file the first
/// time, a new version after.
pub(crate) async fn write_manifest(
    api: &ApiClient,
    master_key: &Arc<MasterKey>,
    device: Uuid,
    me: &DeviceSelf,
    folders: Vec<FolderState>,
) -> Result<(), String> {
    let manifest = DeviceManifest {
        version: MANIFEST_VERSION,
        device_id: Some(me.device_id.clone()),
        device_name: me.name.clone(),
        platform: Some(me.platform),
        folders,
        other: serde_json::Map::new(),
    };
    let existing = manifest_row(api, master_key, device).await?.and_then(|r| id_of(&r));
    let dir = private_temp_dir().map_err(|e| format!("cannot write the device manifest: {e}"))?;
    let path = dir.join(MANIFEST_NAME);
    let result = async {
        std::fs::write(&path, serialize_manifest(&manifest))
            .map_err(|e| format!("cannot write the device manifest: {e}"))?;
        upload(
            api,
            master_key,
            &path,
            MANIFEST_NAME,
            device,
            existing,
            &crate::upload::NoopProgress,
        )
        .await
        .map(|_| ())
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The category folder `name` in the device folder: the one used before while
/// it is live (`Some(true)`), `Some(false)` when it was deleted elsewhere, and
/// on a first backup the folder of that name there, else a new one.
pub(crate) async fn category_folder(
    api: &ApiClient,
    master_key: &MasterKey,
    device: Uuid,
    name: &str,
    known: Option<Uuid>,
) -> Result<(Option<bool>, Option<Uuid>), String> {
    if let Some(id) = known {
        let live = live_in(api, id, device).await?;
        return Ok((Some(live), live.then_some(id)));
    }
    let id = ensure_folder(api, master_key, device, name).await?;
    Ok((None, Some(id)))
}

/// The folder `name` in `parent`: the existing one (exact name), else a new
/// one. Idempotent.
pub(crate) async fn ensure_folder(
    api: &ApiClient,
    master_key: &MasterKey,
    parent: Uuid,
    name: &str,
) -> Result<Uuid, String> {
    for row in rows(&api.list_files(Some(&parent.to_string())).await?) {
        if is_folder(&row)
            && !is_trashed(&row)
            && let Some(id) = id_of(&row)
            && name_of(api, master_key, &row).await.as_deref() == Some(name)
        {
            return Ok(id);
        }
    }
    crate::item_id::create_owned_folder(api, master_key, name, Some(parent)).await
}

/// One category's tree on the server: folders by key (`""` = the category
/// folder), live files with their keys, and whether every listing was whole.
pub(crate) struct Tree {
    pub folders: HashMap<String, Uuid>,
    pub files: Vec<RemoteEntry>,
    pub complete: bool,
}

/// List the category folder `top` and everything under it. A file whose name
/// cannot be read now is listed with an empty key: it is live by its id.
pub(crate) async fn tree(api: &ApiClient, master_key: &MasterKey, top: Uuid) -> Result<Tree, String> {
    let mut tree = Tree {
        folders: HashMap::from([(String::new(), top)]),
        files: Vec::new(),
        complete: true,
    };
    let mut stack = vec![(top, String::new())];
    while let Some((folder, prefix)) = stack.pop() {
        let listing = api.list_files(Some(&folder.to_string())).await?;
        if listing.get("truncated").and_then(Value::as_bool) == Some(true) {
            tree.complete = false;
        }
        for row in rows(&listing) {
            let Some(id) = id_of(&row) else {
                tree.complete = false;
                continue;
            };
            if is_trashed(&row) {
                continue;
            }
            let name = name_of(api, master_key, &row).await;
            let key = name.map(|n| if prefix.is_empty() { n } else { format!("{prefix}/{n}") });
            if is_folder(&row) {
                match key {
                    Some(key) => {
                        tree.folders.insert(key.clone(), id);
                        stack.push((id, key));
                    }
                    // A folder that cannot be read: what is under it is not known.
                    None => tree.complete = false,
                }
            } else {
                tree.files.push(RemoteEntry {
                    key: key.unwrap_or_default(),
                    remote_id: id,
                    size: row.get("size_bytes").and_then(Value::as_u64).unwrap_or(0),
                    uploaded_at_ms: time_ms(&row, "uploaded_at"),
                });
            }
        }
    }
    Ok(tree)
}

/// What an upload ended as.
pub(crate) enum Uploaded {
    /// The server id of the file (the same id for a new version).
    Done { id: Uuid },
    /// A new version was asked for a file the server no longer has.
    Gone,
}

/// Upload `path` (read in place) as `name` into `parent`: a new file, or with
/// `replace` a new version of that file.
pub(crate) async fn upload(
    api: &ApiClient,
    master_key: &Arc<MasterKey>,
    path: &Path,
    name: &str,
    parent: Uuid,
    replace: Option<Uuid>,
    progress: &dyn ChunkProgress,
) -> Result<Uploaded, String> {
    let (file_id, resume_tag, base, pin, key) = match replace {
        None => {
            let minted = crate::item_id::mint_owner(master_key)?;
            (minted.id(), Some(*minted.tag()), None, None, None)
        }
        Some(id) => {
            let meta = match api.get_file(id.to_string()).await {
                Ok(meta) if !is_trashed(&meta) => meta,
                Ok(_) => return Ok(Uploaded::Gone),
                Err(e) if e.status == 404 => return Ok(Uploaded::Gone),
                Err(e) => return Err(e.message),
            };
            let version = crate::api::current_version_number(&meta).unwrap_or(1) as i32;
            let name_encrypted = meta.get("name_encrypted").and_then(Value::as_str).unwrap_or_default();
            let resolved = crate::owned_keys::OwnedKeyResolver::new()
                .resolve(api, master_key, &meta)
                .await?;
            let pin = match &resolved.write_target {
                Some(target) => target.pin(),
                None => crate::crypto::content_pin_with_key(&resolved.key, &id.to_string(), name_encrypted)?,
            };
            (id, None, Some(version), Some(pin), Some(resolved.key))
        }
    };
    let spec = crate::upload::UploadSpec {
        path: path.to_path_buf(),
        file_name: name.to_string(),
        file_id,
        resume_tag,
        base_version_number: base,
        replace_pin: pin,
        replace_file_key: key,
        parent_id: Some(parent),
        concurrency: 1,
        shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        read_row: None,
    };
    let outcome = crate::upload::stream_encrypt_upload(api, Arc::clone(master_key), spec, progress)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Uploaded::Done { id: outcome.server_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_plaintext_manifest_folder_is_this_users_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_temp_dir().unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir(&dir).unwrap();
        assert_eq!(mode, 0o700, "{mode:o}");
    }
}
