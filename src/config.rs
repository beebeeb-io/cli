//! `config.json` and the sign-in it points to (LD-0029).
//!
//! `config.json` holds only `api_url`, `email` and the `credential_store` marker. The master key and the session
//! token live in the OS keyring, or, on the explicit opt-in, in `credentials.json` (`credentials`). Every file here
//! is written through `private_file`: atomically, never readable by anyone else at any moment.
//!
//! An older install's `config.json` still holds both secrets in plaintext. The first run of this version stores
//! them in the keyring, reads them back, compares, and only then rewrites `config.json` without them (F5).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use zeroize::Zeroizing;

use crate::credentials::{self, CREDENTIALS_FILE, CredentialStore, Notice, SecretBackend, Secrets};
use crate::private_file;

const DEFAULT_API_URL: &str = "https://api.beebeeb.io";
const CONFIG_FILE: &str = "config.json";
static API_URL_OVERRIDE: OnceLock<String> = OnceLock::new();

/// What one command knows about the sign-in: the settings from `config.json` and, when signed in, the secrets from
/// wherever the marker says they are.
#[derive(Clone)]
pub struct Config {
    pub api_url: String,
    /// Wiped from memory on drop, like the key.
    pub session_token: Option<Zeroizing<String>>,
    pub email: Option<String>,
    /// Base64-encoded 32-byte master key derived during login. Used to derive per-file encryption keys for
    /// push/pull. Wiped from memory on drop.
    pub master_key: Option<Zeroizing<String>>,
    /// Where the secrets were read from; `None` when there are none.
    pub secrets_location: Option<SecretsLocation>,
}

/// Where a command found the secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretsLocation {
    /// The OS keyring entry for this account.
    Keyring,
    /// The opt-in `credentials.json` (unencrypted).
    File(PathBuf),
    /// An older install's `config.json` that could not be migrated yet (unencrypted).
    LegacyConfig(PathBuf),
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            session_token: None,
            email: None,
            master_key: None,
            secrets_location: None,
        }
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("api_url", &self.api_url)
            .field("email", &self.email)
            .field("session_token", &self.session_token.as_ref().map(|_| "<hidden>"))
            .field("master_key", &self.master_key.as_ref().map(|_| "<hidden>"))
            .field("secrets_location", &self.secrets_location)
            .finish()
    }
}

impl Config {
    fn with_secrets(mut self, secrets: Secrets, location: SecretsLocation) -> Self {
        self.session_token = secrets.session_token;
        self.master_key = secrets.master_key;
        self.secrets_location = Some(location);
        self
    }
}

/// `config.json` as it is on disk. The two secret fields are READ from an older install's file and never written:
/// `skip_serializing` means no code path can put a secret back into this file.
#[derive(Clone, Serialize, Deserialize)]
struct ConfigFile {
    api_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_store: Option<CredentialStore>,
    #[serde(default, skip_serializing)]
    session_token: Option<Zeroizing<String>>,
    #[serde(default, skip_serializing)]
    master_key: Option<Zeroizing<String>>,
}

impl ConfigFile {
    fn account(&self) -> String {
        credentials::account(&self.api_url, self.email.as_deref())
    }

    fn legacy_secrets(&self) -> Option<Secrets> {
        let secrets = Secrets {
            master_key: self.master_key.clone(),
            session_token: self.session_token.clone(),
        };
        (!secrets.is_empty()).then_some(secrets)
    }

    /// The same settings, with the secrets in `store` (and none in the file).
    fn pointing_at(&self, store: CredentialStore) -> Self {
        Self {
            api_url: self.api_url.clone(),
            email: self.email.clone(),
            credential_store: Some(store),
            session_token: None,
            master_key: None,
        }
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("beebeeb")
}

/// `config.json` (macOS `~/Library/Application Support/beebeeb`, Linux `~/.config/beebeeb`).
pub fn config_path() -> PathBuf {
    config_dir().join(CONFIG_FILE)
}

/// The file, or `None` when there is none or it does not parse (then the command is signed out, as before).
fn read_file(dir: &Path) -> Option<ConfigFile> {
    let bytes = private_file::read(&dir.join(CONFIG_FILE)).ok()??;
    serde_json::from_slice(&bytes).ok()
}

fn write_file(dir: &Path, file: &ConfigFile) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(file).map_err(|e| format!("failed to serialize config: {e}"))?;
    private_file::write(&dir.join(CONFIG_FILE), &json)
}

/// Identifies one version of `config.json`. Every `bb login` replaces the file (a new inode, a new mtime) and
/// `bb logout` removes it, so a keyring answer cached for one stamp is never used for another sign-in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Stamp(Option<std::time::SystemTime>, u64, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let inode = 0;
    Some(Stamp(meta.modified().ok(), meta.len(), inode))
}

type CachedRead = (String, Stamp, Result<Option<Secrets>, String>);

/// One keyring read per sign-in per process: `load_config` runs many times per command (every `ApiClient`), and a
/// Secret Service read is a D-Bus round trip. Keyed by the account and the file's [`Stamp`].
#[derive(Default)]
pub(crate) struct KeyringCache(Mutex<Option<CachedRead>>);

impl KeyringCache {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    fn read(
        &self,
        backend: &dyn SecretBackend,
        account: &str,
        stamp: Option<Stamp>,
    ) -> Result<Option<Secrets>, String> {
        let mut cached = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some((cached_account, cached_stamp, result)), Some(stamp)) = (cached.as_ref(), stamp.as_ref()) {
            if cached_account == account && cached_stamp == stamp {
                return result.clone();
            }
        }
        let result = backend
            .get(account)
            .and_then(|bytes| bytes.map(|b| credentials::decode(&b)).transpose());
        // Only a sign-in that was read is kept: a locked keyring or a missing entry is asked again next time, so a
        // long-running process (`bb sync --daemon`, `bb webdav`) recovers once the keyring is unlocked.
        match (&result, stamp) {
            (Ok(Some(_)), Some(stamp)) => *cached = Some((account.to_string(), stamp, result.clone())),
            _ => *cached = None,
        }
        result
    }
}

/// A legacy `config.json` that could not be migrated is not retried for the same file in the same process.
#[derive(Default)]
pub(crate) struct Declined(Mutex<Option<Stamp>>);

impl Declined {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }
}

/// What [`load_from`] found, and what the person should be told about it.
pub(crate) struct Loaded {
    pub config: Config,
    pub notices: Vec<Notice>,
}

/// The sign-in in `dir`: the settings from `config.json`, and the secrets from where its marker points. An older
/// `config.json` with the secrets in it is migrated first when `migrate` is set (F5).
pub(crate) fn load_from(
    dir: &Path,
    backend: &dyn SecretBackend,
    opt_in: bool,
    migrate: bool,
    cache: &KeyringCache,
    declined: &Declined,
) -> Loaded {
    let config_path = dir.join(CONFIG_FILE);
    let file_stamp = stamp(&config_path);
    let mut notices = Vec::new();
    let Some(file) = read_file(dir) else {
        return Loaded {
            config: Config::default(),
            notices,
        };
    };
    let config = Config {
        api_url: file.api_url.clone(),
        email: file.email.clone(),
        ..Config::default()
    };

    if let Some(legacy) = file.legacy_secrets() {
        let already_declined = {
            let declined = declined.0.lock().unwrap_or_else(|e| e.into_inner());
            declined.is_some() && *declined == file_stamp
        };
        let location = if migrate && !already_declined {
            // A symlinked config.json (dotfiles): the rewrite below replaces the link, not the file it points to,
            // which keeps the secrets (review P3-3). Looked at before the migration replaces the link.
            let link_target = std::fs::symlink_metadata(&config_path)
                .is_ok_and(|m| m.file_type().is_symlink())
                .then(|| std::fs::canonicalize(&config_path).or_else(|_| std::fs::read_link(&config_path)))
                .map(|t| t.unwrap_or_else(|_| config_path.clone()));
            match migrate_legacy(dir, &file, &legacy, backend, opt_in) {
                Ok(to) if link_target.is_some() => {
                    let target = link_target.unwrap_or_default();
                    notices.push(Notice::MigratedFromLink { to, target });
                    match to {
                        CredentialStore::Keyring => SecretsLocation::Keyring,
                        CredentialStore::File => SecretsLocation::File(dir.join(CREDENTIALS_FILE)),
                    }
                }
                Ok(CredentialStore::Keyring) => {
                    notices.push(Notice::Migrated {
                        to: CredentialStore::Keyring,
                    });
                    SecretsLocation::Keyring
                }
                Ok(CredentialStore::File) => {
                    let path = dir.join(CREDENTIALS_FILE);
                    notices.push(Notice::Migrated {
                        to: CredentialStore::File,
                    });
                    notices.push(Notice::Unencrypted { path: path.clone() });
                    SecretsLocation::File(path)
                }
                Err(notice) => {
                    *declined.0.lock().unwrap_or_else(|e| e.into_inner()) = file_stamp.clone();
                    notices.push(notice);
                    SecretsLocation::LegacyConfig(config_path)
                }
            }
        } else {
            if !already_declined {
                notices.push(Notice::Unencrypted {
                    path: config_path.clone(),
                });
            }
            SecretsLocation::LegacyConfig(config_path)
        };
        // Either way this run uses the secrets it read: after a migration they are byte for byte what was verified
        // in the new place; without one they are still only in the old file.
        return Loaded {
            config: config.with_secrets(legacy, location),
            notices,
        };
    }

    let config = match file.credential_store {
        Some(CredentialStore::Keyring) => match cache.read(backend, &file.account(), file_stamp) {
            Ok(Some(secrets)) => config.with_secrets(secrets, SecretsLocation::Keyring),
            // The entry is gone (removed by hand, or by `bb logout` elsewhere): signed out.
            Ok(None) => config,
            Err(reason) => {
                notices.push(Notice::Unreadable {
                    keyring_refused: credentials::keyring_refused(&reason),
                    reason: format!("the OS keyring: {reason}"),
                });
                config
            }
        },
        Some(CredentialStore::File) => {
            let path = dir.join(CREDENTIALS_FILE);
            let read = match private_file::read_private(&path) {
                Err(private_file::ReadError::TooOpen) => {
                    notices.push(Notice::TooOpen { path });
                    return Loaded { config, notices };
                }
                Err(private_file::ReadError::Failed(reason)) => Err(reason),
                Ok(bytes) => bytes.map(|b| credentials::decode(&b)).transpose(),
            };
            match read {
                Ok(Some(secrets)) => {
                    notices.push(Notice::Unencrypted { path: path.clone() });
                    config.with_secrets(secrets, SecretsLocation::File(path))
                }
                Ok(None) => config,
                Err(reason) => {
                    notices.push(Notice::Unreadable {
                        reason,
                        keyring_refused: false,
                    });
                    config
                }
            }
        }
        None => config,
    };
    Loaded { config, notices }
}

/// F5. The secrets of an older `config.json` go into the keyring; they are read back and compared; only on a match
/// is `config.json` rewritten without them. No keyring: with the opt-in, the same into `credentials.json`; without
/// it, nothing changes. On any failure the file is left exactly as it was, and the notice says why.
fn migrate_legacy(
    dir: &Path,
    file: &ConfigFile,
    legacy: &Secrets,
    backend: &dyn SecretBackend,
    opt_in: bool,
) -> Result<CredentialStore, Notice> {
    let config_path = dir.join(CONFIG_FILE);
    let account = file.account();
    let encoded = credentials::encode(legacy);
    let read_back_failed = |reason| Notice::ReadBackFailed {
        path: config_path.clone(),
        reason,
    };
    let store = match backend.set(&account, &encoded) {
        Ok(()) => {
            credentials::verify(backend, &account, legacy).map_err(read_back_failed)?;
            CredentialStore::Keyring
        }
        Err(_) if opt_in => {
            let path = dir.join(CREDENTIALS_FILE);
            private_file::write(&path, &encoded)
                .and_then(|()| verify_file(&path, legacy))
                .map_err(read_back_failed)?;
            CredentialStore::File
        }
        Err(reason) => {
            return Err(Notice::NotMigrated {
                path: config_path,
                reason,
            });
        }
    };
    write_file(dir, &file.pointing_at(store)).map_err(|reason| Notice::RewriteFailed {
        path: config_path,
        reason,
    })?;
    Ok(store)
}

fn verify_file(path: &Path, expected: &Secrets) -> Result<(), String> {
    let bytes = private_file::read(path)?.ok_or_else(|| format!("{} is not there", path.display()))?;
    if credentials::decode(&bytes)? == *expected {
        Ok(())
    } else {
        Err(format!("{} reads back different", path.display()))
    }
}

static KEYRING_CACHE: KeyringCache = KeyringCache::new();
static DECLINED: Declined = Declined::new();

pub fn load_config() -> Config {
    // Unit tests read, never migrate: they must not rewrite a developer's real config.json.
    let migrate = !cfg!(test);
    let opt_in = !cfg!(test) && credentials::opted_in_by_env();
    let loaded = load_from(
        &config_dir(),
        credentials::os_backend(),
        opt_in,
        migrate,
        &KEYRING_CACHE,
        &DECLINED,
    );
    credentials::print_once(&loaded.notices);
    let mut config = loaded.config;
    if let Some(api_url) = API_URL_OVERRIDE.get() {
        config.api_url = api_url.clone();
    }
    refuse_non_loopback_when_asked(&config.api_url);
    config
}

/// Is this a test harness's run of the binary (see [`REFUSE_NON_LOOPBACK_ENV`])?
pub(crate) fn test_harness_run() -> bool {
    std::env::var(REFUSE_NON_LOOPBACK_ENV).is_ok_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0"
    })
}

/// The variable a TEST harness sets on every run of the binary. With it, a run whose API URL is not on this
/// machine stops before any request (see [`refuse_non_loopback_when_asked`]), and the OS keyring is never used
/// (`credentials::os_backend`).
pub const REFUSE_NON_LOOPBACK_ENV: &str = "BB_REFUSE_NON_LOOPBACK_API";

/// Is `url` a server on this machine (`localhost`, `127.0.0.0/8`, `::1`)? Fail closed: anything that does not
/// parse, has no host, or names another host (including `localhost.` and `127.0.0.1.example.com`) is not.
pub(crate) fn api_url_is_loopback(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The CLI's API URL comes from `--api`, the config file, or the built-in default, https://api.beebeeb.io. It is
/// NOT read from the environment (there is no `BB_API_BASE`). A test that runs the binary with a scratch HOME and
/// names no server therefore points it at PRODUCTION. So a harness sets [`REFUSE_NON_LOOPBACK_ENV`], and a run
/// that would use a non-loopback URL exits 2 here instead, before it sends anything. It only ever refuses.
fn refuse_non_loopback_when_asked(api_url: &str) {
    if !test_harness_run() || api_url_is_loopback(api_url) {
        return;
    }
    let host = reqwest::Url::parse(api_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "an address that does not parse".to_string());
    eprintln!(
        "Refusing to contact {host}: {REFUSE_NON_LOOPBACK_ENV} is set, so this run may only talk to a server on this \
         machine. Name one with --api, or in the config file."
    );
    std::process::exit(2);
}

pub fn set_api_url_override(api_url: String) -> Result<(), String> {
    let api_url = api_url.trim_end_matches('/').to_string();
    if api_url.is_empty() {
        return Err("--api must not be empty".to_string());
    }
    API_URL_OVERRIDE
        .set(api_url)
        .map_err(|_| "--api was already configured".to_string())
}

/// Can `bb login` keep the secrets on this machine? Asked BEFORE the browser approval, so nobody approves a
/// sign-in that is then refused. `Err` carries the reason the keyring did not answer.
pub fn keyring_reachable() -> Result<(), String> {
    probe(credentials::os_backend())
}

/// A store, a read-back and a delete of a throwaway entry: a keyring that only answers reads (a locked collection
/// with no one to unlock it) must fail here, before the browser approval, not after it.
pub(crate) fn probe(backend: &dyn SecretBackend) -> Result<(), String> {
    let account = credentials::account("reachability check", None);
    let marker = b"bb reachability check";
    backend.set(&account, marker)?;
    let read = backend.get(&account);
    let _ = backend.delete(&account);
    match read? {
        Some(bytes) if bytes.as_slice() == marker => Ok(()),
        Some(_) => Err("the OS keyring reads back a different value".to_string()),
        None => Err("the OS keyring did not keep a value".to_string()),
    }
}

/// `bb login`'s save (F1–F4): the secrets go into the keyring entry for `email` on `api_url`, are read back and
/// compared, and then `config.json` gets only the settings and the marker. A `credentials.json` from an earlier
/// sign-in is removed, and so is the keyring entry of a previous, different account. No keyring: with `opt_in`
/// the secrets go to `credentials.json` instead; without it, the refusal that names the opt-in.
pub fn save_login(api_url: &str, email: &str, secrets: &Secrets, opt_in: bool) -> Result<CredentialStore, String> {
    save_login_in(
        &config_dir(),
        credentials::os_backend(),
        api_url,
        email,
        secrets,
        opt_in,
    )
}

/// The opt-in `credentials.json` of this machine's config dir.
pub fn credentials_path() -> PathBuf {
    config_dir().join(CREDENTIALS_FILE)
}

pub(crate) fn save_login_in(
    dir: &Path,
    backend: &dyn SecretBackend,
    api_url: &str,
    email: &str,
    secrets: &Secrets,
    opt_in: bool,
) -> Result<CredentialStore, String> {
    let file = ConfigFile {
        api_url: api_url.to_string(),
        email: Some(email.to_string()),
        credential_store: None,
        session_token: None,
        master_key: None,
    };
    let account = file.account();
    let previous = read_file(dir);
    let encoded = credentials::encode(secrets);
    let credentials_path = dir.join(CREDENTIALS_FILE);
    let store = match backend.set(&account, &encoded) {
        Ok(()) => {
            // The keyring answered: from here on a failure is an error, never a fall-back to the plaintext file.
            let published = credentials::verify(backend, &account, secrets)
                .map_err(|reason| {
                    format!("bb stored your sign-in in the OS keyring but could not read it back ({reason})")
                })
                // An earlier opt-in sign-in's file: its secret must not outlive this one. Removed before config.json
                // points at the keyring. If writing config.json then fails, the new entry is rolled back below, but
                // the earlier credentials.json is already gone: no secret is left behind, and the machine is signed
                // out until the next `bb login`.
                .and_then(|()| private_file::remove(&credentials_path).map(|_| ()))
                .and_then(|()| write_file(dir, &file.pointing_at(CredentialStore::Keyring)));
            if let Err(e) = published {
                // Roll back: nothing points at this entry, so it must not stay behind.
                let _ = backend.delete(&account);
                return Err(e);
            }
            CredentialStore::Keyring
        }
        Err(_) if opt_in => {
            let published = private_file::write(&credentials_path, &encoded)
                .and_then(|()| verify_file(&credentials_path, secrets))
                .and_then(|()| write_file(dir, &file.pointing_at(CredentialStore::File)));
            if let Err(e) = published {
                // Roll back: a plaintext key that config.json does not point at would outlive `bb logout`.
                let _ = private_file::remove(&credentials_path);
                return Err(e);
            }
            CredentialStore::File
        }
        Err(reason) => return Err(credentials::refusal(&reason)),
    };
    // The previous sign-in's keyring entry, when it was another account (a new sign-in of the same account just
    // overwrote it). Best effort: the new sign-in is complete either way.
    if let Some(previous) = previous.filter(|p| p.credential_store == Some(CredentialStore::Keyring)) {
        if previous.account() != account {
            let _ = backend.delete(&previous.account());
        }
    }
    Ok(store)
}

/// Is there a stored sign-in (a marker, an older file's secrets, a `credentials.json`, or a `config.json` that does
/// not parse), whether or not this run could read it?
pub fn has_stored_sign_in() -> bool {
    let dir = config_dir();
    // A credentials.json, or a config.json that no longer parses, is still something `bb logout` must remove.
    dir.join(CREDENTIALS_FILE).exists()
        || match read_file(&dir) {
            Some(f) => f.credential_store.is_some() || f.legacy_secrets().is_some(),
            None => dir.join(CONFIG_FILE).exists(),
        }
}

/// `bb logout`'s local half (F6): the keyring entry, `credentials.json`, leftover temporaries and `config.json` all
/// go. A keyring entry that cannot be removed is an error that says so, and `config.json` then stays so the logout
/// can be retried (review P3-5); the rest is still removed.
pub fn clear_config() -> Result<(), String> {
    clear_in(&config_dir(), credentials::os_backend())
}

pub(crate) fn clear_in(dir: &Path, backend: &dyn SecretBackend) -> Result<(), String> {
    let mut problems = Vec::new();
    let mut keep_config = false;
    if let Some(file) = read_file(dir) {
        let account = file.account();
        // Every marker: an old entry of this account may be in the keyring even when the marker says `file` (a
        // keyring that was unreadable at login, a timed-out store that finished later) or when there is no marker
        // (a migration whose read-back failed). Only for the `keyring` marker is a failure the person's to know.
        let deleted = backend.delete(&account);
        if let (Some(CredentialStore::Keyring), Err(reason)) = (file.credential_store, deleted) {
            // config.json is the only pointer to the entry: it stays, so `bb logout` can be run again.
            keep_config = true;
            problems.push(format!(
                "bb could not remove your sign-in from the OS keyring ({reason}). {CONFIG_FILE} was kept so you can \
                 run `bb logout` again; or remove the \"{}\" item for \"{account}\" by hand",
                credentials::SERVICE
            ));
        }
    }
    let names: &[&str] = if keep_config {
        &[CREDENTIALS_FILE]
    } else {
        &[CREDENTIALS_FILE, CONFIG_FILE]
    };
    for name in names {
        if let Err(e) = private_file::remove(&dir.join(name)) {
            problems.push(e);
        }
    }
    // Temporaries a crash between create and rename left behind; they may hold secrets (review P3-2).
    private_file::sweep_temporaries(dir, None);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

#[cfg(test)]
mod loopback_tests {
    use super::api_url_is_loopback;

    #[test]
    fn only_a_server_on_this_machine_is_loopback() {
        for url in [
            "http://127.0.0.1:3001",
            "http://127.0.0.1",
            "https://127.1.2.3:8443",
            "http://localhost:5173",
            "http://LOCALHOST:5173/path",
            "http://[::1]:3001",
            "https://localhost",
        ] {
            assert!(api_url_is_loopback(url), "{url}");
        }
        for url in [
            "https://api.beebeeb.io",
            "http://api.beebeeb.io:3001",
            "http://127.0.0.1.example.com",
            "http://localhost.example.com",
            "http://localhost.",
            "http://127.0.0.1@evil.example",
            "http://0.0.0.0:3001",
            "http://10.0.0.5",
            "ftp://127.0.0.1",
            "127.0.0.1:3001",
            "",
            "not a url",
        ] {
            assert!(!api_url_is_loopback(url), "{url}");
        }
    }
}

#[cfg(test)]
mod credential_store_tests;
