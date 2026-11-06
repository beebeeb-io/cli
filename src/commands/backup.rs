//! `bb backup add|remove|list|run|status`: back up folders of this machine
//! into `Backups/<device>/<folder>` (LD-0113, spec Amendment 8). Built for a
//! headless Linux box: `bb backup run` is one pass, for a systemd timer or
//! cron (README "Backups").
//!
//! A folder whose backup was deleted elsewhere turns off and stays off (spec
//! §4.2 rule 11, Amendment 12): `run` never uploads it again by itself,
//! `status` says why and exits [`crate::exit::BACKUP_OFF`], and
//! `bb backup add <folder>` backs it up again, fresh.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use beebeeb_core::backup::{DeviceSelf, FolderState, category_folder_name, records_from_remote};
use beebeeb_types::backup::{Category, Platform};
use colored::Colorize;

use crate::api::ApiClient;
use crate::backup::host::{CliHost, Drained};
use crate::backup::remote::{self, DeviceFolder};
use crate::backup::state::{LastPass, Off, OffReason, Opened, Source, Store, state_dir};
use crate::backup::{PASS_BUDGET, identity, now_ms};
use crate::commands::push::load_master_key;
use crate::{colors, ui};

/// The signed-in account's id (`GET /auth/me` `user_id`): the store belongs to it.
async fn account_id(api: &ApiClient) -> Result<String, String> {
    api.require_auth()?;
    let me = api.get_me().await?;
    me.get("user_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "the server did not say which account this is".to_string())
}

/// One `bb backup` at a time: held for the command, released by the OS when
/// the process ends (also on a crash).
fn lock(dir: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create the state folder: {e}"))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("backup.lock"))
        .map_err(|e| format!("cannot open the backup lock: {e}"))?;
    // `File::try_lock` is std since 1.89 (the toolchain pins 1.96); `.clippy.toml` still says 1.85, as for
    // `upload_lock`.
    #[allow(clippy::incompatible_msrv)]
    let locked = file.try_lock();
    match locked {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(crate::exit::with_code(
            crate::exit::BUSY,
            "another `bb backup` is running on this machine; run this again when it has finished",
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot take the backup lock: {e}")),
    }
}

fn open_mine(dir: &Path, account: &str) -> Result<Store, String> {
    match Store::open(dir, account)? {
        Opened::Mine(store) => Ok(store),
        Opened::OtherAccount => Err(format!(
            "the backup settings on this machine belong to another Beebeeb account. Sign in to that account, \
                 or delete {} to start over for this one.",
            dir.join("backup.json").display()
        )),
    }
}

fn category_of(source: &Source) -> Result<Category, String> {
    Category::from_key(&source.category).ok_or_else(|| format!("unknown backup folder kind: {}", source.category))
}

fn canonical(folder: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(folder).map_err(|e| format!("{}: {e}", folder.display()))
}

/// The path `folder` was added as, for `remove`: canonical while it exists; a folder gone locally is matched
/// by its absolute path (a relative one never equals the stored canonical path).
fn added_path(folder: &Path) -> PathBuf {
    canonical(folder).unwrap_or_else(|_| std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf()))
}

// ── add / remove / list ─────────────────────────────────────────────────────

pub async fn add(folder: PathBuf) -> Result<(), String> {
    let path = canonical(&folder)?;
    if !path.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    identity::check_source(&path, &identity::excluded_dirs())?;
    let api = ApiClient::from_config();
    let account = account_id(&api).await?;
    let dir = state_dir();
    let _lock = lock(&dir)?;
    let mut store = open_mine(&dir, &account)?;

    if let Some(i) = store.settings.sources.iter().position(|s| s.path == path) {
        let Some(off) = store.settings.sources[i].off.clone() else {
            return Err(format!("{} is already backed up", path.display()));
        };
        // "Back up again" (spec §4.2 rule 11): a first backup, from nothing.
        let records_file = store.fresh_records_file()?;
        store.forget_records(store.settings.sources[i].records_file);
        let source = &mut store.settings.sources[i];
        source.records_file = records_file;
        source.category_folder_id = None;
        source.off = None;
        source.last_pass = None;
        if off.reason == OffReason::DeviceDeleted {
            store.settings.device_folder_id = None;
            store.settings.manifest_folders.clear();
        }
        store.save()?;
        say(&format!(
            "backing up {} again, as a first backup \u{00b7} run `bb backup run` to start",
            path.display()
        ));
        return Ok(());
    }
    if let Some(other) = store
        .settings
        .sources
        .iter()
        .find(|s| path.starts_with(&s.path) || s.path.starts_with(&path))
    {
        return Err(format!(
            "{} overlaps {}, which is already backed up: one folder can be in one backup only",
            path.display(),
            other.path.display()
        ));
    }
    let customs: Vec<String> = store
        .settings
        .sources
        .iter()
        .filter_map(|s| match Category::from_key(&s.category) {
            Some(Category::Custom(name)) => Some(name),
            _ => None,
        })
        .collect();
    let taken: Vec<&str> = customs.iter().map(String::as_str).collect();
    let mut category = identity::category_for(&path, &taken);
    if !matches!(category, Category::Custom(_)) && store.settings.sources.iter().any(|s| s.category == category.key()) {
        let raw = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        category = Category::Custom(beebeeb_core::backup::custom_category_name(&raw, &taken));
    }
    if store
        .settings
        .sources
        .iter()
        .any(|s| s.off.as_ref().is_some_and(|o| o.reason == OffReason::DeviceDeleted))
    {
        // The device folder is gone: a new folder is backed up into a new one.
        store.settings.device_folder_id = None;
        store.settings.manifest_folders.clear();
    }
    let records_file = store.fresh_records_file()?;
    store.settings.sources.push(Source {
        path: path.clone(),
        category: category.key(),
        records_file,
        category_folder_id: None,
        off: None,
        last_pass: None,
    });
    store.save()?;
    if ui::is_json() {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "path": path, "folder": category_folder_name(&category) })
        );
    } else {
        say(&format!(
            "added {} as Backups/<this machine>/{} \u{00b7} run `bb backup run` to back it up now",
            path.display(),
            category_folder_name(&category)
        ));
    }
    Ok(())
}

pub async fn remove(folder: PathBuf) -> Result<(), String> {
    let api = ApiClient::from_config();
    let account = account_id(&api).await?;
    let dir = state_dir();
    let _lock = lock(&dir)?;
    let mut store = open_mine(&dir, &account)?;
    let path = added_path(&folder);
    let Some(i) = store.settings.sources.iter().position(|s| s.path == path) else {
        return Err(format!("{} is not backed up (see `bb backup list`)", path.display()));
    };
    let source = store.settings.sources.remove(i);
    store.forget_records(source.records_file);
    store.save()?;
    say(&format!(
        "stopped backing up {} \u{00b7} what is already in Backups stays there",
        path.display()
    ));
    Ok(())
}

pub async fn list() -> Result<(), String> {
    let api = ApiClient::from_config();
    let account = account_id(&api).await?;
    let store = open_mine(&state_dir(), &account)?;
    if ui::is_json() {
        let rows: Vec<serde_json::Value> = store
            .settings
            .sources
            .iter()
            .map(|s| {
                serde_json::json!({
                    "path": s.path,
                    "folder": category_of(s).map(|c| category_folder_name(&c)).unwrap_or_default(),
                    "on": s.off.is_none(),
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "folders": rows }));
        return Ok(());
    }
    if store.settings.sources.is_empty() {
        say("no folders are backed up \u{00b7} add one with `bb backup add <folder>`");
        return Ok(());
    }
    for s in &store.settings.sources {
        let folder = category_of(s).map(|c| category_folder_name(&c)).unwrap_or_default();
        let state = if s.off.is_some() { "off" } else { "on" };
        println!(
            "  {:<20} {:<4} {}",
            folder.custom_color(colors::INK),
            state.custom_color(colors::INK_DIM),
            s.path.display().to_string().custom_color(colors::PATH),
        );
    }
    Ok(())
}

// ── run ─────────────────────────────────────────────────────────────────────

/// The seam for core's `backup_stop` (LD-0121, core v0.2.2; spec Amendment
/// 12). Until it exists the CLI knows one reason, the folder gone from the
/// listing (rule 11), and says rule 11's words. `backup_stop` adds the
/// account-level reasons (files erased, account lapsed, reset, inactive
/// cleanup) from the server's notices, and the `unknown` words.
fn stop_words(device: &str, folder: &str, _reason: OffReason) -> String {
    format!("{device}'s backup of {folder} was deleted from another device.")
}

/// How old `.device.json` may get before a run writes it again, so other
/// devices see the last run (one small version a day, not one per run).
const MANIFEST_REFRESH_MS: i64 = 24 * 60 * 60 * 1000;

/// What `run` did for one folder.
struct FolderRun {
    folder: String,
    last: LastPass,
    turned_off: Option<OffReason>,
    error: Option<String>,
}

pub async fn run() -> Result<(), String> {
    let api = ApiClient::for_backup();
    let account = account_id(&api).await?;
    let master_key = Arc::new(load_master_key()?);
    let dir = state_dir();
    let _lock = lock(&dir)?;
    let mut store = open_mine(&dir, &account)?;
    if store.settings.sources.iter().all(|s| s.off.is_some()) {
        if store.settings.sources.is_empty() {
            say("nothing to back up yet \u{00b7} add a folder with `bb backup add <folder>`");
            return Ok(());
        }
        return status_of(&store);
    }
    let me = DeviceSelf {
        device_id: identity::device_id()?,
        name: identity::device_name(),
        platform: Platform::Cli,
    };
    let root = remote::backups_root(&api, &master_key).await?;
    let (device, fresh) = remote::device_folder(&api, &master_key, root, &me, store.settings.device_folder_id).await?;
    let device_id = match device {
        DeviceFolder::Live(id) => {
            store.settings.device_folder_id = Some(id);
            Some(id)
        }
        DeviceFolder::Deleted => None,
    };
    store.save()?;
    let excluded = identity::excluded_dirs();

    let mut runs = Vec::new();
    for i in 0..store.settings.sources.len() {
        if store.settings.sources[i].off.is_some() {
            continue;
        }
        let run = run_folder(&api, &master_key, &store, i, device_id, &excluded).await;
        let source = &mut store.settings.sources[i];
        if let Ok((run, category_folder_id)) = &run {
            source.category_folder_id = *category_folder_id;
            source.last_pass = Some(run.last.clone());
            if let Some(reason) = run.turned_off {
                source.off = Some(Off {
                    reason,
                    at_ms: now_ms(),
                });
            }
        }
        store.save()?;
        runs.push(run.map(|(r, _)| r).unwrap_or_else(|e| {
            FolderRun {
                folder: category_of(&store.settings.sources[i])
                    .map(|c| category_folder_name(&c))
                    .unwrap_or_default(),
                last: LastPass::default(),
                turned_off: None,
                error: Some(e),
            }
        }));
    }

    if let Some(device) = device_id {
        let folders: Vec<FolderState> = store
            .settings
            .sources
            .iter()
            .map(|s| FolderState {
                category: s.category.clone(),
                enabled: s.off.is_none(),
                last_pass_at_ms: s.last_pass.as_ref().map(|p| p.at_ms),
                items: store.records(s.records_file).map_or(0, |r| r.backed_up() as u64),
                other: serde_json::Map::new(),
            })
            .collect();
        let listed: Vec<String> = folders
            .iter()
            .map(|f| format!("{}:{}", f.category, f.enabled))
            .collect();
        let stale = store
            .settings
            .manifest_written_ms
            .is_none_or(|at| now_ms() - at > MANIFEST_REFRESH_MS);
        if fresh || stale || listed != store.settings.manifest_folders {
            remote::write_manifest(&api, &master_key, device, &me, folders).await?;
            store.settings.manifest_folders = listed;
            store.settings.manifest_written_ms = Some(now_ms());
            store.save()?;
        }
    }
    report(&me.name, &runs)
}

/// One folder's pass, then its uploads. Returns the category folder id to keep.
async fn run_folder(
    api: &ApiClient,
    master_key: &Arc<beebeeb_core::kdf::MasterKey>,
    store: &Store,
    i: usize,
    device: Option<uuid::Uuid>,
    excluded: &[PathBuf],
) -> Result<(FolderRun, Option<uuid::Uuid>), String> {
    let source = &store.settings.sources[i];
    let category = category_of(source)?;
    let folder = category_folder_name(&category);
    let (presence, category_id) = match device {
        Some(device) => {
            let (live, id) =
                remote::category_folder(api, master_key, device, &folder, source.category_folder_id).await?;
            ((true, live), id)
        }
        None => ((false, None), None),
    };
    let tree = match category_id {
        Some(id) => Some(remote::tree(api, master_key, id).await?),
        None => None,
    };
    let mut records = store.records(source.records_file)?;
    // Records lost (a reinstall): core's rebuild rule marks what the server
    // already holds, so nothing is uploaded twice (spec §4.2 rule 10).
    if records.len() == 0
        && let Some(tree) = tree.as_ref().filter(|t| !t.files.is_empty())
    {
        let walk = beebeeb_backup::walk_source(&source.path, excluded)
            .map_err(|e| format!("cannot read {} ({:?})", folder, e.kind()))?;
        for record in records_from_remote(&tree.files, &walk.entries) {
            records.put(record)?;
        }
    }
    let host = CliHost::new(api, Arc::clone(master_key), category.clone(), presence, tree, records);
    let pass = beebeeb_backup::run_pass(&host, &source.path, excluded, &category, PASS_BUDGET)
        .await
        .map_err(|e| e.0)?;
    let queued = host.queued();
    let drained: Drained = host.drain().await;
    let turned_off = host.turned_off().and_then(OffReason::from_presence);
    host.into_records().compact()?;
    let last = LastPass {
        at_ms: now_ms(),
        uploaded: drained.uploaded,
        failed: drained.failed,
        moved: pass.moved,
        unchanged: pass.unchanged,
        deferred: pass.deferred + pass.deferred_unknown + drained.changed_meanwhile + pass.vanished,
        skipped: pass.skipped.values().sum(),
        gone_locally: pass.gone_locally,
    };
    debug_assert_eq!(queued, drained.uploaded + drained.failed + drained.changed_meanwhile);
    Ok((
        FolderRun {
            folder,
            last,
            turned_off,
            error: None,
        },
        category_id,
    ))
}

fn counts(last: &LastPass) -> String {
    format!(
        "{} uploaded \u{00b7} {} failed \u{00b7} {} moved \u{00b7} {} unchanged \u{00b7} {} skipped \u{00b7} {} waiting for the next run",
        last.uploaded, last.failed, last.moved, last.unchanged, last.skipped, last.deferred
    )
}

fn report(device: &str, runs: &[FolderRun]) -> Result<(), String> {
    let off = runs.iter().any(|r| r.turned_off.is_some());
    let failed = runs.iter().any(|r| r.error.is_some() || r.last.failed > 0);
    if ui::is_json() {
        let rows: Vec<serde_json::Value> = runs
            .iter()
            .map(|r| {
                serde_json::json!({
                    "folder": r.folder,
                    "uploaded": r.last.uploaded,
                    "failed": r.last.failed,
                    "moved": r.last.moved,
                    "unchanged": r.last.unchanged,
                    "skipped": r.last.skipped,
                    "waiting": r.last.deferred,
                    "off": r.turned_off.map(|why| stop_words(device, &r.folder, why)),
                    "error": r.error,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "ok": !off && !failed, "folders": rows }));
    } else if !ui::is_quiet() {
        for r in runs {
            if let Some(why) = r.turned_off {
                println!(
                    "  {} {} {}",
                    "off \u{00b7}".custom_color(colors::RED_ERR),
                    stop_words(device, &r.folder, why).custom_color(colors::INK),
                    "Back up again with `bb backup add <folder>`.".custom_color(colors::INK_DIM),
                );
            } else if let Some(e) = &r.error {
                println!("  {} {}: {e}", "error \u{00b7}".custom_color(colors::RED_ERR), r.folder);
            } else {
                println!(
                    "  {} {:<20} {}",
                    "ok \u{00b7}".custom_color(colors::GREEN_OK),
                    r.folder.custom_color(colors::INK),
                    counts(&r.last).custom_color(colors::INK_DIM),
                );
            }
        }
    }
    if off {
        return Err(crate::exit::with_code(
            crate::exit::BACKUP_OFF,
            "a backup is off: its folder was deleted (see `bb backup status`)",
        ));
    }
    if failed {
        return Err(crate::exit::with_code(
            crate::exit::INCOMPLETE,
            "some files could not be backed up; they are tried again on the next run",
        ));
    }
    Ok(())
}

// ── status ──────────────────────────────────────────────────────────────────

pub async fn status() -> Result<(), String> {
    let api = ApiClient::from_config();
    let account = account_id(&api).await?;
    let store = open_mine(&state_dir(), &account)?;
    status_of(&store)
}

fn status_of(store: &Store) -> Result<(), String> {
    let device = identity::device_name();
    let mut any_off = false;
    let mut rows = Vec::new();
    for s in &store.settings.sources {
        let folder = category_of(s).map(|c| category_folder_name(&c)).unwrap_or_default();
        let backed_up = store.records(s.records_file).map(|r| r.backed_up()).unwrap_or(0);
        let off = s.off.as_ref().map(|o| stop_words(&device, &folder, o.reason));
        any_off |= off.is_some();
        rows.push((s, folder, backed_up, off));
    }
    if ui::is_json() {
        let rows: Vec<serde_json::Value> = rows
            .iter()
            .map(|(s, folder, backed_up, off)| {
                serde_json::json!({
                    "path": s.path,
                    "folder": folder,
                    "on": off.is_none(),
                    "off": off,
                    "backed_up": backed_up,
                    "last_run": s.last_pass,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "folders": rows }));
    } else if !ui::is_quiet() {
        if rows.is_empty() {
            say("no folders are backed up \u{00b7} add one with `bb backup add <folder>`");
        }
        for (s, folder, backed_up, off) in &rows {
            println!(
                "  {} {}",
                folder.custom_color(colors::INK),
                s.path.display().to_string().custom_color(colors::PATH)
            );
            match off {
                Some(words) => println!(
                    "    {} {} {}",
                    "off \u{00b7}".custom_color(colors::RED_ERR),
                    words.custom_color(colors::INK),
                    "Back up again with `bb backup add <folder>`.".custom_color(colors::INK_DIM),
                ),
                None => println!("    {}", "on".custom_color(colors::GREEN_OK)),
            }
            println!(
                "    {}",
                format!("{backed_up} files backed up").custom_color(colors::INK_DIM)
            );
            match &s.last_pass {
                Some(last) => {
                    let when = chrono::DateTime::from_timestamp_millis(last.at_ms)
                        .map(|t| ui::relative_time(&t.to_rfc3339()))
                        .unwrap_or_default();
                    println!(
                        "    {}",
                        format!("last run {when}: {}", counts(last)).custom_color(colors::INK_DIM)
                    );
                }
                None => println!("    {}", "not run yet".custom_color(colors::INK_DIM)),
            }
        }
    }
    if any_off {
        return Err(crate::exit::with_code(
            crate::exit::BACKUP_OFF,
            "a backup is off: its folder was deleted. Back up again with `bb backup add <folder>`.",
        ));
    }
    Ok(())
}

fn say(line: &str) {
    if !ui::is_quiet() && !ui::is_json() {
        println!(
            "  {} {}",
            "ok \u{00b7}".custom_color(colors::GREEN_OK),
            line.custom_color(colors::INK)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_gone_locally_is_matched_by_its_absolute_path() {
        let gone = PathBuf::from(format!("cli-backup-gone-{}", uuid::Uuid::new_v4()));
        assert_eq!(added_path(&gone), std::env::current_dir().unwrap().join(&gone));
    }
}
