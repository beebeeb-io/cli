//! L7d: the device's custody memory. The folders this device has ever opened under permanent (L6) custody.
//!
//! ONE monotonic set of folder ids per config dir, keyed by nothing the server reports. It holds no keys and
//! grants no D-L1 sync or cache authority. An entry is never removed: not by sign-out, sign-in, a switch to
//! another account, a 401 or whatever `/me` answers. Only deleting the config directory removes it. There is
//! deliberately no function here that deletes or rewrites an entry.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const RECORD: &str = "custody.json";

#[derive(Default, Serialize, Deserialize)]
struct Observations {
    custody_seen: BTreeSet<String>,
}

pub struct CustodyStore {
    directory: PathBuf,
}

// Next to `config.json`, wherever `config::config_path` puts it.
fn directory() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("beebeeb")
}

// One lock covers read/union/replace. The lock file is never replaced: all processes must lock the same inode.
fn locked<T>(directory: &Path, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::fs::create_dir_all(directory).map_err(|e| format!("create custody directory: {e}"))?;
    let lock = private_file()
        .create(true)
        .truncate(false)
        .open(directory.join(".custody.lock"))
        .map_err(|e| format!("open custody lock: {e}"))?;
    // `File::lock` is std since 1.89; same stance as `upload_lock`: every build pins 1.96.0.
    #[allow(clippy::incompatible_msrv)]
    let locked = lock.lock();
    locked.map_err(|e| format!("lock custody observations: {e}"))?;
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
        .map_err(|e| format!("sync custody directory: {e}"))?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}

/// Commit a same-directory temporary file, then make its name durable. A killed process may leave an unused
/// temporary file; it cannot truncate the record.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("missing custody directory")?;
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file()
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("create custody observations: {e}"))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("sync custody observations: {e}"))?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|e| format!("replace custody observations: {e}"))?;
        sync_directory(directory)?;
        // Also persist creation of the config directory itself.
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

/// One folder, one spelling: the lowercase hyphenated UUID, for what is stored and what is compared. A server
/// that later writes the same id as uppercase, braced, hyphenless or a URN must land on the same entry. An id
/// that is not a UUID cannot be compared at all, so it is an error and the callers refuse.
fn canonical(id: &str) -> Result<String, String> {
    Uuid::parse_str(id)
        .map(|uuid| uuid.hyphenated().to_string())
        .map_err(|_| "folder id is not a UUID".into())
}

impl CustodyStore {
    pub fn open() -> Result<Self, String> {
        Ok(Self { directory: directory() })
    }

    // Replacement is atomic, so a reader sees the old record or the new one and needs no lock. A record that
    // cannot be read is an error, never an empty set: the callers refuse.
    fn read(&self) -> Result<Observations, String> {
        match std::fs::read(self.directory.join(RECORD)) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "invalid custody observations".into()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Observations::default()),
            Err(e) => Err(format!("read custody observations: {e}")),
        }
    }

    pub fn seen(&self, id: &str) -> Result<bool, String> {
        let id = canonical(id)?;
        Ok(self.read()?.custody_seen.contains(&id))
    }

    /// Synchronous durability is intentional: no inventory/content await may happen between authenticating L6
    /// custody and committing this observation.
    pub fn remember(&self, id: &str) -> Result<(), String> {
        let id = canonical(id)?;
        locked(&self.directory, || {
            let mut record = self.read()?;
            if record.custody_seen.insert(id) {
                let bytes = serde_json::to_vec(&record).map_err(|e| format!("serialize custody observations: {e}"))?;
                atomic_write(&self.directory.join(RECORD), &bytes)?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ls`, `pull` and `sync` of one device run as separate processes and each read-union-writes the same
    /// record. Threads that each open the lock file themselves hold separate descriptions of it, which is
    /// what separate processes do, so a lost update here is a lost update there.
    #[test]
    fn concurrent_writers_never_lose_an_observation() {
        let dir = std::env::temp_dir().join(format!("bb-custody-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ids: Vec<String> = (0..160).map(|_| Uuid::new_v4().to_string()).collect();
        std::thread::scope(|scope| {
            for chunk in ids.chunks(20) {
                let directory = dir.clone();
                scope.spawn(move || {
                    let store = CustodyStore { directory };
                    for id in chunk {
                        store.remember(id).unwrap();
                    }
                });
            }
        });
        let store = CustodyStore { directory: dir.clone() };
        let lost = ids.iter().filter(|id| !store.seen(id).unwrap()).count();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            lost,
            0,
            "{lost} of {} observations were lost to a concurrent writer",
            ids.len()
        );
    }

    fn scratch() -> (PathBuf, CustodyStore) {
        let dir = std::env::temp_dir().join(format!("bb-custody-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), CustodyStore { directory: dir })
    }

    /// A server that later writes the same folder id in another form (uppercase, braced, no hyphens, URN) must
    /// not slip past the set: every form is one canonical lowercase hyphenated string, stored and compared.
    #[test]
    fn one_folder_is_one_entry_whatever_form_the_server_writes_its_id_in() {
        let (dir, store) = scratch();
        let id = "22222222-2222-4222-8222-22222222abcd";
        store.remember(id).unwrap();
        for form in [
            id.to_uppercase(),
            format!("{{{id}}}"),
            id.replace('-', ""),
            format!("urn:uuid:{id}"),
        ] {
            assert!(store.seen(&form).unwrap(), "{form} must be the remembered folder");
        }
        // Remembering another spelling adds nothing and writes the canonical form only.
        store.remember(&id.to_uppercase()).unwrap();
        let saved = std::fs::read_to_string(dir.join(RECORD)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(saved, format!(r#"{{"custody_seen":["{id}"]}}"#));
    }

    /// An id that is not a UUID cannot be compared, so it is an error for both calls (the resolver turns it into
    /// "This folder's keys are unavailable"), and nothing is written.
    #[test]
    fn an_id_that_is_not_a_uuid_is_refused_by_seen_and_remember() {
        let (dir, store) = scratch();
        for bad in [
            "",
            "not-a-uuid",
            "22222222-2222-4222-8222",
            "2222222222222222222222222222222g",
        ] {
            assert!(store.seen(bad).is_err(), "seen({bad:?}) must refuse");
            assert!(store.remember(bad).is_err(), "remember({bad:?}) must refuse");
        }
        let written = dir.join(RECORD).exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!written, "a refused id must not create the record");
    }
}
