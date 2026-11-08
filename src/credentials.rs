//! Where `bb` keeps the account's two secrets, the master key and the session token (LD-0029, decisions F1–F6).
//!
//! - **One OS keyring entry per account** (F1): service [`SERVICE`], account `<email> on <api url>`
//!   ([`account`]). It holds BOTH secrets as one JSON value ([`encode`]). `config.json` keeps only `api_url`,
//!   `email` and the `credential_store` marker, and its on-disk type cannot serialize a secret.
//! - **Backends** (F2): macOS Keychain, Windows Credential Manager, Linux Secret Service (pure Rust, over zbus: the
//!   static musl builds link no C library), through the `keyring` crate. A local OS API, no network service.
//! - **Headless** (F3): no keyring reachable → `bb login` refuses, naming the opt-in ([`OPT_IN_FLAG`] /
//!   [`OPT_IN_ENV`]`=1`). With it, the secrets go to [`CREDENTIALS_FILE`] (0600, written atomically,
//!   `private_file`) and every command prints [`unencrypted_warning`].
//! - **Never a secret in an error or a log line**: errors from the keyring are reduced to the platform's own
//!   message ([`describe`]), and a payload that does not parse is reported without its content (serde_json's
//!   messages can quote the input).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// The keyring service every entry is stored under.
pub const SERVICE: &str = "io.beebeeb.cli";
/// The opt-in to keep the secrets in a file when no keyring is reachable (F3).
pub const OPT_IN_ENV: &str = "BB_STORE_SECRETS_IN_FILE";
/// `bb login`'s spelling of the same opt-in.
pub const OPT_IN_FLAG: &str = "--store-secrets-in-file";
/// The opt-in file, next to `config.json`.
pub const CREDENTIALS_FILE: &str = "credentials.json";

/// Where `config.json` says the secrets are (the `credential_store` marker).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialStore {
    /// The OS keyring entry for this account.
    Keyring,
    /// [`CREDENTIALS_FILE`] (the F3 opt-in).
    File,
}

/// The two secrets. Both are wiped from memory on drop. An older install's `config.json` may hold only one of
/// them, so each is optional; a `bb login` always stores both.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secrets {
    /// Base64 of the 32-byte master key.
    pub master_key: Option<Zeroizing<String>>,
    pub session_token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("master_key", &self.master_key.as_ref().map(|_| "<hidden>"))
            .field("session_token", &self.session_token.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

impl Secrets {
    pub fn is_empty(&self) -> bool {
        self.master_key.is_none() && self.session_token.is_none()
    }
}

/// The keyring account for a sign-in: the API base and the email (F1), so two servers or two accounts never
/// share an entry. Readable on purpose: it is what a person sees in Keychain Access or Credential Manager.
pub fn account(api_url: &str, email: Option<&str>) -> String {
    format!("{} on {api_url}", email.unwrap_or("(no email)"))
}

#[derive(Serialize)]
struct PayloadOut<'a> {
    v: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    master_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_token: Option<&'a str>,
}

#[derive(Deserialize)]
struct PayloadIn {
    v: u32,
    #[serde(default)]
    master_key: Option<Zeroizing<String>>,
    #[serde(default)]
    session_token: Option<Zeroizing<String>>,
}

const PAYLOAD_VERSION: u32 = 1;

/// The one value stored for an account: `{"v":1,"master_key":…,"session_token":…}`. The buffer is sized up front
/// so serialization never reallocates (a reallocation would leave a copy of the secrets in freed memory).
pub fn encode(secrets: &Secrets) -> Zeroizing<Vec<u8>> {
    let master_key = secrets.master_key.as_ref().map(|s| s.as_str());
    let session_token = secrets.session_token.as_ref().map(|s| s.as_str());
    let capacity = 96 + master_key.map_or(0, |s| 2 * s.len()) + session_token.map_or(0, |s| 2 * s.len());
    let mut out = Zeroizing::new(Vec::with_capacity(capacity));
    let payload = PayloadOut {
        v: PAYLOAD_VERSION,
        master_key,
        session_token,
    };
    // Serializing two strings and an integer into a Vec cannot fail.
    serde_json::to_writer(&mut *out, &payload).expect("credential payload serializes");
    out
}

/// The inverse of [`encode`]. The error never quotes the input.
pub fn decode(bytes: &[u8]) -> Result<Secrets, String> {
    let payload: PayloadIn = serde_json::from_slice(bytes)
        .map_err(|_| "the stored sign-in is not in a format this bb understands".to_string())?;
    if payload.v != PAYLOAD_VERSION {
        return Err(format!(
            "the stored sign-in has format version {}, which this bb does not know (update bb)",
            payload.v
        ));
    }
    Ok(Secrets {
        master_key: payload.master_key,
        session_token: payload.session_token,
    })
}

/// A place that holds one secret per account. Implemented by the OS keyring, by "no keyring here", and in tests
/// by `keyring`'s mock credentials.
pub(crate) trait SecretBackend: Sync {
    /// Store `secret` for `account`, replacing what is there.
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String>;
    /// The secret for `account`; `Ok(None)` when there is none.
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String>;
    /// Remove the entry for `account`; `Ok(false)` when there was none.
    fn delete(&self, account: &str) -> Result<bool, String>;
}

/// Read back what was stored for `account` and compare it with `expected` (F5: nothing is deleted elsewhere until
/// this passed).
pub(crate) fn verify(backend: &dyn SecretBackend, account: &str, expected: &Secrets) -> Result<(), String> {
    let stored = backend
        .get(account)?
        .ok_or_else(|| "the entry bb just stored is not there".to_string())?;
    if decode(&stored)? == *expected {
        Ok(())
    } else {
        Err("the entry bb just stored reads back different".to_string())
    }
}

/// The `keyring` crate as a [`SecretBackend`]. `entry` makes (or, for the mock, finds) the entry for an account.
pub(crate) struct Keyring<F>(pub F);

impl<F> SecretBackend for Keyring<F>
where
    F: Fn(&str) -> keyring::Result<Arc<keyring::Entry>> + Sync,
{
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String> {
        (self.0)(account)
            .and_then(|entry| entry.set_secret(secret))
            .map_err(|e| describe(&e))
    }

    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        match (self.0)(account).and_then(|entry| entry.get_secret()) {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(&e)),
        }
    }

    fn delete(&self, account: &str) -> Result<bool, String> {
        match (self.0)(account).and_then(|entry| entry.delete_credential()) {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(describe(&e)),
        }
    }
}

/// A keyring error in words, without anything the error carries that could be a secret (`BadEncoding` holds the
/// stored bytes; `Ambiguous` holds credentials). The platform's message (an OS status, a D-Bus error name) is kept:
/// it is what tells a person why their keyring did not answer.
pub(crate) fn describe(error: &keyring::Error) -> String {
    match error {
        keyring::Error::PlatformFailure(inner) | keyring::Error::NoStorageAccess(inner) if platform_refused(error) => {
            format!("{ACCESS_REFUSED} ({inner})")
        }
        keyring::Error::PlatformFailure(inner) | keyring::Error::NoStorageAccess(inner) => inner.to_string(),
        keyring::Error::NoEntry => "no entry".to_string(),
        keyring::Error::BadEncoding(_) => "the entry is not in a format bb understands".to_string(),
        keyring::Error::TooLong(name, limit) => format!("{name} is longer than the keyring allows ({limit})"),
        keyring::Error::Invalid(name, reason) => format!("{name} is invalid: {reason}"),
        keyring::Error::Ambiguous(items) => format!("{} keyring entries match", items.len()),
        _ => "an unknown keyring error".to_string(),
    }
}

/// Is this a keyring that is there and said no, as opposed to no keyring at all? Per platform, from what `keyring`
/// 3.6 maps each OS status to:
/// - macOS: errSecAuthFailed (-25293), errSecUserCanceled (-128) and errSecInteractionNotAllowed (-25308) arrive as
///   `PlatformFailure` (a keychain item whose access list does not trust this binary, asked over SSH or from a
///   LaunchAgent where no prompt can show). `NoStorageAccess` there means no keychain (errSecNotAvailable,
///   errSecNoSuchKeychain), so: not reachable.
/// - Linux Secret Service: `NoStorageAccess` is a locked collection or a dismissed prompt.
/// - Windows: `NoStorageAccess` is ERROR_NO_SUCH_LOGON_SESSION, a logon with no credential store: not reachable.
///
/// The OS status is only in the inner error's `Debug` (`Error { code: -25293, .. }`); it is matched, never printed.
fn platform_refused(error: &keyring::Error) -> bool {
    if cfg!(target_os = "macos") {
        let keyring::Error::PlatformFailure(inner) = error else {
            return false;
        };
        let debug = format!("{inner:?}");
        return ["-25293", "-128", "-25308"]
            .iter()
            .any(|code| debug.contains(&format!("code: {code},")) || debug.contains(&format!("code: {code} }}")));
    }
    !cfg!(target_os = "windows") && matches!(error, keyring::Error::NoStorageAccess(_))
}

/// "No keyring here": every call fails with the reason.
pub(crate) struct Unavailable(pub &'static str);

impl SecretBackend for Unavailable {
    fn set(&self, _: &str, _: &[u8]) -> Result<(), String> {
        Err(self.0.to_string())
    }
    fn get(&self, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Err(self.0.to_string())
    }
    fn delete(&self, _: &str) -> Result<bool, String> {
        Err(self.0.to_string())
    }
}

/// How a keyring error that came from a keyring which exists but refused begins (a denied or locked keychain).
pub(crate) const ACCESS_REFUSED: &str = "the OS keyring did not allow access";
/// How a keyring error begins when the keyring did not answer in time (a prompt nobody answers).
pub(crate) const NO_ANSWER: &str = "the OS keyring did not answer";

/// Did the keyring exist and refuse, or not answer (as opposed to: no keyring reachable at all)? Asked of a reason
/// exactly as a [`SecretBackend`] returned it. The plaintext opt-in is never the fix for this case (review P2-1).
pub(crate) fn keyring_refused(reason: &str) -> bool {
    reason.starts_with(ACCESS_REFUSED) || reason.starts_with(NO_ANSWER)
}

/// A keyring call guard: every call runs on a thread of its own (with zbus on tokio, the Linux Secret Service
/// backend, a keyring call made on a runtime thread can deadlock, keyring-rs#132), one call at a time per process,
/// for at most `timeout`. A call that times out is left to finish on its own thread, and for `backoff` after it
/// every call returns the same error at once instead of starting another blocking call (review P3-1): a locked
/// keyring must not stall each `load_config` of `bb sync` for a minute, nor pile up threads.
pub(crate) struct Guarded<B> {
    inner: Arc<B>,
    timeout: std::time::Duration,
    backoff: std::time::Duration,
    /// Held for the whole call: a second caller waits for the first instead of starting a parallel call.
    turn: Mutex<()>,
    state: Arc<Mutex<GuardState>>,
}

fn lock(state: &Mutex<GuardState>) -> std::sync::MutexGuard<'_, GuardState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct GuardState {
    /// A call's thread has not returned yet (it may have timed out).
    in_flight: bool,
    /// The last timeout: when, and the reason every call gets until the back-off is over.
    failed: Option<(std::time::Instant, String)>,
}

impl<B: SecretBackend + Send + 'static> Guarded<B> {
    pub(crate) fn new(inner: B, timeout: std::time::Duration, backoff: std::time::Duration) -> Self {
        Self {
            inner: Arc::new(inner),
            timeout,
            backoff,
            turn: Mutex::new(()),
            state: Arc::new(Mutex::new(GuardState::default())),
        }
    }

    fn call<T: Send + 'static>(&self, op: impl FnOnce(&B) -> Result<T, String> + Send + 'static) -> Result<T, String> {
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut state = lock(&self.state);
            if let Some((since, reason)) = &state.failed {
                if since.elapsed() < self.backoff {
                    return Err(reason.clone());
                }
            }
            if state.in_flight {
                // The back-off is over but the call that timed out still has not returned: do not start another
                // one beside it, and wait out another back-off.
                let reason = state
                    .failed
                    .as_ref()
                    .map_or_else(|| format!("{NO_ANSWER} yet"), |(_, r)| r.clone());
                state.failed = Some((std::time::Instant::now(), reason.clone()));
                return Err(reason);
            }
            state.failed = None;
            state.in_flight = true;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (inner, state) = (self.inner.clone(), self.state.clone());
        let spawned = std::thread::Builder::new()
            .name("bb-keyring".to_string())
            .spawn(move || {
                let result = op(&inner);
                lock(&state).in_flight = false;
                let _ = tx.send(result);
            });
        if let Err(e) = spawned {
            lock(&self.state).in_flight = false;
            return Err(format!("could not start the OS keyring call: {e}"));
        }
        match rx.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let reason = format!(
                    "{NO_ANSWER} within {} seconds (is it locked, or waiting on a prompt?)",
                    self.timeout.as_secs()
                );
                lock(&self.state).failed = Some((std::time::Instant::now(), reason.clone()));
                Err(reason)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("the OS keyring call failed unexpectedly".to_string())
            }
        }
    }
}

impl<B: SecretBackend + Send + 'static> SecretBackend for Guarded<B> {
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), String> {
        let (account, secret) = (account.to_string(), Zeroizing::new(secret.to_vec()));
        self.call(move |b| b.set(&account, &secret))
    }
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        let account = account.to_string();
        self.call(move |b| b.get(&account))
    }
    fn delete(&self, account: &str) -> Result<bool, String> {
        let account = account.to_string();
        self.call(move |b| b.delete(&account))
    }
}

/// How long one OS keyring call may take, including an unlock prompt a person answers.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
const KEYRING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// After a call timed out, how long every call fails at once with the same reason.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
const KEYRING_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5 * 60);

#[cfg(all(not(test), any(target_os = "macos", target_os = "windows", target_os = "linux")))]
type OsEntry = fn(&str) -> keyring::Result<Arc<keyring::Entry>>;

#[cfg(all(not(test), any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn os_entry(account: &str) -> keyring::Result<Arc<keyring::Entry>> {
    keyring::Entry::new(SERVICE, account).map(Arc::new)
}

/// The backend this process uses.
///
/// - Unit tests never get the OS keyring: they pass `keyring`'s mock explicitly.
/// - A run with `BB_REFUSE_NON_LOOPBACK_API` set (the variable every integration test must set on every run of
///   the binary, enforced by `tests/api_guard.rs`) never touches the OS keyring either: a test must not read or
///   write the developer's keychain. That only ever makes `bb` refuse to sign in or leave a file as it is.
pub(crate) fn os_backend() -> &'static dyn SecretBackend {
    #[cfg(test)]
    {
        static UNIT_TESTS: Unavailable = Unavailable("unit tests never use the OS keyring");
        &UNIT_TESTS
    }
    #[cfg(not(test))]
    {
        static HARNESS: Unavailable =
            Unavailable("BB_REFUSE_NON_LOOPBACK_API is set (a test run), so the OS keyring is not used");
        if crate::config::test_harness_run() {
            return &HARNESS;
        }
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        {
            static OS: std::sync::LazyLock<Guarded<Keyring<OsEntry>>> = std::sync::LazyLock::new(|| {
                Guarded::new(Keyring(os_entry as OsEntry), KEYRING_TIMEOUT, KEYRING_BACKOFF)
            });
            &*OS
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            static NONE: Unavailable = Unavailable("bb has no keyring support on this platform");
            &NONE
        }
    }
}

/// Did the person opt in to the credentials file through the environment (`BB_STORE_SECRETS_IN_FILE=1`)?
pub fn opted_in_by_env() -> bool {
    std::env::var(OPT_IN_ENV).is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true"))
}

/// F3, verbatim: printed on every command while a secret is in a file.
pub fn unencrypted_warning(path: &Path) -> String {
    format!(
        "Your Beebeeb key is stored unencrypted in {}. Anyone who can read this file can decrypt your files.",
        path.display()
    )
}

/// What to do when the keyring exists but refused or did not answer: allow it once, where it can ask. A keychain
/// prompt cannot reach a person over SSH or from a background job, which is where this happens after `bb update`
/// replaced the binary (review P2-1).
fn allow_access_hint(command: &str) -> String {
    let machine = if cfg!(target_os = "macos") {
        "that Mac"
    } else {
        "that machine"
    };
    format!("Run `{command}` once from a terminal on {machine} and allow bb access when the keyring asks.")
}

/// F3: `bb login` without a keyring and without the opt-in. A keyring that exists but refused gets no opt-in
/// sentence: the fix is to allow access, not to store the key unencrypted.
pub fn refusal(reason: &str) -> String {
    if keyring_refused(reason) {
        return format!(
            "The OS keyring on this machine did not allow bb to store your Beebeeb key ({reason}). {}",
            allow_access_hint("bb login")
        );
    }
    format!(
        "No OS keyring is reachable on this machine ({reason}), so bb will not store your Beebeeb key here. To \
         store it unencrypted in a file only you can read, run `bb login {OPT_IN_FLAG}` (or set {OPT_IN_ENV}=1)."
    )
}

/// What a command tells the person about where their secrets are (stderr, once per process).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    /// F3: a secret is in a file (the opt-in file, or a `config.json` not yet migrated).
    Unencrypted { path: PathBuf },
    /// F5 without a keyring and without the opt-in: the old `config.json` stays as it is.
    NotMigrated { path: PathBuf, reason: String },
    /// F5: stored in the keyring, but reading it back failed or differed, so nothing else changed.
    ReadBackFailed { path: PathBuf, reason: String },
    /// F5: the secrets were stored, verified, and then `config.json` could not be rewritten.
    RewriteFailed { path: PathBuf, reason: String },
    /// F5 done: the secrets left `config.json`.
    Migrated { to: CredentialStore },
    /// F5 on a `config.json` that is a symlink: the link was replaced by a file without the secrets, but the file it
    /// pointed to was not touched and still holds them (review P3-3).
    MigratedFromLink { to: CredentialStore, target: PathBuf },
    /// `credentials.json` can be read by group or others: refused, as ssh refuses such a key file (review P3-8).
    TooOpen { path: PathBuf },
    /// The keyring (or the credentials file) could not be read, so this run is signed out. `keyring_refused`: the
    /// keyring exists but refused or did not answer ([`keyring_refused`]).
    Unreadable { reason: String, keyring_refused: bool },
}

impl Notice {
    pub(crate) fn text(&self) -> String {
        match self {
            Notice::Unencrypted { path } => unencrypted_warning(path),
            Notice::NotMigrated { path, reason } if keyring_refused(reason) => format!(
                "{} The OS keyring did not allow bb to move it there ({reason}). {}",
                unencrypted_warning(path),
                allow_access_hint("bb ls")
            ),
            Notice::NotMigrated { path, reason } => format!(
                "{} bb could not move it into the OS keyring ({reason}). To move it into a file only you can read, \
                 run a bb command once with {OPT_IN_ENV}=1; or sign in with `bb login` on a machine with a keyring.",
                unencrypted_warning(path)
            ),
            Notice::ReadBackFailed { path, reason } => format!(
                "{} bb stored it in the OS keyring but could not read it back ({reason}), so {} was left as it is; \
                 bb will try again next time.",
                unencrypted_warning(path),
                path.display()
            ),
            Notice::RewriteFailed { path, reason } => format!(
                "{} Your sign-in is now also in the OS keyring, but bb could not rewrite the file without it \
                 ({reason}); bb will try again next time.",
                unencrypted_warning(path)
            ),
            Notice::Migrated {
                to: CredentialStore::Keyring,
            } => "Moved your Beebeeb key and session out of config.json into the OS keyring.".to_string(),
            Notice::Migrated {
                to: CredentialStore::File,
            } => format!(
                "Moved your Beebeeb key and session out of config.json into {CREDENTIALS_FILE}, readable only by you."
            ),
            Notice::MigratedFromLink { to, target } => format!(
                "Your Beebeeb key and session are now in {}, and config.json was a link, so bb replaced the link \
                 with a file without them. The file it pointed to, {}, still holds your key: remove the \
                 \"master_key\" and \"session_token\" lines from it, or delete it.",
                match to {
                    CredentialStore::Keyring => "the OS keyring".to_string(),
                    CredentialStore::File => format!("{CREDENTIALS_FILE}, readable only by you"),
                },
                target.display()
            ),
            Notice::TooOpen { path } => format!(
                "{} can be read by other users, so bb will not use it and this run is signed out. Run `chmod 600 {}` \
                 to make it readable only by you.",
                path.display(),
                path.display()
            ),
            Notice::Unreadable {
                reason,
                keyring_refused: true,
            } => format!(
                "bb could not read your sign-in ({reason}). Commands that need it will fail. {}",
                allow_access_hint("bb ls")
            ),
            Notice::Unreadable { reason, .. } => format!(
                "bb could not read your sign-in ({reason}). Commands that need it will fail; run `bb login` to sign \
                 in again."
            ),
        }
    }
}

/// Print each notice once per process (`load_config` runs many times per command).
pub(crate) fn print_once(notices: &[Notice]) {
    static PRINTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut printed = PRINTED.lock().unwrap_or_else(|e| e.into_inner());
    let printed = printed.get_or_insert_with(HashSet::new);
    for notice in notices {
        let text = notice.text();
        if printed.insert(text.clone()) {
            eprintln!("  {}", text);
        }
    }
}

#[cfg(test)]
pub(crate) mod mock {
    //! `keyring`'s own mock credentials as a [`SecretBackend`]. Its entries keep nothing between `Entry` values, so
    //! this keeps one `Entry` per account; failures are injected through `MockCredential::set_error`, i.e. through
    //! the same error path the OS keyring takes.
    use super::*;
    use keyring::mock::MockCredential;
    use std::collections::HashMap;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Fault {
        None,
        /// Every store fails (no keyring reachable).
        Sets,
        /// Every read fails (the read-back after a store).
        Gets,
        /// Every read returns another value than the one stored.
        Corrupt,
        /// Every delete fails.
        Deletes,
        /// A keyring that exists but refuses access (a denied keychain): stores and reads fail.
        Denied,
    }

    /// An OS status as `security-framework` shows it (`Error { code: -25293 }`).
    pub struct MockOsStatus(pub i32);
    impl std::fmt::Debug for MockOsStatus {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Error").field("code", &self.0).finish()
        }
    }
    impl std::fmt::Display for MockOsStatus {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "mock: the user denied access to the keychain item (OSStatus {})",
                self.0
            )
        }
    }
    impl std::error::Error for MockOsStatus {}

    pub struct MockKeyring {
        entries: Mutex<HashMap<String, Arc<keyring::Entry>>>,
        pub fault: Mutex<Fault>,
        /// Every call first sleeps this long (a keyring waiting on a prompt).
        pub delay: Mutex<std::time::Duration>,
        /// How many calls reached the mock.
        pub calls: std::sync::atomic::AtomicUsize,
    }

    impl MockKeyring {
        pub fn new() -> Self {
            Self {
                entries: Mutex::new(HashMap::new()),
                fault: Mutex::new(Fault::None),
                delay: Mutex::new(std::time::Duration::ZERO),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        pub fn with_fault(fault: Fault) -> Self {
            let mock = Self::new();
            *mock.fault.lock().unwrap() = fault;
            mock
        }

        pub fn set_fault(&self, fault: Fault) {
            *self.fault.lock().unwrap() = fault;
        }

        fn entry(&self, account: &str) -> keyring::Result<Arc<keyring::Entry>> {
            let mut entries = self.entries.lock().unwrap();
            if let Some(entry) = entries.get(account) {
                return Ok(entry.clone());
            }
            let credential = keyring::mock::default_credential_builder().build(None, SERVICE, account)?;
            let entry = Arc::new(keyring::Entry::new_with_credential(credential));
            entries.insert(account.to_string(), entry.clone());
            Ok(entry)
        }

        fn fail_next(&self, account: &str) {
            let entry = self.entry(account).unwrap();
            let credential: &MockCredential = entry.get_credential().downcast_ref().unwrap();
            credential.set_error(if *self.fault.lock().unwrap() == Fault::Denied {
                // What this platform's keyring returns for a store that is there and said no.
                if cfg!(target_os = "macos") {
                    keyring::Error::PlatformFailure(Box::new(MockOsStatus(-25293)))
                } else {
                    keyring::Error::NoStorageAccess("mock: the user denied access to the keychain item".into())
                }
            } else {
                keyring::Error::PlatformFailure(
                    "mock: org.freedesktop.secrets is not provided by any .service files".into(),
                )
            });
        }

        /// The delay and the call count every call goes through.
        fn arrive(&self) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let delay = *self.delay.lock().unwrap();
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }

        /// The stored secrets for `account`, straight from the mock (no fault applied).
        pub fn stored(&self, account: &str) -> Option<Secrets> {
            Keyring(|a: &str| self.entry(a))
                .get(account)
                .unwrap()
                .map(|bytes| decode(&bytes).unwrap())
        }
    }

    impl SecretBackend for MockKeyring {
        fn set(&self, account: &str, secret: &[u8]) -> Result<(), String> {
            self.arrive();
            if matches!(*self.fault.lock().unwrap(), Fault::Sets | Fault::Denied) {
                self.fail_next(account);
            }
            Keyring(|a: &str| self.entry(a)).set(account, secret)
        }

        fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
            self.arrive();
            let fault = *self.fault.lock().unwrap();
            match fault {
                Fault::Gets | Fault::Denied => self.fail_next(account),
                Fault::Corrupt => {
                    let other = Secrets {
                        master_key: Some(Zeroizing::new("b3RoZXI=".to_string())),
                        session_token: Some(Zeroizing::new("other".to_string())),
                    };
                    return Ok(Some(encode(&other)));
                }
                _ => {}
            }
            Keyring(|a: &str| self.entry(a)).get(account)
        }

        fn delete(&self, account: &str) -> Result<bool, String> {
            self.arrive();
            if *self.fault.lock().unwrap() == Fault::Deletes {
                self.fail_next(account);
            }
            Keyring(|a: &str| self.entry(a)).delete(account)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Fault, MockKeyring, MockOsStatus};
    use super::*;

    fn both() -> Secrets {
        Secrets {
            master_key: Some(Zeroizing::new(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
            )),
            session_token: Some(Zeroizing::new("fixture-session-token".to_string())),
        }
    }

    #[test]
    fn the_payload_round_trips_and_its_errors_never_quote_it() {
        let secrets = both();
        let bytes = encode(&secrets);
        assert_eq!(decode(&bytes).unwrap(), secrets);
        let broken = br#"{"v":1,"master_key":12,"session_token":"fixture-session-token"}"#;
        let err = decode(broken).unwrap_err();
        assert!(!err.contains("fixture-session-token"), "the error quotes the input");
        let future = br#"{"v":2,"session_token":"fixture-session-token"}"#;
        let err = decode(future).unwrap_err();
        assert!(
            err.contains("version 2") && !err.contains("fixture"),
            "the error must name the version and not quote the input"
        );
    }

    #[test]
    fn debug_output_hides_both_secrets() {
        let text = format!("{:?}", both());
        assert!(
            !text.contains("AAAA") && !text.contains("fixture-session-token"),
            "Debug shows a secret"
        );
    }

    /// F7 (b): the keyring path against `keyring`'s mock credentials: store, read back, delete.
    #[test]
    fn the_keyring_backend_stores_reads_back_and_deletes_one_entry_per_account() {
        let mock = MockKeyring::new();
        let a = account("http://127.0.0.1:3001", Some("a@beebeeb.io"));
        let b = account("http://127.0.0.1:3001", Some("b@beebeeb.io"));
        assert_ne!(a, b, "two accounts never share an entry");
        assert_ne!(
            a,
            account("http://127.0.0.1:4001", Some("a@beebeeb.io")),
            "nor two servers"
        );
        assert!(mock.get(&a).unwrap().is_none());
        mock.set(&a, &encode(&both())).unwrap();
        verify(&mock, &a, &both()).unwrap();
        assert!(mock.get(&b).unwrap().is_none());
        assert!(mock.delete(&a).unwrap());
        assert!(!mock.delete(&a).unwrap(), "a second delete finds nothing");
        assert!(mock.get(&a).unwrap().is_none());
    }

    #[test]
    fn keyring_failures_become_messages_without_the_secret() {
        let mock = MockKeyring::with_fault(Fault::Sets);
        let a = account("http://127.0.0.1:3001", Some("a@beebeeb.io"));
        let err = mock.set(&a, &encode(&both())).unwrap_err();
        assert!(err.contains("org.freedesktop.secrets"), "unexpected error text");
        mock.set_fault(Fault::None);
        mock.set(&a, &encode(&both())).unwrap();
        mock.set_fault(Fault::Corrupt);
        let err = verify(&mock, &a, &both()).unwrap_err();
        assert!(err.contains("reads back different"), "unexpected error text");
        assert!(!err.contains("fixture-session-token"), "the error quotes the input");
        // A BadEncoding error carries the stored bytes; its description must not.
        let described = describe(&keyring::Error::BadEncoding(b"fixture-session-token".to_vec()));
        assert!(
            !described.contains("fixture"),
            "the description quotes the stored bytes"
        );
    }

    #[test]
    fn the_warning_is_the_contracts_sentence() {
        assert_eq!(
            unencrypted_warning(Path::new("/home/x/.config/beebeeb/credentials.json")),
            "Your Beebeeb key is stored unencrypted in /home/x/.config/beebeeb/credentials.json. Anyone who can \
             read this file can decrypt your files."
        );
        let refusal = refusal("no Secret Service");
        assert!(refusal.contains("bb login --store-secrets-in-file"), "{refusal}");
        assert!(refusal.contains("BB_STORE_SECRETS_IN_FILE=1"), "{refusal}");
    }

    /// Review P3-1: after a call times out, the next calls fail at once with the same reason and never reach the
    /// keyring again until the back-off is over.
    #[test]
    fn a_keyring_call_that_timed_out_is_not_retried_during_the_back_off() {
        let mock = MockKeyring::new();
        *mock.delay.lock().unwrap() = std::time::Duration::from_millis(400);
        let guarded = Guarded::new(
            mock,
            std::time::Duration::from_millis(50),
            std::time::Duration::from_secs(5 * 60),
        );
        let a = account("http://127.0.0.1:3001", Some("a@beebeeb.io"));
        let first = guarded.get(&a).unwrap_err();
        assert!(first.starts_with(NO_ANSWER), "unexpected error text: {first}");
        let started = std::time::Instant::now();
        let second = guarded.get(&a).unwrap_err();
        let third = guarded.set(&a, b"x").unwrap_err();
        let waited = started.elapsed();
        assert_eq!(second, first, "a call during the back-off gets the same reason");
        assert_eq!(third, first);
        assert!(
            waited < std::time::Duration::from_millis(40),
            "a call during the back-off blocked for {waited:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
        let calls = guarded.inner.calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(calls, 1, "the keyring was called again during the back-off");
    }

    /// Review P2-1: a keyring that exists but refused never sends the person to the plaintext opt-in; one that is
    /// not reachable at all still does.
    #[test]
    fn a_refusing_keyring_is_not_answered_with_the_plaintext_opt_in() {
        let a = account("http://127.0.0.1:3001", Some("a@beebeeb.io"));
        let timed_out = format!("{NO_ANSWER} within 60 seconds (is it locked, or waiting on a prompt?)");
        let mut reasons = vec![timed_out];
        // Windows Credential Manager has no "there but refused" answer; a timeout is the only such case there.
        if !cfg!(target_os = "windows") {
            reasons.push(MockKeyring::with_fault(Fault::Denied).set(&a, b"x").unwrap_err());
        }
        let path = PathBuf::from("/home/x/.config/beebeeb/config.json");
        for reason in reasons.iter().map(String::as_str) {
            let texts = [
                refusal(reason),
                Notice::NotMigrated {
                    path: path.clone(),
                    reason: reason.to_string(),
                }
                .text(),
                Notice::Unreadable {
                    reason: reason.to_string(),
                    keyring_refused: keyring_refused(reason),
                }
                .text(),
            ];
            for text in texts {
                assert!(
                    !text.contains(OPT_IN_FLAG) && !text.contains(OPT_IN_ENV),
                    "a refusing keyring is answered with the opt-in: {text}"
                );
                assert!(
                    text.contains("did not allow access") || text.contains("did not answer"),
                    "{text}"
                );
                assert!(text.contains("from a terminal"), "{text}");
            }
        }
        let missing = MockKeyring::with_fault(Fault::Sets).set(&a, b"x").unwrap_err();
        assert!(!keyring_refused(&missing));
        // macOS: no keychain at all (errSecNoSuchKeychain) is "not reachable", not "refused".
        if cfg!(target_os = "macos") {
            let none = describe(&keyring::Error::NoStorageAccess(Box::new(MockOsStatus(-25294))));
            assert!(!keyring_refused(&none), "{none}");
        }
        assert!(refusal(&missing).contains(OPT_IN_FLAG));
        assert!(
            Notice::NotMigrated { path, reason: missing }
                .text()
                .contains(OPT_IN_ENV)
        );
    }

    /// The real keychain, on purpose and only when asked: `cargo test --bin bb credentials -- --ignored`. It uses
    /// a service name of its own and removes its entry, so it never meets a real sign-in.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    #[test]
    #[ignore = "touches the real OS keyring; run by hand on the machine whose keyring is under test"]
    fn the_os_keyring_round_trips_on_this_machine() {
        let service = format!("io.beebeeb.cli.selftest-{}", uuid::Uuid::new_v4());
        let account = account("http://127.0.0.1:3001", Some("selftest@beebeeb.io"));
        let backend = Guarded::new(
            Keyring(move |a: &str| keyring::Entry::new(&service, a).map(Arc::new)),
            KEYRING_TIMEOUT,
            KEYRING_BACKOFF,
        );
        let result = (|| -> Result<(), String> {
            backend.set(&account, &encode(&both()))?;
            verify(&backend, &account, &both())?;
            if !backend.delete(&account)? {
                return Err("the entry was not there to delete".to_string());
            }
            if backend.get(&account)?.is_some() {
                return Err("the entry is still there after the delete".to_string());
            }
            Ok(())
        })();
        let _ = backend.delete(&account);
        assert!(
            result.is_ok(),
            "the OS keyring round trip failed: {}",
            result.unwrap_err()
        );
    }
}
