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
pub const CUSTODY_UNAVAILABLE: &str = "This folder's keys are unavailable";
#[derive(Clone)]
pub struct ResolvedCliKey {
    pub key: Arc<FileKey>,
    pub origin: KeyOrigin,
    pub canonical: Value,
    pub pending_owner_wrap: Option<Vec<u8>>,
    #[allow(dead_code)] // Consumed by Task 9 sync/index integration.
    pub shared_root_id: Option<String>,
    pub name: String,
    /// SF-0002: the name resolved under the read-only uppercase repair
    /// convention. No rename, no new version; an edit becomes a new file.
    pub read_only: bool,
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
    resolved: HashMap<String, ResolvedCliKey>,
    custody: Option<crate::custody::CustodyStore>,
}
impl OwnedKeyResolver {
    pub fn new() -> Self {
        Self::default()
    }
    // L7d: the device's custody memory, read fresh on every question (another process may have added to it).
    fn custody(&mut self) -> Result<&crate::custody::CustodyStore, String> {
        if self.custody.is_none() {
            self.custody = Some(crate::custody::CustodyStore::open().map_err(|_| CUSTODY_UNAVAILABLE)?);
        }
        Ok(self.custody.as_ref().unwrap())
    }
    fn seen(&mut self, id: &str) -> Result<bool, String> {
        self.custody()?.seen(id).map_err(|_| CUSTODY_UNAVAILABLE.into())
    }
    fn remember(&mut self, id: &str) -> Result<(), String> {
        self.custody()?.remember(id).map_err(|_| CUSTODY_UNAVAILABLE.into())
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
    pub fn resolved(&self) -> Vec<ResolvedCliKey> {
        self.resolved.values().cloned().collect()
    }
    pub fn pending(&self, id: &str) -> Option<Vec<u8>> {
        self.pending.get(id).cloned()
    }
    pub fn descendant_ids(&self, root: &str) -> HashSet<String> {
        self.rows
            .iter()
            .filter(|(_, row)| {
                let mut parent = row["parent_id"].as_str();
                let mut seen = HashSet::new();
                while let Some(id) = parent {
                    if id == root {
                        return true;
                    }
                    if !seen.insert(id) || seen.len() > 256 {
                        break;
                    }
                    parent = self.rows.get(id).and_then(|r| r["parent_id"].as_str());
                }
                false
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
    pub async fn folder_key(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        id: &str,
    ) -> Result<Option<Arc<FileKey>>, String> {
        let seen = self.seen(id)?;
        if let Some(root) = self.roots.get(id) {
            if seen && root.legacy_invite {
                return Err(CUSTODY_UNAVAILABLE.into());
            }
            return Ok((!root.legacy_invite).then(|| root.key.clone()));
        }
        let custody = api.get_shared_folder(id).await?;
        if custody.is_null() {
            if seen {
                return Err(CUSTODY_UNAVAILABLE.into());
            }
            return Ok(None);
        }
        if custody["folder_id"].as_str() != Some(id) || custody["role"].as_str() != Some("owner") {
            return Err("not owner custody".into());
        }
        let frame = normalize_wrap(&custody["owner_wrapped_folder_key"])?.ok_or("missing owner custody")?;
        let key = owned_keys::unwrap_folder_custody(master, id, &frame).map_err(|_| "invalid L6 owner custody")?;
        self.remember(id)?;
        Ok(Some(Arc::new(key)))
    }

    pub async fn resolve(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        row: &Value,
    ) -> Result<ResolvedCliKey, String> {
        let resolved = self.resolve_inner(api, master, row).await?;
        self.resolved.insert(
            resolved.canonical["id"].as_str().ok_or("missing id")?.into(),
            resolved.clone(),
        );
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
            if let Ok(key) = owned_keys::unwrap_owner_file_key(master, &id, frame) {
                if let Ok(name) = authenticated_name(&key, &canonical) {
                    self.pending.remove(&id);
                    return Ok(ResolvedCliKey {
                        key: Arc::new(key),
                        origin: KeyOrigin::OwnerWrap,
                        canonical,
                        pending_owner_wrap: None,
                        shared_root_id: None,
                        name,
                        read_only: false,
                    });
                }
            }
        }
        // A pending key is never confirmation that adoption committed. Authenticate
        // it against the current name, retain its envelope, then retry in sync.
        if let Some(frame) = self.pending.get(&id) {
            if let Ok(key) = owned_keys::unwrap_owner_file_key(master, &id, frame) {
                if let Ok(name) = authenticated_name(&key, &canonical) {
                    return Ok(ResolvedCliKey {
                        key: Arc::new(key),
                        origin: KeyOrigin::OwnerWrap,
                        canonical,
                        pending_owner_wrap: Some(frame.clone()),
                        shared_root_id: None,
                        name,
                        read_only: false,
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
                read_only: false,
            });
        }
        if !foreign && !has_owner {
            let enc = text(&canonical, "name_encrypted")?;
            let (name, read_only) = match crate::crypto::decrypt_name_with_convention(master, &id, enc) {
                Some((name, convention)) => (name, convention == beebeeb_core::kdf::OwnerKeyConvention::UpperRepair),
                None => return Err("can't decrypt item name".into()),
            };
            // SF-0002: the iOS File Provider sealed this row under the
            // uppercase id. Only the owner can derive that key; hand it back
            // read-only so every write path refuses.
            if read_only {
                let (key, _) = beebeeb_core::kdf::owner_file_key_for_name(master, &id, enc)
                    .map_err(|_| "can't resolve item key")?;
                return Ok(ResolvedCliKey {
                    key: Arc::new(key),
                    origin: KeyOrigin::Derived,
                    canonical,
                    pending_owner_wrap: None,
                    shared_root_id: None,
                    name,
                    read_only: true,
                });
            }
            let mut resolved =
                owned_keys::resolve_owned_key(master, &id, None, None, false).map_err(|_| "can't resolve item key")?;
            // Preserve the pre-string-UUID key convention when that is the key
            // which authenticated this ordinary legacy item's encrypted name.
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
                read_only: false,
            });
        }
        let mut parent = canonical["parent_id"].as_str().map(str::to_string);
        let mut visited = HashSet::new();
        while let Some(root_id) = parent {
            if !visited.insert(root_id.clone()) || visited.len() > 256 {
                return Err("shared-folder ancestry cycle".into());
            }
            self.load_root(api, master, &root_id).await?;
            if self.roots.get(&root_id).is_some_and(|r| r.legacy_invite) {
                self.refuse_if_custody_above(api, &root_id).await?;
            }
            if let Some(root) = self.roots.get(&root_id) {
                let frame = root.children.get(&id).ok_or(UNREADABLE)?;
                let kind = if canonical["is_folder"].as_bool() == Some(true) {
                    2
                } else {
                    1
                };
                let resolved = if root.legacy_invite {
                    owned_keys::ResolvedOwnedKey {
                        key: legacy_invite_child_key(&root.key, frame)?,
                        origin: KeyOrigin::FolderWrap,
                    }
                } else {
                    owned_keys::resolve_owned_key(
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
                    .map_err(|_| UNREADABLE)?
                };
                let name = authenticated_name(&resolved.key, &canonical)?;
                let pending = owned_keys::wrap_owner_file_key(master, &id, &resolved.key)
                    .map_err(|_| "can't prepare owner envelope")?;
                self.pending.insert(id.clone(), pending.clone());
                return Ok(ResolvedCliKey {
                    key: Arc::new(resolved.key),
                    origin: resolved.origin,
                    canonical,
                    pending_owner_wrap: Some(pending),
                    shared_root_id: Some(root_id),
                    name,
                    read_only: false,
                });
            }
            let ancestor = self.ancestor_row(api, &root_id).await?;
            parent = ancestor["parent_id"].as_str().map(str::to_string);
        }
        Err(UNREADABLE.into())
    }
    async fn ancestor_row(&mut self, api: &ApiClient, id: &str) -> Result<Value, String> {
        if let Some(r) = self.rows.get(id) {
            return Ok(r.clone());
        }
        let r = api.get_file(id).await?;
        if r["id"].as_str() != Some(id) {
            return Err("ancestor identity changed".into());
        }
        self.rows.insert(id.into(), r.clone());
        Ok(r)
    }
    /// The walk above stops at the nearest root. A legacy root is therefore only readable while nothing higher
    /// in the tree was ever opened under custody here: a server that hides or withdraws the custody of an
    /// outer folder must not get its inner folders read through an old invite.
    async fn refuse_if_custody_above(&mut self, api: &ApiClient, start: &str) -> Result<(), String> {
        let mut visited = HashSet::from([start.to_string()]);
        let mut current = start.to_string();
        loop {
            let row = self.ancestor_row(api, &current).await?;
            let Some(parent) = row["parent_id"].as_str().map(str::to_string) else {
                return Ok(());
            };
            if !visited.insert(parent.clone()) || visited.len() > 256 {
                return Err("shared-folder ancestry cycle".into());
            }
            if self.seen(&parent)? {
                return Err(CUSTODY_UNAVAILABLE.into());
            }
            current = parent;
        }
    }
    async fn load_root(&mut self, api: &ApiClient, master: &MasterKey, id: &str) -> Result<(), String> {
        let seen = self.seen(id)?;
        if seen && (self.absent_roots.contains(id) || self.roots.get(id).is_some_and(|r| r.legacy_invite)) {
            return Err(CUSTODY_UNAVAILABLE.into());
        }
        if self.roots.contains_key(id) || self.absent_roots.contains(id) {
            return Ok(());
        }
        let custody = api.get_shared_folder(id).await?;
        if custody.is_null() {
            if seen {
                return Err(CUSTODY_UNAVAILABLE.into());
            }
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
                let invite_id = text(invite, "id")?;
                // The rows come through the APPROVED invite (the server answers 400 for any other). They are the
                // folder's child inventory and also the evidence a legacy owner copy is proven against.
                let page = api
                    .get_json(&format!("/api/v1/shares/invites/{invite_id}/folder-keys"))
                    .await?;
                let mut rows: Vec<(String, Vec<u8>)> = Vec::new();
                let mut children = HashMap::new();
                for row in page["keys"].as_array().ok_or("missing invite inventory")? {
                    if let (Some(id), Ok(Some(frame))) =
                        (row["file_id"].as_str(), normalize_wrap(&row["encrypted_file_key"]))
                    {
                        rows.push((id.into(), frame.clone()));
                        children.insert(id.into(), frame);
                    }
                }
                // L6 permits the named legacy invite reader only here, and only for a copy that is proven.
                // Permanent custody never reaches this branch. No proof is no key: never a mint.
                let key = open_invite_owner_copy(master, id, &frame, &rows).ok_or(CUSTODY_UNAVAILABLE)?;
                // Another process may have opened custody for this folder while the requests above were in
                // flight: the answer "no custody" is stale, so ask the store again before trusting it.
                if self.seen(id)? {
                    return Err(CUSTODY_UNAVAILABLE.into());
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
        self.remember(id)?;
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
            let key = owned_keys::unwrap_owner_file_key(master, id, &wrapped).map_err(|_| "invalid adoption winner")?;
            authenticated_name(&key, &winner)?;
            if key.as_bytes() != resolved.key.as_bytes() {
                return Err("adoption winner key differs".into());
            }
            Ok(())
        })();
        match confirmed {
            Ok(()) => {
                self.pending.remove(id);
                self.rows.insert(id.into(), winner.clone());
                if let Some(cached) = self.resolved.get_mut(id) {
                    cached.canonical = winner;
                    cached.pending_owner_wrap = None;
                }
                Ok(true)
            }
            Err(e) => {
                posted?;
                Err(e)
            }
        }
    }
}
/// SF-0002 round 2: the three answers of the read-only check a write asks
/// before it renames a row or writes a version under its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCheck {
    /// Neither the name nor the current version is sealed under the read-only
    /// uppercase repair convention: the write may go ahead.
    Writable,
    /// The name or the current version resolves as core's `UpperRepair`: the
    /// row is read-only. No rename, no new version, and an edit is not written
    /// over it ([`crate::crypto::OWNER_REPAIR_READ_ONLY`]).
    UpperRepair,
    /// The check could not decide: a download error, a timeout, a 5xx, a short
    /// body. It never proceeds and never marks the row read-only: an interactive
    /// write is refused with [`WRITE_CHECK_UNKNOWN`], sync leaves the row alone
    /// and asks again on its next pass.
    Unknown,
}

/// What a write says when [`WriteCheck::Unknown`] refused it. Retryable: the
/// row is not marked read-only.
pub const WRITE_CHECK_UNKNOWN: &str = "Couldn't check this file before changing it. Try again.";

impl WriteCheck {
    /// The guard form for an interactive write (`bb push --replace`, `bb mv`):
    /// only [`WriteCheck::Writable`] proceeds; the error is the user-facing text.
    pub fn ensure(self) -> Result<(), String> {
        match self {
            Self::Writable => Ok(()),
            Self::UpperRepair => Err(crate::crypto::OWNER_REPAIR_READ_ONLY.to_string()),
            Self::Unknown => Err(WRITE_CHECK_UNKNOWN.to_string()),
        }
    }
}

/// What one row's write check is decided from. Each surface fills it from the
/// listing or metadata that resolved the row, so no second GET is made.
pub struct WriteTarget<'a> {
    pub file_id: &'a str,
    /// The name verdict: [`crate::crypto::name_is_upper_repair`], which is what
    /// the resolver's `read_only` and every listing's flag carry.
    pub name_read_only: bool,
    /// The plaintext size the listing carried, if any.
    pub size_bytes: Option<u64>,
    /// The chunk count the listing carried (`1` when it did not say).
    pub chunk_count: u32,
}

/// SF-0002: the ONE read-only decision every write surface asks first (`bb
/// push --replace`, `bb mv`, WebDAV PUT and MOVE, the mount's rename, `bb
/// sync`). The name decides first; otherwise the CURRENT version's chunk 0 is
/// read (a bounded prefix, never the whole file) and core's per-version owner
/// resolver decides.
pub async fn write_check(api: &ApiClient, master: &MasterKey, target: &WriteTarget<'_>) -> WriteCheck {
    if target.name_read_only {
        return WriteCheck::UpperRepair;
    }
    if target.chunk_count == 0 {
        return WriteCheck::Writable;
    }
    let Ok((body, chunk_size, complete)) =
        crate::download::download_frame_prefix(api, target.file_id, target.size_bytes, target.chunk_count).await
    else {
        return WriteCheck::Unknown;
    };
    first_chunk_check(
        master,
        target.file_id,
        &body,
        target.size_bytes,
        target.chunk_count,
        chunk_size,
        complete,
    )
}

fn first_chunk_check(
    master: &MasterKey,
    file_id: &str,
    body: &[u8],
    size_bytes: Option<u64>,
    chunk_count: u32,
    chunk_size: Option<u64>,
    complete: bool,
) -> WriteCheck {
    for len in chunk0_lengths(body.len(), size_bytes, chunk_count, chunk_size, complete) {
        if let Some((_, convention)) = crate::crypto::owner_content_key(master, file_id, &body[..len]) {
            return if convention == beebeeb_core::kdf::OwnerKeyConvention::UpperRepair {
                WriteCheck::UpperRepair
            } else {
                WriteCheck::Writable
            };
        }
    }
    WriteCheck::Writable
}

/// Candidate lengths for wire frame 0, from the version's geometry in its
/// metadata (the server's plaintext chunk size is authoritative when present).
fn chunk0_lengths(
    have: usize,
    size_bytes: Option<u64>,
    chunk_count: u32,
    chunk_size: Option<u64>,
    complete: bool,
) -> Vec<usize> {
    const NONCE_AND_TAG: u64 = 28;
    const FILE_PROVIDER_CHUNK: u64 = 1024 * 1024;
    let mut out: Vec<usize> = Vec::new();
    let mut push = |v: u64| {
        if let Ok(v) = usize::try_from(v) {
            if v > 0 && v <= have && !out.contains(&v) {
                out.push(v);
            }
        }
    };
    if chunk_count <= 1 {
        if complete {
            push(have as u64);
        } else if let Some(size) = size_bytes {
            push(size.saturating_add(NONCE_AND_TAG));
        }
        return out;
    }
    if let Some(cs) = chunk_size {
        push(cs.saturating_add(NONCE_AND_TAG));
    }
    if let Some(size) = size_bytes {
        let total = size.saturating_add(NONCE_AND_TAG.saturating_mul(chunk_count as u64));
        push(total / chunk_count as u64);
    }
    push(FILE_PROVIDER_CHUNK + NONCE_AND_TAG);
    if complete {
        push(have as u64);
    }
    out
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
fn legacy_invite_child_key(folder_key: &FileKey, frame: &[u8]) -> Result<FileKey, String> {
    let bytes = zeroize::Zeroizing::new(
        beebeeb_core::encrypt::decrypt_chunk_raw(folder_key, frame)
            .map_err(|_| "invalid legacy invite child envelope")?,
    );
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "invalid legacy invite child key length")?;
    Ok(FileKey::from_bytes(key))
}

/// An approved invite's owner copy of the folder key (task 2266). The purpose-bound format first, which needs
/// no proof; otherwise a legacy copy only through core's possession proof over the folder's `rows`, in the
/// order the server sent them (core examines a bounded, deduplicated prefix). `None` means unproven: the caller
/// refuses, and no key is minted. This is never an owner file-key opener or a permanent custody fallback.
fn open_invite_owner_copy(
    master: &MasterKey,
    folder_id: &str,
    frame: &[u8],
    rows: &[(String, Vec<u8>)],
) -> Option<FileKey> {
    let blob = beebeeb_types::EncryptedBlob {
        cipher_suite: beebeeb_types::CipherSuite::V1Aes256Gcm,
        nonce: frame.get(..12)?.to_vec(),
        ciphertext: frame.get(12..)?.to_vec(),
    };
    if let Ok(key) = beebeeb_core::kdf::unwrap_invite_owner_copy(master, folder_id.as_bytes(), &blob) {
        return Some(key);
    }
    beebeeb_core::kdf::open_legacy_invite_owner_copy(
        master,
        folder_id.as_bytes(),
        frame,
        rows.iter().map(|(id, wrap)| (id.as_bytes(), wrap.as_slice())),
    )
}
