//! Task 2266: an approved legacy invite's owner copy is the folder key only when core can prove it
//! (`kdf::open_legacy_invite_owner_copy`), or when it is the purpose-bound format
//! (`kdf::unwrap_invite_owner_copy`). Anything else is "This folder's keys are unavailable": no key, never a mint.
#[path = "support/shared_folder.rs"]
mod sf;
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use beebeeb_core::{
    encrypt::encrypt_chunk_raw,
    kdf::{self, FileKey},
};
use serde_json::{Value, json};
use sf::*;

/// The 32-byte name JSON of a folder: valid UTF-8, sealed under the folder's own derived key.
const NAME_JSON: &[u8; 32] = br#"{"name":"Docs","mime_type":null}"#;

fn seal(key: &FileKey, inner: &[u8]) -> String {
    B64.encode(encrypt_chunk_raw(key, inner).unwrap())
}
fn name_key(id: &str) -> FileKey {
    kdf::derive_file_key(&master(), id.as_bytes())
}
fn owner_derived(id: &str) -> Vec<u8> {
    name_key(id).as_bytes().to_vec()
}
fn wire(blob: beebeeb_types::EncryptedBlob) -> String {
    B64.encode([blob.nonce, blob.ciphertext].concat())
}
/// A never-seen folder with an approved legacy invite; FILE is a member upload wrapped under `key`.
fn legacy(key: &FileKey) -> Fixture {
    let mut f = Fixture::transfer(false);
    f.legacy_invite = true;
    f.custody_status = Some(404);
    f.rows[1]["owner_wrapped_file_key"] = Value::Null;
    f.inventory[0]["encrypted_file_key"] = json!(seal(key, item_key().as_bytes()));
    f
}
async fn pull(h: &Harness, name: &str) -> (std::process::Output, std::path::PathBuf) {
    let output = h.home.join(name);
    (h.run(&["pull", FILE, "-o", output.to_str().unwrap()]).await, output)
}
fn refused(h: &Harness, out: &std::process::Output, output: &std::path::Path) {
    assert!(
        !out.status.success(),
        "an unproven owner copy opened the folder: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("This folder's keys are unavailable"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!output.exists(), "plaintext was published");
    let f = h.state.lock().unwrap();
    assert_eq!(f.requests.iter().filter(|(m, _, _)| m != "GET").count(), 0);
    assert_eq!(
        f.requests.iter().filter(|(_, p, _)| p.ends_with("/download")).count(),
        0
    );
}

/// The folder's own name blob is offered as the owner copy, and a row passes possession under it (a server that
/// knows one owner-derived child key can build one). FILE is wrapped under the candidate, so a reader that
/// accepts it would open FILE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_blob_candidate_is_refused() {
    let candidate = FileKey::from_bytes(*NAME_JSON);
    let mut f = legacy(&candidate);
    f.invite_owner_copy = Some(seal(&name_key(ROOT), NAME_JSON));
    f.invite_rows = Some(vec![
        f.inventory[0].clone(),
        json!({"file_id": PROOF_ID, "encrypted_file_key": seal(&candidate, &owner_derived(PROOF_ID))}),
    ]);
    let h = Harness::new(f).await;
    let (out, output) = pull(&h, "name-blob.txt").await;
    refused(&h, &out, &output);
    h.close().await;
}

/// A key that is not text, but no row proves it: every row opens to something other than its own owner-derived key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forged_rows_are_refused() {
    let candidate = FileKey::from_bytes([0xF8; 32]);
    let mut f = legacy(&candidate);
    f.invite_owner_copy = Some(seal(&name_key(ROOT), candidate.as_bytes()));
    f.invite_rows = Some(vec![
        f.inventory[0].clone(),
        // opens to a key, but not to this id's owner-derived key
        json!({"file_id": PROOF_ID, "encrypted_file_key": seal(&candidate, &[0x5A; 32])}),
        // opens to ANOTHER id's owner-derived key
        json!({"file_id": SUB, "encrypted_file_key": seal(&candidate, &owner_derived(PROOF_ID))}),
    ]);
    let h = Harness::new(f).await;
    let (out, output) = pull(&h, "forged.txt").await;
    refused(&h, &out, &output);
    h.close().await;
}

/// The honest flag-off flow: a non-text key and an owner-derived child row proving it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proven_legacy_copy_opens() {
    let h = Harness::new(legacy(&folder_key())).await;
    let (out, output) = pull(&h, "proven.txt").await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&output).unwrap(), h.state.lock().unwrap().plaintext);
    h.close().await;
}

/// The purpose-bound copy needs no proof row: it is authenticated by its purpose and folder id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purpose_bound_copy_opens() {
    let mut f = legacy(&folder_key());
    f.invite_owner_copy = Some(wire(
        kdf::wrap_invite_owner_copy(&master(), ROOT.as_bytes(), &folder_key()).unwrap(),
    ));
    f.invite_rows = Some(f.inventory.clone());
    let h = Harness::new(f).await;
    let (out, output) = pull(&h, "bound.txt").await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(&output).unwrap(), h.state.lock().unwrap().plaintext);
    h.close().await;
}

/// A purpose-bound copy sealed for another folder does not open here, and does not open as a legacy copy either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_purpose_bound_copy_for_another_folder_is_refused() {
    let mut f = legacy(&folder_key());
    f.invite_owner_copy = Some(wire(
        kdf::wrap_invite_owner_copy(&master(), SUB.as_bytes(), &folder_key()).unwrap(),
    ));
    let h = Harness::new(f).await;
    let (out, output) = pull(&h, "other-folder.txt").await;
    refused(&h, &out, &output);
    h.close().await;
}

impl Harness {
    async fn run(&self, args: &[&str]) -> std::process::Output {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_bb"))
            .args(args)
            .args(["--api", &self.api, "--json"])
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("BB_REFUSE_NON_LOOPBACK_API", "1")
            .env("BB_NO_UPDATE", "1")
            .env("BB_TRANSPORT", "api")
            .env_remove("APP_URL")
            .output()
            .await
            .unwrap()
    }
}
