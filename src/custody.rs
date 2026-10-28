//! L7c security observations shared by all commands and sync roots.
//! These contain no keys and do not grant D-L1 sync/cache authority.
use crate::api::ApiClient;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
struct Observations {
    account_id: Uuid,
    custody_seen: BTreeSet<String>,
}

pub struct CustodyStore {
    account: Uuid,
    directory: PathBuf,
}

fn directory() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|p| p.join("beebeeb/accounts"))
        .ok_or_else(|| "cannot locate account security state".into())
}

// One lock covers binding changes, removal and read/union/replace. Lock files
// are never replaced: all processes must lock the same inode.
fn locked<T>(directory: &Path, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::fs::create_dir_all(directory).map_err(|e| format!("create account state: {e}"))?;
    let lock = private_file()
        .create(true)
        .truncate(false)
        .open(directory.join(".lock"))
        .map_err(|e| format!("open account state lock: {e}"))?;
    // `File::lock` is std since 1.89; same stance as `upload_lock`: every build pins 1.96.0.
    #[allow(clippy::incompatible_msrv)]
    let locked = lock.lock();
    locked.map_err(|e| format!("lock account state: {e}"))?;
    operation()
}

fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn sync_directory(directory: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|e| format!("sync account directory: {e}"))?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

/// Commit a same-directory temporary file, then make its name durable. A killed
/// process may leave an unused temporary file; it cannot truncate the record.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("missing account directory")?;
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file()
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("create security state: {e}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("sync security state: {e}"))?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|e| format!("replace security state: {e}"))?;
        sync_directory(directory)?;
        // Also persist creation of the accounts directory itself.
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn active(directory: &Path) -> Result<Option<Uuid>, String> {
    match std::fs::read(directory.join("active.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "invalid account security binding".into()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read account security binding: {e}")),
    }
}

fn remove_account(directory: &Path, account: Uuid) -> Result<(), String> {
    match std::fs::remove_file(directory.join(format!("{account}.json"))) {
        Ok(()) => sync_directory(directory),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clear account security state: {e}")),
    }
}

/// Called even when logout cannot reach the server. The local binding identifies
/// exactly the account to clear; other accounts' records are never consulted.
pub fn clear_current() -> Result<(), String> {
    let directory = directory()?;
    if !directory.exists() {
        return Ok(());
    }
    locked(&directory, || {
        // Every other reader fails closed on a damaged store, so this call is how it gets repaired: an
        // unreadable binding is dropped as well. It cannot say whose record it named; that record stays
        // and is only read again if the same account signs back in, which can only make a check stricter.
        if let Some(account) = active(&directory).ok().flatten() {
            remove_account(&directory, account)?;
        }
        match std::fs::remove_file(directory.join("active.json")) {
            Ok(()) => sync_directory(&directory),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("clear account binding: {e}")),
        }
    })
}

impl CustodyStore {
    pub async fn open(api: &ApiClient) -> Result<Self, String> {
        let me = api.get_me().await?;
        let account = me["id"]
            .as_str()
            .ok_or("session response missing user id")?
            .parse::<Uuid>()
            .map_err(|_| "session response has invalid user id")?;
        let store = Self {
            account,
            directory: directory()?,
        };
        locked(&store.directory, || {
            let previous = active(&store.directory)?;
            if previous != Some(account) {
                if let Some(previous) = previous {
                    remove_account(&store.directory, previous)?;
                }
                atomic_write(
                    &store.directory.join("active.json"),
                    &serde_json::to_vec(&account).unwrap(),
                )?;
            }
            Ok(())
        })?;
        Ok(store)
    }

    fn read(&self) -> Result<Observations, String> {
        if active(&self.directory)? != Some(self.account) {
            return Err("account security binding changed; retry the command".into());
        }
        match std::fs::read(self.directory.join(format!("{}.json", self.account))) {
            Ok(bytes) => {
                let record: Observations =
                    serde_json::from_slice(&bytes).map_err(|_| "invalid custody observations")?;
                if record.account_id != self.account {
                    return Err("custody observations belong to another account".into());
                }
                Ok(record)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Observations {
                account_id: self.account,
                custody_seen: BTreeSet::new(),
            }),
            Err(e) => Err(format!("read custody observations: {e}")),
        }
    }

    pub fn seen(&self, id: &str) -> Result<bool, String> {
        locked(&self.directory, || Ok(self.read()?.custody_seen.contains(id)))
    }

    /// Synchronous durability is intentional: no inventory/content await may
    /// happen between authenticating L6 custody and committing this observation.
    pub fn remember(&self, id: &str) -> Result<(), String> {
        locked(&self.directory, || {
            let mut record = self.read()?;
            if record.custody_seen.insert(id.into()) {
                let bytes = serde_json::to_vec(&record).map_err(|e| format!("serialize custody observations: {e}"))?;
                atomic_write(&self.directory.join(format!("{}.json", self.account)), &bytes)?;
            }
            Ok(())
        })
    }
}
