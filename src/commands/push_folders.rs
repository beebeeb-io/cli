//! The folders a `bb push` writes into: the existing folder of that name, else a new one (task 2334).
//!
//! A re-push of a folder reuses the folder of the same name already in the target, at every level, instead of
//! making a second one. A name matches only when it decrypts, under this account's key, to the same bytes (the
//! 1022 spec's rule for a client that mirrors a local tree); a plaintext name, which anyone with database access
//! could have planted, never matches. When several folders of the name exist, the oldest is used (`created_at`,
//! then the smallest id), so every run picks the same one, and one line says how many there are; for a folder
//! found implicitly, the oldest one that is not shared on its own comes first (a "no" below stays a no).
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

/// The folders in a listing whose name decrypts to exactly `wanted`. Rows that are not folders, are trashed,
/// carry a plaintext name or do not decrypt under this key are not candidates.
pub(crate) fn same_name_folders(rows: &[Value], master_key: &MasterKey, wanted: &str) -> Vec<Candidate> {
    rows.iter()
        .filter(|row| row.get("is_folder").and_then(Value::as_bool) == Some(true))
        .filter(|row| row.get("is_trashed").and_then(Value::as_bool) != Some(true))
        .filter_map(|row| {
            let id_text = row.get("id")?.as_str()?;
            let id = Uuid::parse_str(id_text).ok()?;
            let sealed = row.get("name_encrypted")?.as_str()?;
            if !sealed.starts_with('{') {
                return None;
            }
            let name = crate::crypto::decrypt_name(master_key, id_text, sealed)?;
            (name == wanted).then(|| Candidate {
                id,
                created_at: row
                    .get("created_at")
                    .and_then(Value::as_str)
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&Utc)),
                share_count: row.get("share_count").and_then(Value::as_u64),
            })
        })
        .collect()
}

/// Who a folder is shared with, on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exposure {
    Known {
        people: usize,
        links: u64,
    },
    /// The server does not say: a share list has no route here and the row carries no `share_count`. Treated
    /// as shared (lead ruling, 2026-11-06: "not supported" is unknown, never private).
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
    pub fn from_answers(invites: &Value, links: &Value, now: DateTime<Utc>) -> ShareIndex {
        let live = |row: &Value| {
            row.get("expires_at")
                .and_then(Value::as_str)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .is_none_or(|t| t.with_timezone(&Utc) > now)
        };
        let mut people: HashMap<Uuid, BTreeSet<String>> = HashMap::new();
        for invite in invites.get("invites").and_then(Value::as_array).into_iter().flatten() {
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
        for link in links.get("links").and_then(Value::as_array).into_iter().flatten() {
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
        ShareIndex {
            people: (!invites.is_null()).then_some(people),
            links: (!links.is_null()).then_some(link_counts),
        }
    }

    /// The candidate's own people and links. When the server has no route for one of the two lists, the row's
    /// `share_count` decides; with no `share_count` either, it is [`Exposure::Unknown`].
    pub fn exposure(&self, candidate: &Candidate) -> Exposure {
        let people = self
            .people
            .as_ref()
            .map(|p| p.get(&candidate.id).map_or(0, BTreeSet::len));
        let links = self.links.as_ref().map(|l| l.get(&candidate.id).copied().unwrap_or(0));
        match (people, links, candidate.share_count) {
            (Some(people), Some(links), share_count) => Exposure::Known {
                people,
                links: links + share_count.unwrap_or(0),
            },
            (people, links, Some(share_count)) => Exposure::Known {
                people: people.unwrap_or(0),
                links: links.unwrap_or(0) + share_count,
            },
            (_, _, None) => Exposure::Unknown,
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
    shares: Option<ShareIndex>,
    /// The first folder reused although shared on its own (after a yes or `--reuse-shared`).
    shared_reused: Option<Uuid>,
}

impl<'a> PushFolders<'a> {
    pub fn new(api: &'a ApiClient, master_key: &'a MasterKey, shared: SharedReuse) -> Self {
        Self {
            api,
            master_key,
            shared,
            created: HashSet::new(),
            shares: None,
            shared_reused: None,
        }
    }

    /// The folder named `name` in `parent` that a push of a local folder writes into, found implicitly. `shown`
    /// is how the folder is named to the user (its path relative to the push).
    ///
    /// Of several same-name folders the oldest that is not shared on its own is used: a "no" to a shared one
    /// made a private one next to it, and the next run must write there without asking again. Only when every
    /// one is shared is the oldest offered.
    pub async fn child(&mut self, parent: Option<Uuid>, name: &str, shown: &str) -> Result<Uuid, String> {
        if parent.is_some_and(|p| self.created.contains(&p)) {
            return self.create(parent, name).await;
        }
        let candidates = self.find(parent, name, shown).await?;
        let Some(oldest) = candidates.first().cloned() else {
            return self.create(parent, name).await;
        };
        if self.shared == SharedReuse::Allow {
            // The user chose to reuse it either way: who it is shared with is said when it can be read.
            if let Ok(exposure) = self.exposure(&oldest, shown).await
                && exposure.is_shared()
            {
                note(&format!(
                    "{shown} {}; uploading into it (--reuse-shared)",
                    exposure.describe()
                ));
                self.shared_reused.get_or_insert(oldest.id);
            }
            say_how_many(&candidates, shown, "the oldest");
            return Ok(oldest.id);
        }
        let mut exposures = Vec::with_capacity(candidates.len());
        for candidate in &candidates {
            exposures.push(self.exposure(candidate, shown).await?);
        }
        if let Some(i) = exposures.iter().position(|e| !e.is_shared()) {
            say_how_many(
                &candidates,
                shown,
                if i == 0 {
                    "the oldest"
                } else {
                    "the oldest one that is not shared"
                },
            );
            return Ok(candidates[i].id);
        }
        say_how_many(&candidates, shown, "the oldest");
        if ask_reuse_shared(shown, exposures[0])? {
            self.shared_reused.get_or_insert(oldest.id);
            return Ok(oldest.id);
        }
        let created = self.create(parent, name).await?;
        note(&format!("made a new folder {shown}; the shared one is left as it was"));
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
    async fn find(&self, parent: Option<Uuid>, name: &str, shown: &str) -> Result<Vec<Candidate>, String> {
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
        Ok(oldest_first(same_name_folders(rows, self.master_key, name)))
    }

    async fn exposure(&mut self, candidate: &Candidate, shown: &str) -> Result<Exposure, String> {
        if self.shares.is_none() {
            let unknown = |e: crate::api::ApiError| format!("could not check whether {shown} is shared: {e}");
            let (invites, links) = tokio::join!(self.api.list_sent_invites(), self.api.list_folder_links());
            self.shares = Some(ShareIndex::from_answers(
                &invites.map_err(unknown)?,
                &links.map_err(unknown)?,
                Utc::now(),
            ));
        }
        Ok(self
            .shares
            .as_ref()
            .map_or(Exposure::Unknown, |s| s.exposure(candidate)))
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

    #[test]
    fn only_folders_whose_sealed_name_is_byte_identical_are_candidates() {
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
        let found = same_name_folders(&rows, &master, wanted);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id.to_string(), mine["id"].as_str().unwrap());
        assert_eq!(found[0].created_at, at("2026-11-06T10:00:00Z"));
        assert_eq!(found[0].share_count, Some(2));
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
        let index = ShareIndex::from_answers(&invites, &links, now);
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

        // "Not supported" (404/405/501, read as Null) is unknown, never private: the row's share_count decides,
        // and with none the folder counts as shared.
        let unsupported = ShareIndex::from_answers(&Value::Null, &Value::Null, now);
        let mut row = candidate(1, None);
        row.share_count = None;
        assert_eq!(unsupported.exposure(&row), Exposure::Unknown);
        assert!(Exposure::Unknown.is_shared());
        assert_eq!(
            Exposure::Unknown.describe(),
            "may be shared: this server does not say with whom"
        );
        row.share_count = Some(0);
        assert!(!unsupported.exposure(&row).is_shared());
        row.share_count = Some(2);
        assert_eq!(unsupported.exposure(&row), Exposure::Known { people: 0, links: 2 });
        // One list supported, the other not: what is known counts, the row's share_count fills in.
        let invites_only = ShareIndex::from_answers(&invites, &Value::Null, now);
        row.share_count = Some(0);
        assert_eq!(invites_only.exposure(&row), Exposure::Known { people: 2, links: 0 });
        row.share_count = None;
        assert_eq!(invites_only.exposure(&row), Exposure::Unknown);

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
