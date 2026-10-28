//! Bounded index deltas. Shared pages use CAS; ordinary owner pages use LWW.
use crate::{api::ApiClient, owned_keys::OwnedKeyResolver};
use beebeeb_core::{
    kdf::MasterKey,
    search_index::{EncryptedShard, SearchIndex, ShardKeySource},
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
#[derive(Default)]
pub struct IndexRepairReport {
    pub written: usize,
    pub deferred: usize,
    pub partial: bool,
}
type Pages = BTreeMap<(u32, u32), u64>;
async fn pages(api: &ApiClient, base: &str) -> Result<Pages, String> {
    let doc = api.get_json(base).await?;
    let rows = doc["shards"].as_array().ok_or("invalid index listing")?;
    if rows.len() > 64 * 16 {
        return Err("index page limit".into());
    }
    let mut pages = Pages::new();
    for r in rows {
        let b = r["bucket"].as_u64().ok_or("missing bucket")?;
        let p = r["page"].as_u64().ok_or("missing page")?;
        let v = r["version"].as_u64().filter(|v| *v > 0).ok_or("missing version")?;
        if b >= 64 || p >= 16 || pages.insert((b as u32, p as u32), v).is_some() {
            return Err("invalid index address".into());
        }
    }
    Ok(pages)
}
async fn read(api: &ApiClient, base: &str, pages: &Pages, source: ShardKeySource<'_>) -> Result<SearchIndex, String> {
    let mut shards = Vec::new();
    for &(bucket, page) in pages.keys() {
        if page > 0 && !pages.contains_key(&(bucket, page - 1)) {
            return Err("missing index page".into());
        }
        shards.push(EncryptedShard {
            bucket,
            page,
            blob: api.index_blob(&format!("{base}/{bucket}/{page}")).await?,
        });
    }
    SearchIndex::from_encrypted_shards_with(source, &shards, 64).map_err(|e| e.to_string())
}
async fn write_deltas(
    api: &ApiClient,
    base: &str,
    source: ShardKeySource<'_>,
    entries: &[(String, String)],
    removed: &[String],
    shared: bool,
) -> Result<IndexRepairReport, String> {
    let mut report = IndexRepairReport::default();
    for _ in 0..if shared { 3 } else { 1 } {
        let old = pages(api, base).await?;
        let (mut index, mut dirty) = match read(api, base, &old, source).await {
            Ok(index) => (index, BTreeSet::new()),
            // Only shared repair has a fresh complete listing. An unreadable
            // owner page is deferred, never replaced from this folder's subset.
            Err(_) if shared => (SearchIndex::new(64), (0..64).collect()),
            Err(e) => return Err(e),
        };
        for (id, name) in entries {
            dirty.extend(index.upsert(id, name));
        }
        for id in removed {
            dirty.extend(index.remove(id));
        }
        let encrypted = index.encrypt_buckets_with(source, &dirty).map_err(|e| e.to_string())?;
        let mut next = BTreeMap::new();
        for sh in encrypted {
            if sh.bucket >= 64 || sh.page >= 16 || sh.blob.len() > 160 * 1024 {
                return Err("index page limit".into());
            }
            next.insert((sh.bucket, sh.page), sh.blob);
        }
        let mut conflict = false;
        for (&(b, p), blob) in &next {
            let version = shared.then_some(old.get(&(b, p)).copied().unwrap_or(0));
            if !api
                .write_index_page(&format!("{base}/{b}/{p}"), Some(blob.clone()), version)
                .await?
            {
                conflict = true;
                break;
            }
            report.written += 1;
        }
        if !conflict {
            for (&(b, p), &v) in &old {
                if dirty.contains(&b) && !next.contains_key(&(b, p)) {
                    if !api
                        .write_index_page(&format!("{base}/{b}/{p}"), None, shared.then_some(v))
                        .await?
                    {
                        conflict = true;
                        break;
                    }
                    report.written += 1;
                }
            }
        }
        if !conflict {
            return Ok(report);
        }
    }
    report.deferred += 1;
    Ok(report)
}
pub async fn update_owner_index(
    api: &ApiClient,
    master: &MasterKey,
    entries: &[(String, String)],
    removed_ids: &[String],
) -> Result<IndexRepairReport, String> {
    write_deltas(
        api,
        "/api/v1/search-index/shards",
        ShardKeySource::Master(master),
        entries,
        removed_ids,
        false,
    )
    .await
}
pub async fn repair(
    api: &ApiClient,
    master: &MasterKey,
    keys: &mut OwnedKeyResolver,
    folder_id: &str,
) -> Result<IndexRepairReport, String> {
    let Some(key) = keys.folder_key(api, master, folder_id).await? else {
        return Ok(IndexRepairReport::default());
    };
    let previous = keys.descendant_ids(folder_id);
    let mut current = HashSet::new();
    let mut queue = vec![folder_id.to_string()];
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    let mut partial = false;
    let mut count = 0;
    while let Some(id) = queue.pop() {
        if !seen.insert(id.clone()) {
            partial = true;
            continue;
        }
        if count >= 20_000 {
            partial = true;
            break;
        }
        let listing = api.list_files_bounded(&id, 20_000 - count).await?;
        partial |= crate::sync_safety::listing_truncated(&listing);
        let rows = listing["files"].as_array().ok_or("invalid repair listing")?;
        keys.seed(rows);
        for row in rows {
            count += 1;
            if count > 20_000 {
                partial = true;
                break;
            }
            match keys.resolve(api, master, row).await {
                Ok(r) => {
                    let id = r.canonical["id"].as_str().ok_or("missing item id")?.to_string();
                    if !current.insert(id.clone()) {
                        partial = true;
                        continue;
                    }
                    if r.canonical["is_folder"] == true {
                        queue.push(id.clone());
                    }
                    entries.push((id, r.name));
                }
                Err(_) => partial = true,
            }
        }
        if count > 20_000 {
            break;
        }
    }
    if partial {
        return Ok(IndexRepairReport {
            partial: true,
            deferred: 1,
            written: 0,
        });
    }
    let mut removed = Vec::new();
    for id in previous.difference(&current) {
        let Ok(row) = api.get_file(id).await else {
            continue;
        };
        if row["id"].as_str() != Some(id) {
            continue;
        }
        if row["is_trashed"] == true {
            removed.push(id.clone());
            continue;
        }
        let mut row = row;
        let mut parents = HashSet::new();
        loop {
            match row["parent_id"].as_str() {
                None => {
                    removed.push(id.clone());
                    break;
                }
                Some(parent) if parent == folder_id => break,
                Some(parent) => {
                    if !parents.insert(parent.to_string()) || parents.len() > 256 {
                        break;
                    }
                    let Ok(next) = api.get_file(parent).await else {
                        break;
                    };
                    if next["id"].as_str() != Some(parent) {
                        break;
                    }
                    row = next;
                }
            }
        }
    }
    write_deltas(
        api,
        &format!("/api/v1/shared-folders/{folder_id}/search-index/shards"),
        ShardKeySource::Folder(&key),
        &entries,
        &removed,
        true,
    )
    .await
}
