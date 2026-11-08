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
            match migrate_legacy(dir, &file, &legacy, backend, opt_in) {
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
                    reason: format!("the OS keyring: {reason}"),
                });
                config
            }
        },
        Some(CredentialStore::File) => {
            let path = dir.join(CREDENTIALS_FILE);
            let read = private_file::read(&path).and_then(|bytes| bytes.map(|b| credentials::decode(&b)).transpose());
            match read {
                Ok(Some(secrets)) => {
                    notices.push(Notice::Unencrypted { path: path.clone() });
                    config.with_secrets(secrets, SecretsLocation::File(path))
                }
                Ok(None) => config,
                Err(reason) => {
                    notices.push(Notice::Unreadable { reason });
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
                // An earlier opt-in sign-in's file: its secret must not outlive this one. Removed BEFORE config.json
                // points at the keyring, so a failure leaves the previous sign-in as it was.
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

/// `bb logout`'s local half (F6): the keyring entry, `credentials.json` and `config.json` all go. A keyring entry
/// that cannot be removed is an error that says so (the rest is still removed).
pub fn clear_config() -> Result<(), String> {
    clear_in(&config_dir(), credentials::os_backend())
}

pub(crate) fn clear_in(dir: &Path, backend: &dyn SecretBackend) -> Result<(), String> {
    let mut problems = Vec::new();
    if let Some(file) = read_file(dir) {
        let account = file.account();
        match file.credential_store {
            Some(CredentialStore::Keyring) => {
                if let Err(reason) = backend.delete(&account) {
                    problems.push(format!(
                        "bb could not remove your sign-in from the OS keyring ({reason}); remove the \"{}\" item \
                         for \"{account}\" by hand",
                        credentials::SERVICE
                    ));
                }
            }
            // An older file: a migration whose read-back failed may have left a copy in the keyring.
            None => {
                let _ = backend.delete(&account);
            }
            Some(CredentialStore::File) => {}
        }
    }
    for name in [CREDENTIALS_FILE, CONFIG_FILE] {
        if let Err(e) = private_file::remove(&dir.join(name)) {
            problems.push(e);
        }
    }
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
mod credential_store_tests {
    //! F7 (a)–(d) against `keyring`'s mock credentials, in a scratch directory (never the real config dir).
    use super::*;
    use crate::credentials::mock::{Fault, MockKeyring};
    use crate::credentials::{Unavailable, unencrypted_warning};

    const API: &str = "http://127.0.0.1:3001";
    const EMAIL: &str = "fixture@beebeeb.io";
    const MASTER_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const TOKEN: &str = "fixture-session-token";

    fn both() -> Secrets {
        Secrets {
            master_key: Some(Zeroizing::new(MASTER_KEY.to_string())),
            session_token: Some(Zeroizing::new(TOKEN.to_string())),
        }
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bb-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An older install's file, as the old `save_config` wrote it.
    fn legacy(dir: &Path) -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "api_url": API,
            "session_token": TOKEN,
            "email": EMAIL,
            "master_key": MASTER_KEY,
        }))
        .unwrap();
        std::fs::write(dir.join(CONFIG_FILE), &bytes).unwrap();
        bytes
    }

    fn load(dir: &Path, backend: &dyn SecretBackend, opt_in: bool) -> Loaded {
        load_from(dir, backend, opt_in, true, &KeyringCache::new(), &Declined::new())
    }

    fn on_disk(dir: &Path, name: &str) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
    }

    fn assert_no_secret(dir: &Path, name: &str) {
        let text = std::fs::read_to_string(dir.join(name)).unwrap();
        assert!(!text.contains(MASTER_KEY), "{name} still holds the master key");
        assert!(!text.contains(TOKEN), "{name} still holds the session token");
        let json = on_disk(dir, name);
        assert!(
            json.get("master_key").is_none() && json.get("session_token").is_none(),
            "{name} still has a secret field"
        );
    }

    fn account() -> String {
        credentials::account(API, Some(EMAIL))
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn assert_signed_in(config: &Config) {
        // `assert!`, not `assert_eq!`: a failure must never print a secret, not even a fixture.
        assert!(
            config.master_key.as_deref().map(String::as_str) == Some(MASTER_KEY),
            "the master key is not the one stored"
        );
        assert!(
            config.session_token.as_deref().map(String::as_str) == Some(TOKEN),
            "the session token is not the one stored"
        );
        assert_eq!(config.email.as_deref(), Some(EMAIL));
    }

    /// F7 (a) on the save path itself: under umask 022, each file `bb login` writes is never readable by group or
    /// other, seen at the moment its bytes are on disk (the `private_file` seam) and after it is published.
    #[cfg(unix)]
    #[test]
    fn the_login_save_path_never_leaves_a_file_wider_than_0600_under_umask_022() {
        let dir = tempdir();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let observed = seen.clone();
        let mask = private_file::test_umask(0o022);
        private_file::observe(Box::new(move |p: &Path| {
            observed
                .borrow_mut()
                .push((p.to_path_buf(), mode(p), std::fs::read(p).unwrap()));
        }));
        let store = save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true);
        private_file::stop_observing();
        drop(mask);
        assert_eq!(store.unwrap(), CredentialStore::File);
        let seen = seen.borrow();
        assert_eq!(
            seen.len(),
            2,
            "credentials.json and config.json, each seen once with its bytes"
        );
        let (secret_path, _, _) = seen
            .iter()
            .find(|(_, _, bytes)| String::from_utf8_lossy(bytes).contains(MASTER_KEY))
            .expect("the seam saw the file that holds the key");
        assert!(secret_path.to_string_lossy().contains(CREDENTIALS_FILE));
        for (path, mode_while_written, _) in seen.iter() {
            assert_eq!(
                mode_while_written & 0o077,
                0,
                "{} was {mode_while_written:o} while its bytes were on disk",
                path.display()
            );
        }
        assert_eq!(mode(&dir.join(CREDENTIALS_FILE)), 0o600);
        assert_eq!(mode(&dir.join(CONFIG_FILE)), 0o600);
    }

    /// F7 (b): `bb login` with a keyring: both secrets in ONE entry, nothing secret on disk, and the next command
    /// reads them back.
    #[test]
    fn login_stores_both_secrets_in_one_keyring_entry_and_none_on_disk() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        let store = save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        assert_eq!(store, CredentialStore::Keyring);
        assert_eq!(mock.stored(&account()), Some(both()));
        assert_no_secret(&dir, CONFIG_FILE);
        assert_eq!(on_disk(&dir, CONFIG_FILE)["credential_store"], "keyring");
        assert!(!dir.join(CREDENTIALS_FILE).exists());
        let loaded = load(&dir, &mock, false);
        assert_signed_in(&loaded.config);
        assert_eq!(loaded.config.secrets_location, Some(SecretsLocation::Keyring));
        assert!(loaded.notices.is_empty(), "{:?}", loaded.notices);
    }

    #[test]
    fn a_keyring_read_is_cached_per_sign_in_and_a_new_sign_in_is_read_again() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        let cache = KeyringCache::new();
        let declined = Declined::new();
        assert_signed_in(&load_from(&dir, &mock, false, true, &cache, &declined).config);
        mock.set_fault(Fault::Gets);
        assert_signed_in(&load_from(&dir, &mock, false, true, &cache, &declined).config);
        mock.set_fault(Fault::None);
        // A new sign-in replaces config.json: the cached answer is not used for it.
        let other = Secrets {
            master_key: Some(Zeroizing::new(
                "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=".to_string(),
            )),
            session_token: Some(Zeroizing::new("second-token".to_string())),
        };
        save_login_in(&dir, &mock, API, EMAIL, &other, false).unwrap();
        let again = load_from(&dir, &mock, false, true, &cache, &declined).config;
        assert!(
            again.session_token.as_deref().map(String::as_str) == Some("second-token"),
            "the second sign-in's token was not read"
        );
    }

    #[test]
    fn login_without_a_keyring_refuses_and_names_the_opt_in() {
        let dir = tempdir();
        let err = save_login_in(&dir, &MockKeyring::with_fault(Fault::Sets), API, EMAIL, &both(), false).unwrap_err();
        assert!(
            err.contains("--store-secrets-in-file") && err.contains("BB_STORE_SECRETS_IN_FILE=1"),
            "unexpected error text"
        );
        assert!(
            !err.contains(MASTER_KEY) && !err.contains(TOKEN),
            "the error quotes a secret"
        );
        assert!(!dir.join(CONFIG_FILE).exists() && !dir.join(CREDENTIALS_FILE).exists());
        assert!(probe(&MockKeyring::with_fault(Fault::Gets)).is_err());
        assert!(
            probe(&MockKeyring::new()).is_ok(),
            "an empty keyring is a reachable keyring"
        );
    }

    #[test]
    fn login_with_the_opt_in_and_no_keyring_writes_a_0600_credentials_file_and_warns() {
        let dir = tempdir();
        let store = save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).unwrap();
        assert_eq!(store, CredentialStore::File);
        assert_no_secret(&dir, CONFIG_FILE);
        assert_eq!(on_disk(&dir, CONFIG_FILE)["credential_store"], "file");
        #[cfg(unix)]
        assert_eq!(mode(&dir.join(CREDENTIALS_FILE)), 0o600);
        let loaded = load(&dir, &Unavailable("no keyring"), false);
        assert_signed_in(&loaded.config);
        let warning = unencrypted_warning(&dir.join(CREDENTIALS_FILE));
        assert!(
            loaded.notices.iter().any(|n| n.text() == warning),
            "{:?}",
            loaded.notices
        );
    }

    #[test]
    fn the_opt_in_never_wins_over_a_reachable_keyring() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        assert_eq!(
            save_login_in(&dir, &mock, API, EMAIL, &both(), true).unwrap(),
            CredentialStore::Keyring
        );
        assert!(!dir.join(CREDENTIALS_FILE).exists());
    }

    #[test]
    fn a_keyring_sign_in_removes_an_earlier_credentials_file_and_another_accounts_entry() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).unwrap();
        assert!(dir.join(CREDENTIALS_FILE).exists());
        save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        assert!(
            !dir.join(CREDENTIALS_FILE).exists(),
            "the plaintext file must go once the keyring has it"
        );
        save_login_in(&dir, &mock, API, "second@beebeeb.io", &both(), false).unwrap();
        assert_eq!(mock.stored(&account()), None, "the previous account's entry must go");
        assert!(
            mock.stored(&credentials::account(API, Some("second@beebeeb.io")))
                .is_some()
        );
    }

    /// F7 (c), success: config.json has no secret left, and the keyring holds both.
    #[test]
    fn migration_moves_both_secrets_into_the_keyring_and_scrubs_config_json() {
        let dir = tempdir();
        legacy(&dir);
        let mock = MockKeyring::new();
        let loaded = load(&dir, &mock, false);
        assert_signed_in(&loaded.config);
        assert_eq!(loaded.config.secrets_location, Some(SecretsLocation::Keyring));
        assert_eq!(
            loaded.notices,
            vec![Notice::Migrated {
                to: CredentialStore::Keyring
            }]
        );
        assert_eq!(mock.stored(&account()), Some(both()));
        assert_no_secret(&dir, CONFIG_FILE);
        let json = on_disk(&dir, CONFIG_FILE);
        assert_eq!(json["credential_store"], "keyring");
        assert_eq!(json["api_url"], API);
        assert_eq!(json["email"], EMAIL);
        #[cfg(unix)]
        assert_eq!(mode(&dir.join(CONFIG_FILE)), 0o600);
        // The next run reads from the keyring and says nothing.
        let next = load(&dir, &mock, false);
        assert_signed_in(&next.config);
        assert!(next.notices.is_empty(), "{:?}", next.notices);
    }

    /// F7 (c), a failed read-back: the file is unchanged, byte for byte, and this run still works.
    #[test]
    fn a_failed_read_back_leaves_config_json_unchanged() {
        for fault in [Fault::Gets, Fault::Corrupt] {
            let dir = tempdir();
            let before = legacy(&dir);
            let mock = MockKeyring::with_fault(fault);
            let loaded = load(&dir, &mock, true);
            assert!(
                std::fs::read(dir.join(CONFIG_FILE)).unwrap() == before,
                "{fault:?}: config.json must be unchanged"
            );
            assert!(!dir.join(CREDENTIALS_FILE).exists(), "{fault:?}");
            assert_signed_in(&loaded.config);
            assert!(
                matches!(loaded.notices.as_slice(), [Notice::ReadBackFailed { .. }]),
                "{fault:?}: {:?}",
                loaded.notices
            );
        }
    }

    /// F7 (c), no keyring and no opt-in: the file is unchanged, and the F3 warning says how to opt in.
    #[test]
    fn no_keyring_and_no_opt_in_leaves_the_file_and_warns() {
        let dir = tempdir();
        let before = legacy(&dir);
        let loaded = load(&dir, &MockKeyring::with_fault(Fault::Sets), false);
        assert!(
            std::fs::read(dir.join(CONFIG_FILE)).unwrap() == before,
            "config.json must be unchanged"
        );
        assert!(!dir.join(CREDENTIALS_FILE).exists());
        assert_signed_in(&loaded.config);
        let [notice] = loaded.notices.as_slice() else {
            panic!("one notice expected: {:?}", loaded.notices)
        };
        let text = notice.text();
        assert!(
            text.starts_with(&unencrypted_warning(&dir.join(CONFIG_FILE))),
            "unexpected notice text"
        );
        assert!(
            text.contains("BB_STORE_SECRETS_IN_FILE=1") && text.contains("bb login"),
            "unexpected notice text"
        );
        assert!(
            !text.contains(MASTER_KEY) && !text.contains(TOKEN),
            "the text quotes a secret"
        );
    }

    /// F7 (c), no keyring with the opt-in: `credentials.json` at 0600, and `config.json` scrubbed.
    #[test]
    fn no_keyring_with_the_opt_in_moves_the_secrets_into_a_0600_credentials_file() {
        let dir = tempdir();
        legacy(&dir);
        let loaded = load(&dir, &Unavailable("no keyring"), true);
        assert_signed_in(&loaded.config);
        assert_no_secret(&dir, CONFIG_FILE);
        assert_eq!(on_disk(&dir, CONFIG_FILE)["credential_store"], "file");
        let creds = std::fs::read(dir.join(CREDENTIALS_FILE)).unwrap();
        assert_eq!(credentials::decode(&creds).unwrap(), both());
        #[cfg(unix)]
        assert_eq!(mode(&dir.join(CREDENTIALS_FILE)), 0o600);
        assert!(
            loaded.notices.contains(&Notice::Unencrypted {
                path: dir.join(CREDENTIALS_FILE)
            }),
            "{:?}",
            loaded.notices
        );
    }

    #[test]
    fn a_declined_migration_is_not_retried_in_the_same_process() {
        let dir = tempdir();
        legacy(&dir);
        let mock = MockKeyring::with_fault(Fault::Sets);
        let cache = KeyringCache::new();
        let declined = Declined::new();
        assert_eq!(load_from(&dir, &mock, false, true, &cache, &declined).notices.len(), 1);
        let again = load_from(&dir, &mock, false, true, &cache, &declined);
        assert_signed_in(&again.config);
        assert!(again.notices.is_empty(), "{:?}", again.notices);
    }

    /// F7 (d): logout deletes the keyring entry, and both files.
    #[test]
    fn logout_deletes_the_keyring_entry_and_both_files() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        assert!(mock.stored(&account()).is_some());
        clear_in(&dir, &mock).unwrap();
        assert_eq!(mock.stored(&account()), None, "logout must delete the keyring entry");
        assert!(!dir.join(CONFIG_FILE).exists());
        let after = load(&dir, &mock, false);
        assert!(after.config.session_token.is_none() && after.config.master_key.is_none());
    }

    #[test]
    fn logout_removes_the_credentials_file() {
        let dir = tempdir();
        save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).unwrap();
        clear_in(&dir, &Unavailable("no keyring")).unwrap();
        assert!(!dir.join(CREDENTIALS_FILE).exists() && !dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn a_keyring_entry_that_cannot_be_deleted_is_reported_and_the_files_still_go() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        mock.set_fault(Fault::Deletes);
        let err = clear_in(&dir, &mock).unwrap_err();
        assert!(
            err.contains(credentials::SERVICE) && err.contains("by hand"),
            "unexpected error text"
        );
        assert!(
            !err.contains(MASTER_KEY) && !err.contains(TOKEN),
            "the error quotes a secret"
        );
        assert!(!dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn a_keyring_that_cannot_be_read_signs_the_run_out_with_a_notice() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        mock.set_fault(Fault::Gets);
        let loaded = load(&dir, &mock, false);
        assert!(loaded.config.session_token.is_none() && loaded.config.master_key.is_none());
        assert!(
            matches!(loaded.notices.as_slice(), [Notice::Unreadable { .. }]),
            "{:?}",
            loaded.notices
        );
    }

    /// A failed keyring read is not kept: once the keyring answers again, the same process is signed in again.
    #[test]
    fn a_failed_keyring_read_is_asked_again_and_recovers() {
        let dir = tempdir();
        let mock = MockKeyring::new();
        save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
        let cache = KeyringCache::new();
        let declined = Declined::new();
        mock.set_fault(Fault::Gets);
        let locked = load_from(&dir, &mock, false, true, &cache, &declined);
        assert!(locked.config.master_key.is_none(), "a locked keyring signs the run out");
        mock.set_fault(Fault::None);
        assert_signed_in(&load_from(&dir, &mock, false, true, &cache, &declined).config);
    }

    /// A login whose config.json cannot be written leaves no secret behind: no keyring entry, no credentials.json.
    #[test]
    fn a_login_that_cannot_write_config_json_rolls_back_the_stored_secrets() {
        let blocked = |dir: &Path| {
            // config.json is a non-empty directory: the rename over it fails after the secrets were stored.
            std::fs::create_dir(dir.join(CONFIG_FILE)).unwrap();
            std::fs::write(dir.join(CONFIG_FILE).join("x"), b"").unwrap();
        };
        let dir = tempdir();
        blocked(&dir);
        let mock = MockKeyring::new();
        assert!(save_login_in(&dir, &mock, API, EMAIL, &both(), false).is_err());
        assert!(
            mock.stored(&account()).is_none(),
            "the keyring entry must be rolled back"
        );
        let dir = tempdir();
        blocked(&dir);
        assert!(save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).is_err());
        assert!(
            !dir.join(CREDENTIALS_FILE).exists(),
            "credentials.json must be rolled back"
        );
    }

    /// A keyring that answers but reads back wrong is an error, never a fall-back to the plaintext file.
    #[test]
    fn a_reachable_keyring_that_reads_back_wrong_never_falls_back_to_the_file() {
        let dir = tempdir();
        let mock = MockKeyring::with_fault(Fault::Corrupt);
        let err = save_login_in(&dir, &mock, API, EMAIL, &both(), true).unwrap_err();
        assert!(err.contains("could not read it back"), "unexpected error text");
        assert!(!dir.join(CREDENTIALS_FILE).exists() && !dir.join(CONFIG_FILE).exists());
        mock.set_fault(Fault::None);
        assert!(
            mock.stored(&account()).is_none(),
            "the unverified entry must be removed"
        );
    }

    /// The pre-approval probe stores, not only reads: a keyring that answers reads but refuses a store fails it.
    #[test]
    fn the_probe_fails_on_a_keyring_that_refuses_a_store() {
        assert!(probe(&MockKeyring::with_fault(Fault::Sets)).is_err());
        let mock = MockKeyring::new();
        assert!(probe(&mock).is_ok());
        assert!(
            mock.stored(&credentials::account("reachability check", None)).is_none(),
            "the probe leaves no entry"
        );
    }

    #[test]
    fn debug_output_hides_the_secrets() {
        let dir = tempdir();
        legacy(&dir);
        let text = format!("{:?}", load(&dir, &MockKeyring::new(), false).config);
        assert!(
            !text.contains(MASTER_KEY) && !text.contains(TOKEN),
            "the text quotes a secret"
        );
    }
}
