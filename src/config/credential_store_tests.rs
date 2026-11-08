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

/// Review P3-5: config.json is the only pointer to the entry, so it stays for a retry of `bb logout`.
#[test]
fn a_keyring_entry_that_cannot_be_deleted_is_reported_and_config_json_is_kept_for_a_retry() {
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
    assert!(
        err.contains("`bb logout`"),
        "the error must say logout can be retried: {err}"
    );
    assert!(
        dir.join(CONFIG_FILE).exists(),
        "config.json must stay so logout can be retried"
    );
    mock.set_fault(Fault::None);
    clear_in(&dir, &mock).unwrap();
    assert_eq!(mock.stored(&account()), None, "the retry must delete the entry");
    assert!(!dir.join(CONFIG_FILE).exists());
}

/// Review P2-2: with the `file` marker, logout still removes a leftover keyring entry of the same account.
#[test]
fn logout_with_the_file_marker_also_removes_a_leftover_keyring_entry() {
    let dir = tempdir();
    let mock = MockKeyring::new();
    mock.set(&account(), &credentials::encode(&both())).unwrap();
    save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).unwrap();
    assert_eq!(on_disk(&dir, CONFIG_FILE)["credential_store"], "file");
    clear_in(&dir, &mock).unwrap();
    assert_eq!(
        mock.stored(&account()),
        None,
        "the leftover keyring entry survived logout"
    );
    assert!(!dir.join(CREDENTIALS_FILE).exists() && !dir.join(CONFIG_FILE).exists());
    // A failing best-effort delete is not an error for the `file` marker.
    save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).unwrap();
    clear_in(&dir, &MockKeyring::with_fault(Fault::Deletes)).unwrap();
}

/// Review P3-2: a temporary left by a crash between create and rename (it holds secrets) goes with logout.
#[test]
fn logout_removes_leftover_temporary_files() {
    let dir = tempdir();
    let mock = MockKeyring::new();
    save_login_in(&dir, &mock, API, EMAIL, &both(), false).unwrap();
    let planted = [
        dir.join(format!(".{CREDENTIALS_FILE}.{}.tmp", uuid::Uuid::new_v4())),
        dir.join(format!(".{CONFIG_FILE}.{}.tmp", uuid::Uuid::new_v4())),
    ];
    for path in &planted {
        std::fs::write(path, credentials::encode(&both()).as_slice()).unwrap();
    }
    clear_in(&dir, &mock).unwrap();
    for path in &planted {
        assert!(!path.exists(), "{} survived logout", path.display());
    }
}

/// Review P3-8: a credentials.json that group or others can read is refused, as ssh refuses a wide key file.
#[cfg(unix)]
#[test]
fn a_credentials_file_readable_by_others_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir();
    save_login_in(&dir, &Unavailable("no keyring"), API, EMAIL, &both(), true).unwrap();
    let path = dir.join(CREDENTIALS_FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let loaded = load(&dir, &Unavailable("no keyring"), true);
    assert!(
        loaded.config.master_key.is_none() && loaded.config.session_token.is_none(),
        "a 0644 credentials.json was used"
    );
    let texts: Vec<String> = loaded.notices.iter().map(Notice::text).collect();
    let text = texts.join("\n");
    assert!(
        text.contains(&path.display().to_string()) && text.contains("chmod 600"),
        "the notice must name the path and chmod 600: {text}"
    );
    assert!(
        !text.contains(MASTER_KEY) && !text.contains(TOKEN),
        "a notice quotes a secret"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_signed_in(&load(&dir, &Unavailable("no keyring"), true).config);
}

/// Review P3-3: a symlinked config.json. The notice names the link's target, says it still holds the key, and
/// does not claim the key moved.
#[cfg(unix)]
#[test]
fn a_symlinked_config_json_is_not_reported_as_moved() {
    let dir = tempdir();
    let real = tempdir().join("dotfiles-config.json");
    legacy(&dir);
    std::fs::rename(dir.join(CONFIG_FILE), &real).unwrap();
    std::os::unix::fs::symlink(&real, dir.join(CONFIG_FILE)).unwrap();
    let mock = MockKeyring::new();
    let loaded = load(&dir, &mock, false);
    assert_signed_in(&loaded.config);
    assert!(
        !loaded.notices.iter().any(|n| matches!(n, Notice::Migrated { .. })),
        "the notice claims the key moved: {:?}",
        loaded.notices
    );
    let target = real.canonicalize().unwrap().display().to_string();
    let text: Vec<String> = loaded.notices.iter().map(Notice::text).collect();
    assert!(
        text.iter().any(|t| t.contains(&target) && t.contains("still holds")),
        "no notice names the target that still holds the key: {text:?}"
    );
    assert!(
        !text.iter().any(|t| t.contains(MASTER_KEY) || t.contains(TOKEN)),
        "a notice quotes a secret"
    );
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
