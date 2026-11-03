//! Owner key routing. Envelopes stay opaque on disk; every unwrap is core's.
use crate::{api::ApiClient, commands::request::RequestKeyResolver};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use beebeeb_core::{
    kdf::{FileKey, MasterKey},
    owned_keys::{self, KeyOrigin},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

mod adopt;
mod l14;
#[cfg(test)]
#[path = "owned_keys/request_name_tests.rs"]
mod request_name_tests;

pub const UNREADABLE: &str = "can't open (added via shared folder)";
pub const CUSTODY_UNAVAILABLE: &str = "This folder's keys are unavailable";
/// L15 = A: desktop and CLI never publish an owner key, so a write into a shared
/// folder stays owner-only until the owner shares it from web or mobile (A14b).
/// A rename needs the key the row's name authenticates under. A row this
/// device cannot resolve (a guest's folder, an unreadable row) has none, and a
/// derived guess would seal the new name under a key that opens nothing.
/// L12 (P2-10): an O row whose name is plaintext proves nothing about its key.
/// It stays readable, but every surface refuses to change it with this sentence
/// (not the SF-0002 iOS-repair one, which is about a different convention).
pub const PLAINTEXT_NAME_READ_ONLY: &str = "This file's name isn't encrypted, so this device can't verify or change it. You can still open and download it; to change it, save a copy with the web or mobile app.";
/// A request row whose name is not an encrypted envelope cannot resolve as a
/// request row; it falls back to the plaintext-owner path and a rename refuses
/// with [`RENAME_UNKNOWN_KEY`] (2275 review-3 P2-9).
pub const RENAME_UNKNOWN_KEY: &str = "can't rename this item: this device doesn't hold the key its name uses";
/// R4-B: a row that carries request fields while the request list could not be
/// loaded cannot be classified. Treating it as an owner row would let a 5xx
/// turn a renamed request row writable, so it stays read-only and says why.
pub const REQUEST_KEYS_UNAVAILABLE: &str =
    "can't change this item: this device couldn't load your file-request keys. Try again when it can.";
pub const OWNER_ONLY_NOTICE: &str = "Only you can open this. Share it from the web or the app.";

/// L13/A9 (2275): the string-convention owner key for `id` — the key a request
/// row's NEW name is sealed under on a rename, whatever key its CURRENT name
/// resolved under.
///
/// A pre-2236 request row's name is sealed under its content key `C`, which the
/// anonymous uploader holds; re-sealing under it would hand the owner's new name
/// to the uploader and the server. Core's [`OwnerKeyConvention::String`] is
/// exactly `derive_file_key(master, id.as_bytes())`, and `item_class` enforces
/// the canonical lowercase hyphenated id that derivation requires.
///
/// [`OwnerKeyConvention::String`]: beebeeb_core::kdf::OwnerKeyConvention::String
pub fn string_name_key(master: &MasterKey, id: &str) -> Result<Arc<FileKey>, String> {
    match beebeeb_core::item_id::item_class(id) {
        Ok(beebeeb_core::item_id::ItemClass::Owner) => {}
        _ => return Err(RENAME_UNKNOWN_KEY.to_string()),
    }
    Ok(Arc::new(beebeeb_core::kdf::derive_file_key(master, id.as_bytes())))
}

/// Print [`OWNER_ONLY_NOTICE`] when `folder_id` sits in a shared folder.
pub async fn announce_owner_only(api: &ApiClient, master: &MasterKey, folder_id: Option<&str>) {
    let Some(folder_id) = folder_id else {
        return;
    };
    if matches!(
        OwnedKeyResolver::new().shared_root_of(api, master, folder_id).await,
        Ok(Some(_))
    ) {
        println!("{OWNER_ONLY_NOTICE}");
    }
}

#[derive(Clone)]
pub struct ResolvedCliKey {
    pub key: Arc<FileKey>,
    /// L12: the key the row's NAME authenticates under, when it differs from
    /// `key` (a request row: derived-first name key, content under C).
    pub name_key: Option<Arc<FileKey>>,
    /// P2-9: the pin the resolved name carries, so a rename re-seals under the
    /// same convention the row's name authenticated with.
    pub name_pin: Option<beebeeb_core::chunk_stream::ExpectedChunkFormat>,
    pub origin: KeyOrigin,
    pub canonical: Value,
    pub pending_owner_wrap: Option<Vec<u8>>,
    #[allow(dead_code)] // Consumed by Task 9 sync/index integration.
    pub shared_root_id: Option<String>,
    pub name: String,
    /// SF-0002: the name resolved under the read-only uppercase repair
    /// convention. No rename, no new version; an edit becomes a new file.
    pub read_only: bool,
    /// R4-A: where `name_key` came from (a request row's rename convention).
    pub name_origin: Option<KeyOrigin>,
    /// R4-B: why a row is read-only when the reason is not the SF-0002 or
    /// plaintext one (the request key list could not be loaded).
    pub read_only_reason: Option<&'static str>,
    /// L13: the seal target core issued for this row. Present only when the
    /// resolver could authenticate the row for writing; a write must take its
    /// key and chunk-format pin from here, never re-derive them.
    pub write_target: Option<Arc<owned_keys::WriteTarget>>,
}

/// The authenticated name from a core resolution (L12).
fn resolved_name(name: &owned_keys::ResolvedName) -> String {
    match name {
        owned_keys::ResolvedName::Encrypted(meta) => meta.name.clone(),
        owned_keys::ResolvedName::Plaintext(text) => text.clone(),
    }
}
/// L12: the three `derived_*` origins all open from the master key itself, so
/// the legacy master read path (which tries each id convention) and the
/// new-item-on-edit rule still apply to them. Only a wrap or a request key
/// needs the explicit-key path.
pub fn is_derived_origin(origin: KeyOrigin) -> bool {
    matches!(
        origin,
        KeyOrigin::Derived | KeyOrigin::DerivedBinary | KeyOrigin::DerivedUpperRepair
    )
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
    /// R4-B: `GET /file-requests` failed. A row carrying request fields can no
    /// longer be classified, so it must not fall through to the owner path.
    requests_failed: bool,
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
    /// The key `id` resolved to in this resolver, if it was resolved here.
    pub fn key_for(&self, id: &str) -> Option<Arc<FileKey>> {
        self.resolved.get(id).map(|r| r.key.clone())
    }
    /// P2-10: `id`'s stored name is plaintext, not an envelope.
    pub fn name_is_plaintext_for(&self, id: &str) -> bool {
        self.resolved
            .get(id)
            .is_some_and(|r| crate::crypto::name_is_plaintext(&r.canonical))
    }
    /// The key origin `id` resolved to in this resolver, if it was resolved here.
    pub fn origin_for(&self, id: &str) -> Option<KeyOrigin> {
        self.resolved.get(id).map(|r| r.origin)
    }
    /// L13: the seal target core issued for `id`, if the resolver authenticated
    /// the row for writing.
    pub fn write_target_for(&self, id: &str) -> Option<Arc<owned_keys::WriteTarget>> {
        self.resolved.get(id).and_then(|r| r.write_target.clone())
    }
    /// L12 (P2-9): the chunk-format pin recorded in a request row's resolved
    /// name. A rename re-seals under it instead of assuming `Unrecorded`.
    pub fn name_pin_for(&self, id: &str) -> Option<beebeeb_core::chunk_stream::ExpectedChunkFormat> {
        self.resolved.get(id).and_then(|r| r.name_pin)
    }
    /// R4-A: which convention `name_key` came from (a request row only).
    pub fn name_origin_for(&self, id: &str) -> Option<KeyOrigin> {
        self.resolved.get(id).and_then(|r| r.name_origin)
    }
    /// R4-B: the one-line reason a row is read-only when it is not the
    /// SF-0002 or plaintext-name reason.
    pub fn read_only_reason_for(&self, id: &str) -> Option<&'static str> {
        self.resolved.get(id).and_then(|r| r.read_only_reason)
    }
    /// Load (and cache) a shared root's custody and inventory, so the L14 check
    /// has the wrap even when the row itself resolves through an owner envelope.
    pub async fn load_shared_root(&mut self, api: &ApiClient, master: &MasterKey, id: &str) -> Result<(), String> {
        self.load_root(api, master, id).await
    }
    /// L15: the shared root `id` sits in, if any. Walks up until a folder with
    /// owner custody is found.
    ///
    /// Fails CLOSED: an ancestor the server will not answer for is an error, not
    /// "not shared". A caller that would write must refuse rather than treat an
    /// unreadable ancestry as an owner folder (2275 review P2-5).
    pub async fn shared_root_of(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        id: &str,
    ) -> Result<Option<String>, String> {
        let mut current = Some(id.to_string());
        let mut visited = HashSet::new();
        while let Some(folder) = current {
            if !visited.insert(folder.clone()) || visited.len() > 256 {
                return Err("shared-folder ancestry cycle".into());
            }
            match self.load_root(api, master, &folder).await {
                Ok(()) if self.roots.contains_key(&folder) => return Ok(Some(folder)),
                Ok(()) => {}
                // Custody that cannot be read is doubt, never an owner folder.
                Err(e) => return Err(e),
            }
            let row = self.ancestor_row(api, &folder).await?;
            current = row["parent_id"].as_str().map(str::to_string);
        }
        Ok(None)
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
        // L12: the id class decides the key path; attribution never does. A
        // non-canonical id is not an item this client can open.
        let class = beebeeb_core::item_id::item_class(&id).map_err(|_| "invalid item id")?;
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
        let kind = if canonical["is_folder"].as_bool() == Some(true) {
            owned_keys::ItemKind::Folder
        } else {
            owned_keys::ItemKind::File
        };
        match class {
            beebeeb_core::item_id::ItemClass::Owner => self.resolve_owner_row(api, master, canonical, &id, kind).await,
            beebeeb_core::item_id::ItemClass::Foreign => {
                self.resolve_foreign_row(api, master, canonical, &id, kind).await
            }
        }
    }
    /// L12: an owner item's key is derived from its id. A served L8 envelope, a
    /// pending one this device prepared, or an L7 child wrap is ignored for an
    /// owner id — core refuses to open an owner envelope for one, because it
    /// would be a second, competing key. Upper-repair rows stay read-only.
    async fn resolve_owner_row(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        canonical: Value,
        id: &str,
        kind: owned_keys::ItemKind,
    ) -> Result<ResolvedCliKey, String> {
        let enc = text(&canonical, "name_encrypted")?.to_string();
        // L12 §4 (2236): a request row is content-under-`C`, name derived first.
        // P2-9: the request fields only say where to look. The row IS a request
        // row only when core's bound reader (or the class+name-gated R5 reader)
        // resolves it; a server that adds those fields to a v2-pinned owner file
        // must not turn it into one, so a refusal falls through to the owner path
        // below, which keeps the recorded pin and its write target.
        // R4-B: when the request list itself could not load, the fall-through is
        // not safe: a 5xx would turn a renamed request row into a writable owner
        // row. Such a row is refused for writes below, with a reason.
        let mut unverified_request = false;
        if crate::commands::request::is_request_upload(&canonical) {
            if self.requests.is_none() && !self.requests_failed {
                match RequestKeyResolver::load(api).await {
                    Ok(requests) => self.requests = Some(requests),
                    Err(_) => self.requests_failed = true,
                }
            }
            if self.requests_failed {
                unverified_request = true;
            } else if let Some(keys) = self.requests.as_mut().and_then(|r| r.row_keys(master, &canonical)) {
                let beebeeb_core::owned_keys::RequestRowKeys {
                    content_key,
                    name_key,
                    name_origin,
                    name,
                } = keys;
                return Ok(ResolvedCliKey {
                    key: Arc::new(content_key),
                    name_key: Some(Arc::new(name_key)),
                    name_pin: Some(name.chunk_format),
                    name_origin: Some(name_origin),
                    origin: KeyOrigin::RequestContent,
                    canonical,
                    pending_owner_wrap: None,
                    shared_root_id: None,
                    name: name.name,
                    read_only: name_origin == KeyOrigin::DerivedUpperRepair,
                    read_only_reason: None,
                    write_target: None,
                });
            }
        }
        // L12: a plaintext name authenticates nothing, so core identifies such
        // a file by its first content chunk, which a listing does not carry.
        // The row is still shown under the name the server sent, but nothing
        // about it is proven: it is read-only and gets no seal target.
        if kind == owned_keys::ItemKind::File && !enc.starts_with('{') {
            return Ok(ResolvedCliKey {
                key: Arc::new(beebeeb_core::kdf::derive_file_key(master, id.as_bytes())),
                name_key: None,
                name_pin: None,
                name_origin: None,
                origin: KeyOrigin::Derived,
                canonical,
                pending_owner_wrap: None,
                shared_root_id: None,
                name: enc.to_string(),
                read_only: true,
                read_only_reason: unverified_request.then_some(REQUEST_KEYS_UNAVAILABLE),
                write_target: None,
            });
        }
        let resolved = owned_keys::resolve_owned_key(master, id, kind, None, None, &enc, None)
            .map_err(|_| "can't decrypt item name".to_string())?;
        let read_only = resolved.write_target.is_none() || unverified_request;
        let name_pin = match &resolved.name {
            beebeeb_core::owned_keys::ResolvedName::Encrypted(meta) => Some(meta.chunk_format),
            beebeeb_core::owned_keys::ResolvedName::Plaintext(_) => None,
        };
        Ok(ResolvedCliKey {
            key: Arc::new(resolved.key),
            name_key: None,
            name_pin,
            name_origin: None,
            origin: resolved.origin,
            canonical,
            pending_owner_wrap: None,
            shared_root_id: None,
            name: resolved_name(&resolved.name),
            read_only,
            read_only_reason: unverified_request.then_some(REQUEST_KEYS_UNAVAILABLE),
            write_target: if unverified_request {
                None
            } else {
                resolved.write_target.map(Arc::new)
            },
        })
    }
    /// L12: a foreign id is never derived. Its key is an L8 owner envelope that
    /// opens and authenticates the name, else an L7 child wrap of a proven
    /// custody root. An L8 envelope for a foreign id is adopted once (written
    /// back) by sync; an L7 wrap is prepared for adoption the same way.
    async fn resolve_foreign_row(
        &mut self,
        api: &ApiClient,
        master: &MasterKey,
        canonical: Value,
        id: &str,
        kind: owned_keys::ItemKind,
    ) -> Result<ResolvedCliKey, String> {
        let enc = text(&canonical, "name_encrypted")?;
        let served = normalize_wrap(&canonical["owner_wrapped_file_key"]).ok().flatten();
        let pending = self.pending.get(id).cloned();
        if let Some(frame) = served.as_deref().or(pending.as_deref()) {
            if let Ok(resolved) = owned_keys::resolve_owned_key(master, id, kind, Some(frame), None, enc, None) {
                self.pending.remove(id);
                let read_only = resolved.write_target.is_none();
                return Ok(ResolvedCliKey {
                    key: Arc::new(resolved.key),
                    name_key: None,
                    name_pin: None,
                    name_origin: None,
                    origin: resolved.origin,
                    canonical,
                    // A pending envelope is never confirmation that adoption
                    // committed; keep it so sync retries the write.
                    pending_owner_wrap: pending,
                    shared_root_id: None,
                    name: resolved_name(&resolved.name),
                    read_only,
                    read_only_reason: None,
                    write_target: resolved.write_target.map(Arc::new),
                });
            }
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
            let found = self
                .roots
                .get(&root_id)
                .map(|r| (r.key.clone(), r.children.get(id).cloned(), r.legacy_invite));
            if let Some((root_key, Some(frame), legacy_invite)) = found {
                if legacy_invite {
                    // L7b: the shipped legacy invite copy binds neither the
                    // folder id nor the item kind, so core's bound child-wrap
                    // reader must not accept it. This is the named legacy reader.
                    let key = legacy_invite_child_key(&root_key, &frame)?;
                    let name = authenticated_name(&key, &canonical)?;
                    let pending = owned_keys::wrap_owner_file_key(master, id, &key)
                        .map_err(|_| "can't prepare owner envelope")?;
                    self.pending.insert(id.to_string(), pending.clone());
                    return Ok(ResolvedCliKey {
                        key: Arc::new(key),
                        name_key: None,
                        name_pin: None,
                        name_origin: None,
                        origin: KeyOrigin::FolderWrap,
                        canonical,
                        pending_owner_wrap: Some(pending),
                        shared_root_id: Some(root_id),
                        name,
                        read_only: false,
                        read_only_reason: None,
                        write_target: None,
                    });
                }
                let folder_wrap = owned_keys::FolderWrap {
                    shared_folder_id: &root_id,
                    kind: kind.as_u8(),
                    folder_key: &root_key,
                    wrapped_key: &frame,
                };
                let resolved = owned_keys::resolve_owned_key(master, id, kind, None, Some(folder_wrap), enc, None)
                    .map_err(|_| UNREADABLE.to_string())?;
                let pending = owned_keys::wrap_owner_file_key(master, id, &resolved.key)
                    .map_err(|_| "can't prepare owner envelope")?;
                self.pending.insert(id.to_string(), pending.clone());
                let read_only = resolved.write_target.is_none();
                return Ok(ResolvedCliKey {
                    key: Arc::new(resolved.key),
                    name_key: None,
                    name_pin: None,
                    name_origin: None,
                    origin: resolved.origin,
                    canonical,
                    pending_owner_wrap: Some(pending),
                    shared_root_id: Some(root_id),
                    name: resolved_name(&resolved.name),
                    read_only,
                    read_only_reason: None,
                    write_target: resolved.write_target.map(Arc::new),
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
    /// body, missing geometry, or no key authenticated chunk 0. It never
    /// proceeds and never marks the row read-only: an interactive
    /// write is refused with [`WRITE_CHECK_UNKNOWN`], sync leaves the row alone
    /// and asks again on its next pass.
    Unknown,
}

/// What a write says when [`WriteCheck::Unknown`] refused it. Retryable: the
/// row is not marked read-only.
pub const WRITE_CHECK_UNKNOWN: &str = "Couldn't check this file before changing it. Try again.";

/// Is `e` one of the two refusals a write check gives (the row was left
/// untouched on purpose), rather than a failed request?
pub fn is_write_refusal(e: &str) -> bool {
    e == crate::crypto::OWNER_REPAIR_READ_ONLY || e == WRITE_CHECK_UNKNOWN
}

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
    /// A folder has no content: only its name decides, nothing is downloaded.
    pub is_folder: bool,
    /// The name verdict: [`name_is_upper_repair`] in `crypto::names`, which is what
    /// the resolver's `read_only` and every listing's flag carry.
    pub name_read_only: bool,
    /// The key the row resolved to, when the surface resolved one (a shared
    /// folder, owner-wrap or file-request key). Chunk 0 opening under it proves
    /// the version is not uppercase-sealed: core's owner resolver is asked
    /// first, so the owner's uppercase key is reported as `UpperRepair` before
    /// this key is tried. Without it, only the owner conventions can prove a
    /// version writable.
    pub row_key: Option<&'a FileKey>,
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
    // A folder is never probed: it has no version to read (C4).
    if target.is_folder || target.chunk_count == 0 {
        return WriteCheck::Writable;
    }
    let probe =
        |wide| crate::download::download_frame_prefix(api, target.file_id, target.size_bytes, target.chunk_count, wide);
    let Ok((body, chunk_size, complete)) = probe(false).await else {
        return WriteCheck::Unknown;
    };
    let check = |body: &[u8], chunk_size: Option<u64>, complete: bool| {
        first_chunk_check(
            master,
            target.file_id,
            target.row_key,
            body,
            target.size_bytes,
            target.chunk_count,
            chunk_size,
            complete,
        )
    };
    let verdict = check(&body, chunk_size, complete);
    // Round 3 (K2): a legacy multi-chunk raw file without X-Chunk-Size whose
    // first frame is longer than the ordinary prefix. Read every feasible
    // frame-0 length once (a second request, only in this case) and probe again.
    if verdict == WriteCheck::Unknown
        && target.chunk_count > 1
        && target.size_bytes.is_some()
        && chunk_size.is_none()
        && !complete
    {
        let Ok((body, chunk_size, complete)) = probe(true).await else {
            return WriteCheck::Unknown;
        };
        return check(&body, chunk_size, complete);
    }
    verdict
}

/// Decide from the prefix of the current version. Writable needs PROOF: chunk 0
/// authenticated under a write-capable key. No candidate length (missing
/// geometry: a single-chunk row whose size the listing did not carry, a body
/// cut short) or no key that opens chunk 0 is [`WriteCheck::Unknown`], never
/// writable and never read-only.
#[allow(clippy::too_many_arguments)]
fn first_chunk_check(
    master: &MasterKey,
    file_id: &str,
    row_key: Option<&FileKey>,
    body: &[u8],
    size_bytes: Option<u64>,
    chunk_count: u32,
    chunk_size: Option<u64>,
    complete: bool,
) -> WriteCheck {
    for len in chunk0_lengths(body.len(), size_bytes, chunk_count, chunk_size, complete) {
        if let Some(verdict) = frame_check(master, file_id, row_key, &body[..len]) {
            return verdict;
        }
    }
    // Round 3 (K2): a legacy multi-chunk raw file served without X-Chunk-Size.
    // The read path's AEAD-validated frame-size search (same order, bounds and
    // budget), over the prefix in hand and bounded by the version's length from
    // the listing.
    if chunk_count > 1 {
        if let Some(size) = size_bytes {
            let total = size.saturating_add(28u64.saturating_mul(u64::from(chunk_count)));
            let mut verdict = None;
            let _ = crate::crypto::probe_first_frame_len(body, total, chunk_count as usize, chunk_size, &mut |frame| {
                verdict = frame_check(master, file_id, row_key, frame);
                verdict.is_some()
            });
            if let Some(verdict) = verdict {
                return verdict;
            }
        }
    }
    // Round 3 (K1): a legacy CLI JSON-blob chunk 0. Only the old iOS File
    // Provider sealed under the uppercase id, and it wrote raw frames, so a JSON
    // blob is never UpperRepair: it is writable once blob 0 authenticates under
    // the owner's string or binary key, or the row's own key.
    if let Some(frame) = crate::crypto::first_json_blob_frame(body) {
        if json_blob_opens(master, file_id, row_key, &frame) {
            return WriteCheck::Writable;
        }
    }
    WriteCheck::Unknown
}

/// One candidate frame 0: core's owner resolver first (`UpperRepair`, or a
/// writable String/Binary convention), then the row's own key. `None`: no key
/// opens it.
fn frame_check(master: &MasterKey, file_id: &str, row_key: Option<&FileKey>, frame: &[u8]) -> Option<WriteCheck> {
    if let Some((_, convention)) = crate::crypto::owner_content_key(master, file_id, frame) {
        return Some(if convention == beebeeb_core::kdf::OwnerKeyConvention::UpperRepair {
            WriteCheck::UpperRepair
        } else {
            WriteCheck::Writable
        });
    }
    row_key
        .is_some_and(|key| crate::crypto::first_frame_opens(key, file_id, frame))
        .then_some(WriteCheck::Writable)
}

/// Does a JSON-blob chunk-0 frame open under a write-capable key: the owner's
/// string-id key, its legacy binary-id key (the old CLI's), or the row's own?
/// One key at a time, each dropped (zeroized) before the next is derived.
fn json_blob_opens(master: &MasterKey, file_id: &str, row_key: Option<&FileKey>, frame: &[u8]) -> bool {
    let Ok(uuid) = uuid::Uuid::parse_str(file_id) else {
        return false;
    };
    if crate::crypto::first_frame_opens(
        &beebeeb_core::kdf::derive_file_key(master, file_id.as_bytes()),
        file_id,
        frame,
    ) {
        return true;
    }
    if crate::crypto::first_frame_opens(
        &beebeeb_core::kdf::derive_file_key(master, uuid.as_bytes()),
        file_id,
        frame,
    ) {
        return true;
    }
    row_key.is_some_and(|key| crate::crypto::first_frame_opens(key, file_id, frame))
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
    // N5: keep the array in a Zeroizing buffer. A bare `[u8; 32]` in this
    // frame would never be wiped; core only wipes the value it is handed.
    let key = zeroize::Zeroizing::new(
        <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "invalid legacy invite child key length")?,
    );
    Ok(crate::crypto::key_from_bytes(&key))
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

#[cfg(test)]
mod write_check_tests;
