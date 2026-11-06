//! The folders a `bb push` writes into: the existing folder of that name, else a new one (task 2334).
//!
//! A re-push of a folder reuses the folder of the same name already in the target, at every level, instead of
//! making a second one. A name matches only when it decrypts to the same bytes (the 1022 spec's rule for a
//! client that mirrors a local tree), through the key resolver `bb ls` and the conflict check use, so a folder a
//! collaborator made inside a shared folder (sealed under that folder's key) matches too; a plaintext name,
//! which anyone with database access could have planted, never matches. A folder row this device cannot read
//! fails the lookup when nothing else matches: it may be the one. When several folders of the name exist, the
//! oldest is used (`created_at`, then the smallest id), so every run picks the same one; for a folder found
//! implicitly, the oldest one that is not shared on its own comes first, with or without `--reuse-shared` (a
//! "no" below stays a no). One line, after the choice, says how many there are and which was used.
//!
//! A folder found IMPLICITLY (the pushed folder's own name, or one of its subfolders) is reused without a
//! question only when it is not shared on its own. Before this, a push always made a new, private folder, so
//! reusing a shared one silently would show the new files to other people. A shared one is reused after a yes at
//! the terminal, or with `--reuse-shared`; without a terminal the push stops and says why; a no makes a new
//! folder, as before. "Shared on its own" counts the folder's own invited people and its own links: what it
//! inherits from its parent does not count, because a new folder in that parent inherits the same. A folder
//! named EXPLICITLY (`--folder`) is the user's own choice and is reused even when shared.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;

use beebeeb_core::kdf::MasterKey;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde_json::Value;
use uuid::Uuid;

use crate::api::ApiClient;
use crate::owned_keys::OwnedKeyResolver;

/// What a push does with an implicitly found folder that is shared on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedReuse {
    /// Ask at the terminal; refuse when no one can answer.
    Ask,
    /// `--reuse-shared`: upload into it.
    Allow,
}

/// A folder of the wanted name, as the parent's listing shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub id: Uuid,
    pub created_at: Option<DateTime<Utc>>,
    /// Active file-style links on the folder itself (the listing's `share_count`; `None` when the row has none).
    pub share_count: Option<u64>,
}

/// The folders of one parent as a push read them: each readable folder with its decrypted name, and the folder
/// rows this device could not read. Read once per parent per push.
#[derive(Debug, Default)]
pub(crate) struct ParentFolders {
    named: Vec<(Candidate, String)>,
    /// How many folder rows could not be read, and why the first one could not.
    unreadable: Option<(usize, String)>,
}

impl ParentFolders {
    /// Read the folder rows of a listing with `keys`. Rows that are not folders, are trashed, have no id or carry
    /// a plaintext name are left out.
    pub async fn read(keys: &mut OwnedKeyResolver, api: &ApiClient, master_key: &MasterKey, rows: &[Value]) -> Self {
        keys.seed(rows);
        let mut folders = ParentFolders::default();
        for row in rows {
            if row.get("is_folder").and_then(Value::as_bool) != Some(true)
                || row.get("is_trashed").and_then(Value::as_bool) == Some(true)
                || !row
                    .get("name_encrypted")
                    .and_then(Value::as_str)
                    .is_some_and(|n| n.starts_with('{'))
            {
                continue;
            }
            let Some(id) = row
                .get("id")
                .and_then(Value::as_str)
                .and_then(|t| Uuid::parse_str(t).ok())
            else {
                continue;
            };
            let candidate = Candidate {
                id,
                created_at: row
                    .get("created_at")
                    .and_then(Value::as_str)
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&Utc)),
                share_count: row.get("share_count").and_then(Value::as_u64),
            };
            match keys.resolve(api, master_key, row).await {
                Ok(resolved) => folders.named.push((candidate, resolved.name)),
                Err(e) => {
                    let (count, _) = folders.unreadable.get_or_insert((0, e));
                    *count += 1;
                }
            }
        }
        folders
    }

    /// The folders named exactly `wanted`, oldest first. None of them while a folder row here could not be read
    /// is an error, never "none": that row may be the one.
    pub fn named(&self, wanted: &str, shown: &str) -> Result<Vec<Candidate>, String> {
        let found: Vec<Candidate> = self
            .named
            .iter()
            .filter(|(_, name)| name == wanted)
            .map(|(candidate, _)| candidate.clone())
            .collect();
        match &self.unreadable {
            Some((count, why)) if found.is_empty() => Err(format!(
                "cannot tell whether {shown} already exists: {count} folder{} in the folder it goes into could \
                 not be read ({why})",
                if *count == 1 { "" } else { "s" }
            )),
            _ => Ok(oldest_first(found)),
        }
    }
}

/// Who a folder is shared with, on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exposure {
    Known {
        people: usize,
        links: u64,
    },
    /// The server does not say: it has no invite route (the row's `share_count` counts public links, never
    /// people), or no folder-link route and the row carries no `share_count`. Treated as shared (lead ruling,
    /// 2026-11-06: "not supported" is unknown, never private).
    Unknown,
}

impl Exposure {
    pub fn is_shared(self) -> bool {
        match self {
            Exposure::Known { people, links } => people > 0 || links > 0,
            Exposure::Unknown => true,
        }
    }

    /// What follows the folder's name: "is shared with 2 people and 1 link", "may be shared: …".
    pub fn describe(self) -> String {
        let (people, links) = match self {
            Exposure::Unknown => return "may be shared: this server does not say with whom".to_string(),
            Exposure::Known { people, links } => (people, links),
        };
        let people = match people {
            0 => None,
            1 => Some("1 person".to_string()),
            n => Some(format!("{n} people")),
        };
        let links = match links {
            0 => None,
            1 => Some("1 link".to_string()),
            n => Some(format!("{n} links")),
        };
        let with = match (people, links) {
            (Some(p), Some(l)) => format!("{p} and {l}"),
            (Some(one), None) | (None, Some(one)) => one,
            (None, None) => "no one".to_string(),
        };
        format!("is shared with {with}")
    }
}

/// This account's folder invites and folder links, by folder: read once per push, and only when a reuse
/// candidate needs it.
#[derive(Debug, Default)]
pub(crate) struct ShareIndex {
    /// `None`: the server has no invite route, so it cannot say.
    people: Option<HashMap<Uuid, BTreeSet<String>>>,
    /// `None`: the server has no folder-link route, so it cannot say.
    links: Option<HashMap<Uuid, u64>>,
}

impl ShareIndex {
    /// Build from `GET /shares/invites/sent` and `GET /shares/folder-links` (`Value::Null` = the server has
    /// no such route). An invite counts while it can still grant access (invited, claimed or approved, not
    /// expired); a link while it is live (not revoked, not expired; a paused link can be resumed, so it counts).
    /// An answer without its list cannot say who sees a folder: an error, never "nobody".
    pub fn from_answers(invites: &Value, links: &Value, now: DateTime<Utc>) -> Result<ShareIndex, String> {
        let list = |answer: &Value, field: &str| -> Result<Vec<Value>, String> {
            if answer.is_null() {
                return Ok(Vec::new());
            }
            answer
                .get(field)
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| format!("the server's answer has no {field} list"))
        };
        let (invite_rows, link_rows) = (list(invites, "invites")?, list(links, "links")?);
        let live = |row: &Value| {
            row.get("expires_at")
                .and_then(Value::as_str)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .is_none_or(|t| t.with_timezone(&Utc) > now)
        };
        let mut people: HashMap<Uuid, BTreeSet<String>> = HashMap::new();
        for invite in &invite_rows {
            let granting = matches!(
                invite.get("status").and_then(Value::as_str),
                Some("invited" | "claimed" | "approved")
            );
            if invite.get("is_folder_share").and_then(Value::as_bool) != Some(true) || !granting || !live(invite) {
                continue;
            }
            let (Some(folder), Some(email)) = (
                invite
                    .get("file_id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok()),
                invite.get("recipient_email").and_then(Value::as_str),
            ) else {
                continue;
            };
            people.entry(folder).or_default().insert(email.to_lowercase());
        }
        let mut link_counts: HashMap<Uuid, u64> = HashMap::new();
        for link in &link_rows {
            let revoked = link.get("revoked_at").is_some_and(|v| !v.is_null());
            let Some(folder) = link
                .get("folder_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            if !revoked && live(link) {
                *link_counts.entry(folder).or_default() += 1;
            }
        }
        Ok(ShareIndex {
            people: (!invites.is_null()).then_some(people),
            links: (!links.is_null()).then_some(link_counts),
        })
    }

    /// The candidate's own people and links.
    ///
    /// The row's `share_count` counts its public links only (server `PAGE_SHARE_COUNTS_SQL`), never invites
    /// and never folder links, so it cannot stand in for the invite list: without that route the folder is
    /// [`Exposure::Unknown`]. Without the folder-link route it can: a 501 means shared folders are off for this
    /// owner, and the server then serves none of their folder links (`folder_public/link.rs`), so the row's
    /// `share_count` is all the link exposure there is; with no `share_count` either, it is Unknown.
    pub fn exposure(&self, candidate: &Candidate) -> Exposure {
        let Some(people) = &self.people else {
            return Exposure::Unknown;
        };
        let people = people.get(&candidate.id).map_or(0, BTreeSet::len);
        match (&self.links, candidate.share_count) {
            (Some(links), share_count) => Exposure::Known {
                people,
                links: links.get(&candidate.id).copied().unwrap_or(0) + share_count.unwrap_or(0),
            },
            (None, Some(share_count)) => Exposure::Known {
                people,
                links: share_count,
            },
            (None, None) => Exposure::Unknown,
        }
    }
}

/// Finds or creates the folders of one push. Folders it created itself are new, so nothing is looked up
/// under them.
pub(crate) struct PushFolders<'a> {
    api: &'a ApiClient,
    master_key: &'a MasterKey,
    shared: SharedReuse,
    created: HashSet<Uuid>,
    /// Each parent's folders, read once per push (`None` = the root): sibling folders share one listing.
    parents: HashMap<Option<Uuid>, ParentFolders>,
    keys: OwnedKeyResolver,
    /// The share lists, read once per push; a failure is kept too, so it is asked and said once.
    shares: Option<Result<ShareIndex, String>>,
    /// The first folder reused although shared on its own (after a yes or `--reuse-shared`).
    shared_reused: Option<Uuid>,
    /// `--reuse-shared` with share lists that could not be read: said once.
    share_failure_said: bool,
}

impl<'a> PushFolders<'a> {
    pub fn new(api: &'a ApiClient, master_key: &'a MasterKey, shared: SharedReuse) -> Self {
        Self {
            api,
            master_key,
            shared,
            created: HashSet::new(),
            parents: HashMap::new(),
            keys: OwnedKeyResolver::new(),
            shares: None,
            shared_reused: None,
            share_failure_said: false,
        }
    }

    /// The folder named `name` in `parent` that a push of a local folder writes into, found implicitly. `shown`
    /// is how the folder is named to the user (its path relative to the push).
    ///
    /// Of several same-name folders the oldest that is not shared on its own is used, with or without
    /// `--reuse-shared`: a "no" to a shared one made a private one next to it, and every later run must write
    /// there. Only when every one is shared (or this push cannot tell) is the oldest offered: asked at a terminal,
    /// or taken with `--reuse-shared`.
    pub async fn child(&mut self, parent: Option<Uuid>, name: &str, shown: &str) -> Result<Uuid, String> {
        if parent.is_some_and(|p| self.created.contains(&p)) {
            return self.create(parent, name).await;
        }
        let candidates = self.find(parent, name, shown).await?;
        if candidates.is_empty() {
            return self.create(parent, name).await;
        }
        // `None`: the share lists could not be read, and `--reuse-shared` lets the push go on (said once).
        let exposures = match self.exposures(&candidates, shown).await {
            Ok(exposures) => Some(exposures),
            Err(e) if self.shared == SharedReuse::Allow => {
                if !self.share_failure_said {
                    note(&format!("{e}; uploading into it (--reuse-shared)"));
                    self.share_failure_said = true;
                }
                None
            }
            Err(e) => return Err(e),
        };
        if let Some(i) = exposures.as_ref().and_then(|e| e.iter().position(|x| !x.is_shared())) {
            let which = if i == 0 {
                "the oldest"
            } else {
                "the oldest one that is not shared"
            };
            say_how_many(&candidates, shown, which);
            return Ok(candidates[i].id);
        }
        let reuse = match exposures.as_ref().map(|e| e[0]) {
            None => true,
            Some(exposure) if self.shared == SharedReuse::Allow => {
                note(&format!(
                    "{shown} {}; uploading into it (--reuse-shared)",
                    exposure.describe()
                ));
                true
            }
            Some(exposure) => ask_reuse_shared(shown, exposure)?,
        };
        let oldest = candidates[0].id;
        if reuse {
            say_how_many(&candidates, shown, "the oldest");
            self.shared_reused.get_or_insert(oldest);
            return Ok(oldest);
        }
        let created = self.create(parent, name).await?;
        let left = if candidates.len() == 1 {
            "the shared one is left as it was"
        } else {
            "the shared ones are left as they were"
        };
        note(&format!("made a new folder {shown}; {left}"));
        Ok(created)
    }

    /// The root-level folder `--folder` names: the user's explicit choice, reused even when shared.
    pub async fn named(&mut self, name: &str) -> Result<Uuid, String> {
        let candidates = self.find(None, name, name).await?;
        match candidates.first() {
            Some(oldest) => {
                say_how_many(&candidates, name, "the oldest");
                Ok(oldest.id)
            }
            None => self.create(None, name).await,
        }
    }

    /// A new folder, never a lookup: for a local name that is not UTF-8, which the push stores under a stand-in
    /// name that must not match (and merge into) an unrelated folder of that name.
    pub async fn new_folder(&mut self, parent: Option<Uuid>, name: &str) -> Result<Uuid, String> {
        self.create(parent, name).await
    }

    /// The first folder this push reused although it is shared on its own, if any.
    pub fn shared_reused(&self) -> Option<Uuid> {
        self.shared_reused
    }

    /// The folders of this name in `parent`, oldest first.
    async fn find(&mut self, parent: Option<Uuid>, name: &str, shown: &str) -> Result<Vec<Candidate>, String> {
        let folders = match self.parents.remove(&parent) {
            Some(folders) => folders,
            None => self.read_parent(parent, shown).await?,
        };
        let found = folders.named(name, shown);
        self.parents.insert(parent, folders);
        found
    }

    async fn read_parent(&mut self, parent: Option<Uuid>, shown: &str) -> Result<ParentFolders, String> {
        let parent_text = parent.map(|p| p.to_string());
        let listing = self.api.list_files(parent_text.as_deref()).await?;
        if crate::sync_safety::listing_truncated(&listing) {
            return Err(format!(
                "cannot tell whether {shown} already exists: the listing of the folder it goes into is incomplete"
            ));
        }
        let rows = listing
            .get("files")
            .and_then(Value::as_array)
            .ok_or("invalid file listing response")?;
        Ok(ParentFolders::read(&mut self.keys, self.api, self.master_key, rows).await)
    }

    /// Each candidate's own exposure, from the share lists read once per push.
    async fn exposures(&mut self, candidates: &[Candidate], shown: &str) -> Result<Vec<Exposure>, String> {
        let shares = match self.shares.take() {
            Some(shares) => shares,
            None => self.read_shares(shown).await,
        };
        let exposures = match &shares {
            Ok(index) => Ok(candidates.iter().map(|c| index.exposure(c)).collect()),
            Err(e) => Err(e.clone()),
        };
        self.shares = Some(shares);
        exposures
    }

    async fn read_shares(&self, shown: &str) -> Result<ShareIndex, String> {
        let unknown = |e: String| format!("could not check whether {shown} is shared: {e}");
        let (invites, links) = tokio::join!(self.api.list_sent_invites(), self.api.list_links_to_folders());
        ShareIndex::from_answers(
            &invites.map_err(|e| unknown(e.to_string()))?,
            &links.map_err(|e| unknown(e.to_string()))?,
            Utc::now(),
        )
        .map_err(unknown)
    }

    async fn create(&mut self, parent: Option<Uuid>, name: &str) -> Result<Uuid, String> {
        // L13 (SF-0001): the client's own id, echoed unchanged by the answer.
        let id = crate::item_id::create_owned_folder(self.api, self.master_key, name, parent).await?;
        self.created.insert(id);
        Ok(id)
    }
}

/// `candidates` in the order a push considers them: the oldest by `created_at`, then the smallest id. One
/// without a creation time sorts after every one that has it.
pub(crate) fn oldest_first(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by_key(|c| (c.created_at.is_none(), c.created_at, c.id));
    candidates
}

/// "2 folders named X are here; using the oldest", when there is more than one.
fn say_how_many(candidates: &[Candidate], shown: &str, which: &str) {
    if candidates.len() > 1 {
        note(&format!(
            "{} folders named {shown} are here; using {which}",
            candidates.len()
        ));
    }
}

/// One line on stderr, unless `--quiet`.
fn note(line: &str) {
    if !crate::ui::is_quiet() {
        eprintln!("  {}", line.custom_color(crate::colors::INK_DIM));
    }
}

/// Ask whether to upload into the shared folder `shown`; `Err` when no one can answer.
fn ask_reuse_shared(shown: &str, exposure: Exposure) -> Result<bool, String> {
    let shared = format!("{shown} already exists here and {}", exposure.describe());
    let no_one_to_ask = if crate::ui::is_json() || crate::ui::is_quiet() {
        Some("--json and --quiet ask no questions")
    } else if !crate::exit::stdin_is_interactive() {
        Some("stdin is not a terminal, so there is no one to ask")
    } else {
        None
    };
    if let Some(why) = no_one_to_ask {
        return Err(crate::exit::with_code(
            crate::exit::USAGE,
            format!("{shared}. Pass --reuse-shared to upload into it, or push it somewhere else ({why})"),
        ));
    }
    eprint!(
        "  {} {} [y/N]: ",
        format!("{shared}.").custom_color(crate::colors::AMBER),
        "Upload into it?".custom_color(crate::colors::INK_DIM),
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Ok(false);
    }
    Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(t: &str) -> Option<DateTime<Utc>> {
        Some(DateTime::parse_from_rfc3339(t).unwrap().with_timezone(&Utc))
    }

    fn id(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn candidate(n: u8, created_at: Option<DateTime<Utc>>) -> Candidate {
        Candidate {
            id: id(n),
            created_at,
            share_count: Some(0),
        }
    }

    #[test]
    fn the_oldest_same_name_folder_comes_first_then_the_smallest_id() {
        let first = |c: Vec<Candidate>| oldest_first(c).first().cloned();
        let newer = candidate(1, at("2026-11-06T10:00:00Z"));
        let older = candidate(9, at("2026-11-05T10:00:00+00:00"));
        assert_eq!(first(vec![newer.clone(), older.clone()]), Some(older.clone()));
        assert_eq!(first(vec![older.clone(), newer.clone()]), Some(older.clone()));

        // Equal times: the smallest id, whatever the listing order.
        let a = candidate(2, at("2026-11-05T10:00:00Z"));
        let b = candidate(3, at("2026-11-05T10:00:00.000Z"));
        assert_eq!(first(vec![b.clone(), a.clone()]), Some(a.clone()));

        // No creation time sorts last.
        let unknown = candidate(0, None);
        assert_eq!(
            oldest_first(vec![unknown.clone(), newer.clone(), older.clone()]),
            vec![older, newer, unknown.clone()]
        );
        assert_eq!(first(vec![unknown.clone()]), Some(unknown));
        assert_eq!(first(vec![]), None);
    }

    /// A sealed folder row, as `bb mkdir` would make it.
    fn sealed_folder(master: &MasterKey, name: &str, created_at: &str) -> Value {
        let item = beebeeb_core::item_id::NewItemId::owner(master).unwrap();
        let sealed = beebeeb_core::encrypt::encrypt_name_for_new_item(
            master,
            &item,
            name,
            None,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
        )
        .unwrap();
        json!({
            "id": item.as_str(),
            "name_encrypted": sealed,
            "is_folder": true,
            "is_trashed": false,
            "created_at": created_at,
            "share_count": 2,
        })
    }

    #[tokio::test]
    async fn only_folders_whose_sealed_name_is_byte_identical_are_candidates() {
        let master = MasterKey::from_bytes([7u8; 32]);
        let other_key = MasterKey::from_bytes([8u8; 32]);
        let wanted = "A House of Dynamite (2025)";
        let mine = sealed_folder(&master, wanted, "2026-11-06T10:00:00Z");
        let mut a_file = sealed_folder(&master, wanted, "2026-11-06T10:00:00Z");
        a_file["is_folder"] = json!(false);
        let mut trashed = sealed_folder(&master, wanted, "2026-11-06T10:00:00Z");
        trashed["is_trashed"] = json!(true);
        let rows = vec![
            mine.clone(),
            a_file,
            trashed,
            sealed_folder(&master, "a house of dynamite (2025)", "2026-11-06T10:00:00Z"),
            sealed_folder(&master, &format!("{wanted} "), "2026-11-06T10:00:00Z"),
            sealed_folder(&other_key, wanted, "2026-11-06T10:00:00Z"),
            // A plaintext name decrypts "as is", but proves nothing: never adopted.
            json!({ "id": id(4).to_string(), "name_encrypted": wanted, "is_folder": true }),
        ];
        // The row sealed under another key needs the server to be read, and this one answers nothing.
        let api = ApiClient::new_for_test("http://127.0.0.1:9".to_string());
        let read = |rows: Vec<Value>| {
            let (api, master) = (&api, &master);
            async move { ParentFolders::read(&mut OwnedKeyResolver::new(), api, master, &rows).await }
        };
        let found = read(rows.clone()).await.named(wanted, wanted).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id.to_string(), mine["id"].as_str().unwrap());
        assert_eq!(found[0].created_at, at("2026-11-06T10:00:00Z"));
        assert_eq!(found[0].share_count, Some(2));

        // Without that match, the unreadable folder may be the one: an error, never "none here".
        let without_mine: Vec<Value> = rows.into_iter().filter(|r| r["id"] != mine["id"]).collect();
        let err = read(without_mine).await.named(wanted, wanted).unwrap_err();
        assert!(
            err.contains("1 folder in the folder it goes into could not be read"),
            "{err}"
        );
        // And with no unreadable row, an absent name is simply none.
        let readable = vec![sealed_folder(&master, "Other", "2026-11-06T10:00:00Z")];
        assert_eq!(read(readable).await.named(wanted, wanted).unwrap(), vec![]);
    }

    #[test]
    fn a_folder_is_shared_by_its_own_live_invites_and_links_only() {
        let now = DateTime::parse_from_rfc3339("2026-11-06T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let folder = id(1);
        let invites = json!({ "invites": [
            { "file_id": folder, "is_folder_share": true, "status": "approved", "recipient_email": "a@x.eu" },
            { "file_id": folder, "is_folder_share": true, "status": "invited", "recipient_email": "A@x.eu" },
            { "file_id": folder, "is_folder_share": true, "status": "claimed", "recipient_email": "b@x.eu",
              "expires_at": "2026-11-07T00:00:00Z" },
            { "file_id": folder, "is_folder_share": true, "status": "approved", "recipient_email": "gone@x.eu",
              "expires_at": "2026-11-05T00:00:00Z" },
            { "file_id": folder, "is_folder_share": true, "status": "denied", "recipient_email": "c@x.eu" },
            { "file_id": folder, "is_folder_share": true, "status": "withdrawn", "recipient_email": "d@x.eu" },
            { "file_id": folder, "is_folder_share": false, "status": "approved", "recipient_email": "e@x.eu" },
            { "file_id": id(2), "is_folder_share": true, "status": "approved", "recipient_email": "f@x.eu" },
        ]});
        let links = json!({ "links": [
            { "folder_id": folder, "revoked_at": null, "expires_at": null },
            { "folder_id": folder, "revoked_at": null, "expires_at": "2026-11-05T00:00:00Z" },
            { "folder_id": folder, "revoked_at": "2026-11-01T00:00:00Z" },
            { "folder_id": folder, "paused": true },
            { "folder_id": id(2) },
        ]});
        let index = ShareIndex::from_answers(&invites, &links, now).unwrap();
        let mut c = candidate(1, None);
        c.share_count = Some(1);
        let exposure = index.exposure(&c);
        assert_eq!(
            exposure,
            Exposure::Known { people: 2, links: 3 },
            "a@ (twice, any case) and b@; 2 live links + 1"
        );
        assert_eq!(exposure.describe(), "is shared with 2 people and 3 links");
        assert!(exposure.is_shared());

        let private = index.exposure(&candidate(3, None));
        assert!(!private.is_shared());

        // "Not supported" (404/405/501, read as Null) is unknown, never private. Without the invite route no
        // share_count can say who was invited (it counts public links only): unknown, whatever the row says.
        let mut row = candidate(1, None);
        for links_answer in [&Value::Null, &links] {
            let no_invites = ShareIndex::from_answers(&Value::Null, links_answer, now).unwrap();
            for share_count in [None, Some(0), Some(2)] {
                row.share_count = share_count;
                assert_eq!(no_invites.exposure(&row), Exposure::Unknown, "{share_count:?}");
            }
        }
        assert!(Exposure::Unknown.is_shared());
        assert_eq!(
            Exposure::Unknown.describe(),
            "may be shared: this server does not say with whom"
        );
        // Without the folder-link route (a 501: shared folders off for this owner, so none of their folder links
        // is served) the row's share_count is the link exposure; with no share_count either, unknown.
        let invites_only = ShareIndex::from_answers(&invites, &Value::Null, now).unwrap();
        row.share_count = Some(0);
        assert_eq!(invites_only.exposure(&row), Exposure::Known { people: 2, links: 0 });
        row.share_count = Some(2);
        assert_eq!(invites_only.exposure(&row), Exposure::Known { people: 2, links: 2 });
        row.share_count = None;
        assert_eq!(invites_only.exposure(&row), Exposure::Unknown);
        assert_eq!(
            invites_only.exposure(&candidate(3, None)),
            Exposure::Known { people: 0, links: 0 },
            "no invite, no public link, no folder link served: private"
        );

        // An answer that is there but carries no list says nothing: an error, never "nobody".
        for (bad_invites, bad_links) in [(json!({}), links.clone()), (invites.clone(), json!({ "error": "x" }))] {
            let err = ShareIndex::from_answers(&bad_invites, &bad_links, now).unwrap_err();
            assert!(err.contains("has no"), "{err}");
        }

        assert_eq!(
            Exposure::Known { people: 1, links: 0 }.describe(),
            "is shared with 1 person"
        );
        assert_eq!(
            Exposure::Known { people: 0, links: 1 }.describe(),
            "is shared with 1 link"
        );
    }
}
