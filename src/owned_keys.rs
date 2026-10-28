//! Owner key routing. Envelopes stay opaque on disk; every unwrap is core's.
use crate::{api::ApiClient, commands::request::RequestKeyResolver};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use beebeeb_core::{
    kdf::{FileKey, MasterKey},
    owned_keys::{self, FolderWrap, KeyOrigin},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub const UNREADABLE: &str = "can't open (added via shared folder)";
#[derive(Clone)]
pub struct ResolvedCliKey {
    pub key: Arc<FileKey>,
    pub origin: KeyOrigin,
    pub canonical: Value,
    pub pending_owner_wrap: Option<Vec<u8>>,
    #[allow(dead_code)] // Consumed by Task 9 sync/index integration.
    pub shared_root_id: Option<String>,
    pub name: String,
}
struct SharedRoot {
    key: Arc<FileKey>,
    children: HashMap<String, Vec<u8>>,
    legacy_invite: bool,
}
#[derive(Default)]
pub struct OwnedKeyResolver {
    rows: HashMap<String, Value>,
    roots: HashMap<String, SharedRoot>,
    absent_roots: HashSet<String>,
    pending: HashMap<String, Vec<u8>>,
    requests: Option<RequestKeyResolver>,
    sent_invites: Option<Vec<Value>>,
    resolved: HashMap<String,ResolvedCliKey>,
}
impl OwnedKeyResolver {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn seed(&mut self, rows: &[Value]) {
        for row in rows {
            if let Some(id) = row["id"].as_str() {
                self.rows.insert(id.into(), row.clone());
            }
        }
    }
    #[allow(dead_code)] // Consumed by task 9's account/root-bound manifest.
    pub fn restore_pending(&mut self, file_id: &str, owner_wrap: Vec<u8>) -> Result<(), String> {
        uuid::Uuid::parse_str(file_id).map_err(|_| "invalid pending item id")?;
        if owner_wrap.len() != 60 {
            return Err("invalid pending owner envelope".into());
        }
        self.pending.insert(file_id.into(), owner_wrap);
        Ok(())
    }
    pub fn resolved(&self) -> Vec<ResolvedCliKey> { self.resolved.values().cloned().collect() }
    pub fn pending(&self,id:&str) -> Option<Vec<u8>> {self.pending.get(id).cloned()}
    pub async fn folder_key(&mut self,api:&ApiClient,master:&MasterKey,id:&str)->Result<Option<Arc<FileKey>>,String> {
        self.load_root(api,master,id).await?;
        Ok(self.roots.get(id).filter(|r|!r.legacy_invite).map(|r|r.key.clone()))
    }
    pub async fn resolve(&mut self,api:&ApiClient,master:&MasterKey,row:&Value)->Result<ResolvedCliKey,String> {
        let resolved=self.resolve_inner(api,master,row).await?;
        self.resolved.insert(resolved.canonical["id"].as_str().ok_or("missing id")?.into(),resolved.clone());
        Ok(resolved)
    }
    async fn resolve_inner(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        row: &Value,
    ) -> Result<ResolvedCliKey, String> {
        let id = row["id"].as_str().ok_or("missing item id")?.to_string();
        uuid::Uuid::parse_str(&id).map_err(|_| "invalid item id")?;
        let mut canonical = row.clone();
        // Old servers lack C0 attribution. Enrich only their unresolved rows;
        // ordinary legacy rows remain readable on servers without the new routes.
        let sparse = ["owner_wrapped_file_key", "acted_via_share_id", "uploaded_by"]
            .iter()
            .any(|k| row.get(k).is_none());
        if sparse && crate::crypto::decrypt_name(master, &id, text(row, "name_encrypted")?).is_none() {
            canonical = api.get_file(&id).await?;
            if canonical["id"].as_str() != Some(&id) {
                return Err("canonical item identity changed".into());
            }
        }
        self.rows.insert(id.clone(), canonical.clone());
        let foreign = canonical["acted_via_share_id"].as_str().is_some() || canonical["uploaded_by"].as_str().is_some();
        let owner = normalize_wrap(&canonical["owner_wrapped_file_key"]);
        let has_owner = !canonical["owner_wrapped_file_key"].is_null();
        if let Ok(Some(ref frame)) = owner {
            if let Ok(key) = owned_keys::unwrap_owner_key(master, &id, frame) {
                if let Ok(name) = authenticated_name(&key, &canonical) {
                    self.pending.remove(&id);
                    return Ok(ResolvedCliKey {
                        key: Arc::new(key),
                        origin: KeyOrigin::OwnerWrap,
                        canonical,
                        pending_owner_wrap: None,
                        shared_root_id: None,
                        name,
                    });
                }
            }
        }
        // A pending key is never confirmation that adoption committed. Authenticate
        // it against the current name, retain its envelope, then retry in sync.
        if let Some(frame) = self.pending.get(&id) {
            if let Ok(key) = owned_keys::unwrap_owner_key(master, &id, frame) {
                if let Ok(name) = authenticated_name(&key, &canonical) {
                    return Ok(ResolvedCliKey {
                        key: Arc::new(key),
                        origin: KeyOrigin::OwnerWrap,
                        canonical,
                        pending_owner_wrap: Some(frame.clone()),
                        shared_root_id: None,
                        name,
                    });
                }
            }
        }
        if crate::commands::request::is_request_upload(&canonical) && !has_owner {
            if self.requests.is_none() {
                self.requests = Some(RequestKeyResolver::load(api).await?);
            }
            let key = self
                .requests
                .as_mut()
                .and_then(|r| r.content_key(master, &canonical))
                .ok_or("can't open request upload")?;
            let name = authenticated_name(&key, &canonical)?;
            return Ok(ResolvedCliKey {
                key: Arc::new(key),
                origin: KeyOrigin::FolderWrap,
                canonical,
                pending_owner_wrap: None,
                shared_root_id: None,
                name,
            });
        }
        if !foreign && !has_owner {
            let name = crate::crypto::decrypt_name(master, &id, text(&canonical, "name_encrypted")?)
                .ok_or("can't decrypt item name")?;
            let mut resolved =
                owned_keys::resolve_owned_key(master, &id, None, None, false).map_err(|_| "can't resolve item key")?;
            // Preserve the pre-string-UUID key convention when that is the key
            // which authenticated this ordinary legacy item's encrypted name.
            let enc = text(&canonical, "name_encrypted")?;
            if enc.starts_with('{') && authenticated_name(&resolved.key, &canonical).is_err() {
                let binary = beebeeb_core::kdf::derive_file_key(master, uuid::Uuid::parse_str(&id).unwrap().as_bytes());
                authenticated_name(&binary, &canonical)?;
                resolved.key = binary;
            }
            return Ok(ResolvedCliKey {
                key: Arc::new(resolved.key),
                origin: resolved.origin,
                canonical,
                pending_owner_wrap: None,
                shared_root_id: None,
                name,
            });
        }
        let mut parent = canonical["parent_id"].as_str().map(str::to_string);
        let mut visited = HashSet::new();
        while let Some(root_id) = parent {
            if !visited.insert(root_id.clone()) || visited.len() > 256 {
                return Err("shared-folder ancestry cycle".into());
            }
            self.load_root(api, master, &root_id).await?;
            if let Some(root) = self.roots.get(&root_id) {
                let frame = root.children.get(&id).ok_or(UNREADABLE)?;
                let kind = if canonical["is_folder"].as_bool() == Some(true) {
                    2
                } else {
                    1
                };
                let resolved = if root.legacy_invite {
                    owned_keys::ResolvedOwnedKey { key: legacy_invite_child_key(&root.key,frame)?, origin: KeyOrigin::FolderWrap }
                } else { owned_keys::resolve_owned_key(
                    master,
                    &id,
                    owner.as_ref().ok().and_then(|x| x.as_deref()),
                    Some(FolderWrap {
                        shared_folder_id: &root_id,
                        kind,
                        folder_key: &root.key,
                        wrapped_key: frame,
                    }),
                    true,
                )
                .map_err(|_| UNREADABLE)? };
                let name = authenticated_name(&resolved.key, &canonical)?;
                let pending = owned_keys::wrap_key_to_owner(master, &id, &resolved.key)
                    .map_err(|_| "can't prepare owner envelope")?;
                self.pending.insert(id.clone(), pending.clone());
                return Ok(ResolvedCliKey {
                    key: Arc::new(resolved.key),
                    origin: resolved.origin,
                    canonical,
                    pending_owner_wrap: Some(pending),
                    shared_root_id: Some(root_id),
                    name,
                });
            }
            let ancestor = if let Some(r) = self.rows.get(&root_id) {
                r.clone()
            } else {
                let r = api.get_file(&root_id).await?;
                if r["id"].as_str() != Some(&root_id) {
                    return Err("ancestor identity changed".into());
                }
                self.rows.insert(root_id.clone(), r.clone());
                r
            };
            parent = ancestor["parent_id"].as_str().map(str::to_string);
        }
        Err(UNREADABLE.into())
    }
    async fn load_root(&mut self, api: &ApiClient, master: &MasterKey, id: &str) -> Result<(), String> {
        if self.roots.contains_key(id) || self.absent_roots.contains(id) {
            return Ok(());
        }
        let custody = api.get_shared_folder(id).await?;
        if custody.is_null() {
            // An approved legacy invite permits its sender to read the existing
            // inventory without registering custody (reads must not mutate).
            if self.sent_invites.is_none() {
                let sent = api.get_json("/api/v1/shares/invites/sent").await?;
                self.sent_invites = Some(sent["invites"].as_array().cloned().unwrap_or_default());
            }
            if let Some(invite) = self.sent_invites.as_ref().unwrap().iter().find(|r| {
                r["file_id"].as_str() == Some(id)
                    && r["is_folder_share"].as_bool() == Some(true)
                    && r["status"].as_str() == Some("approved")
            }) {
                let frame = normalize_wrap(&invite["encrypted_owner_folder_key"])?.ok_or("missing invite seed")?;
                // L6 explicitly permits the named legacy invite reader only for
                // this seed. Permanent custody never reaches this branch.
                let key = owned_keys::unwrap_owner_key(master, id, &frame).map_err(|_| "invalid invite seed")?;
                let invite_id = text(invite, "id")?;
                let page = api
                    .get_json(&format!("/api/v1/shares/invites/{invite_id}/folder-keys"))
                    .await?;
                let mut children = HashMap::new();
                for row in page["keys"].as_array().ok_or("missing invite inventory")? {
                    if let (Some(id), Ok(Some(frame))) =
                        (row["file_id"].as_str(), normalize_wrap(&row["encrypted_file_key"]))
                    {
                        children.insert(id.into(), frame);
                    }
                }
                self.roots.insert(
                    id.into(),
                    SharedRoot {
                        key: Arc::new(key),
                        children,
                        legacy_invite: true,
                    },
                );
            } else {
                self.absent_roots.insert(id.into());
            }
            return Ok(());
        }
        if custody["folder_id"].as_str() != Some(id) || custody["role"].as_str() != Some("owner") {
            return Err("not owner custody".into());
        }
        let frame = normalize_wrap(&custody["owner_wrapped_folder_key"])?.ok_or("missing owner custody")?;
        let key = owned_keys::unwrap_folder_custody(master, id, &frame).map_err(|_| "invalid L6 owner custody")?;
        let mut children = HashMap::new();
        let mut cursor = None;
        let mut cursors = HashSet::new();
        loop {
            let page = api.get_shared_folder_keys(id, cursor.as_deref()).await?;
            let rows = page["keys"].as_array().ok_or("missing folder key inventory")?;
            for row in rows {
                if let (Some(id), Ok(Some(frame))) =
                    (row["file_id"].as_str(), normalize_wrap(&row["encrypted_file_key"]))
                {
                    children.insert(id.into(), frame);
                }
            }
            match page["next_cursor"].as_str() {
                Some(next) => {
                    if !cursors.insert(next.to_string()) {
                        return Err("folder key cursor cycle".into());
                    }
                    cursor = Some(next.to_string());
                }
                None => break,
            }
        }
        self.roots.insert(
            id.into(),
            SharedRoot {
                key: Arc::new(key),
                children,
                legacy_invite: false,
            },
        );
        Ok(())
    }
    #[allow(dead_code)] // Task 9 is the only mutating caller.
    pub async fn adopt(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        resolved: &ResolvedCliKey,
    ) -> Result<bool, String> {
        let id = text(&resolved.canonical, "id")?;
        let Some(frame) = resolved.pending_owner_wrap.as_ref() else {
            return Ok(true);
        };
        self.pending.insert(id.into(), frame.clone());
        // Regardless of adopted=false or a lost response, canonical readback is
        // the only confirmation. A different valid key is not our winner.
        let posted = api.adopt_owner_key(id, &B64.encode(frame)).await;
        let winner = api.get_file(id).await?;
        let confirmed = (|| {
            if winner["id"].as_str() != Some(id) {
                return Err("adoption identity changed".to_string());
            }
            let wrapped = normalize_wrap(&winner["owner_wrapped_file_key"])?.ok_or("adoption pending")?;
            let key = owned_keys::unwrap_owner_key(master, id, &wrapped).map_err(|_| "invalid adoption winner")?;
            authenticated_name(&key, &winner)?;
            if key.as_bytes() != resolved.key.as_bytes() {
                return Err("adoption winner key differs".into());
            }
            Ok(())
        })();
        match confirmed {
            Ok(()) => {
                self.pending.remove(id);
                self.rows.insert(id.into(),winner.clone());
                if let Some(cached)=self.resolved.get_mut(id){cached.canonical=winner;cached.pending_owner_wrap=None;}
                Ok(true)
            }
            Err(e) => {
                posted?;
                Err(e)
            }
        }
    }
}
fn text<'a>(row: &'a Value, key: &str) -> Result<&'a str, String> {
    row[key].as_str().ok_or_else(|| format!("missing {key}"))
}
fn authenticated_name(key: &FileKey, row: &Value) -> Result<String, String> {
    let enc = text(row, "name_encrypted")?;
    if !enc.starts_with('{') {
        return Err(UNREADABLE.into());
    }
    beebeeb_core::encrypt::decrypt_name_metadata_with_key(key, enc)
        .map(|m| m.name)
        .map_err(|_| UNREADABLE.into())
}
pub fn normalize_wrap(v: &Value) -> Result<Option<Vec<u8>>, String> {
    let bytes = match v {
        Value::Null => return Ok(None),
        Value::String(s) => B64.decode(s).map_err(|_| "invalid base64 envelope")?,
        Value::Array(xs) => xs
            .iter()
            .map(|x| {
                x.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or("invalid envelope byte")
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("invalid envelope shape".into()),
    };
    if bytes.len() != 60 {
        return Err("invalid envelope length".into());
    }
    Ok(Some(bytes))
}

/// L7b flag-off invite reader. Reached only after permanent custody was absent;
/// custody-backed roots always use core's bound reader and never fall back here.
fn legacy_invite_child_key(folder_key: &FileKey, frame: &[u8]) -> Result<FileKey,String> {
    let bytes=zeroize::Zeroizing::new(beebeeb_core::encrypt::decrypt_chunk_raw(folder_key,frame).map_err(|_| "invalid legacy invite child envelope")?);
    let key: [u8;32]=bytes.as_slice().try_into().map_err(|_| "invalid legacy invite child key length")?;
    Ok(FileKey::from_bytes(key))
}
