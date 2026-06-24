//! Cross-platform compatibility vectors for the CLI login handshake, validated
//! against `beebeeb_core::cli_auth`.
//!
//! These vectors were captured from a real browser **Node.js WebCrypto** run
//! (the old `test_ecdh_compat.mjs`): a fixed CLI P-256 private key + a fixed browser
//! P-256 public key, the WebCrypto-derived ECDH shared secret, the HKDF-SHA256
//! (info "beebeeb-cli-auth-v1") AES key, and a real `{nonce, ciphertext}` for the
//! `{session_token, master_key_b64, email}` payload. They are the strongest drift
//! guard we have: they prove `beebeeb-core` produces output BYTE-IDENTICAL to the
//! browser web client, so a `bb login` against any web build keeps working.
//!
//! The handshake crypto itself now lives ENTIRELY in `beebeeb_core::cli_auth` — this
//! test no longer hand-rolls P-256/HKDF/AES; it only drives the core API.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use beebeeb_core::cli_auth::{CliEphemeralKey, decrypt_cli_payload, derive_cli_auth_key};

fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn hex32(hex: &str) -> [u8; 32] {
    hex_decode(hex).try_into().unwrap()
}

// ---- WebCrypto-captured vector ---------------------------------------------
// Fixed CLI P-256 private scalar (the JWK `d`, base64url-decoded → hex).
const CLI_PRIV_HEX: &str = "3ee0dc854e89db9cd0f877fcc82bd1f3955ac7496f0e8dabd452ce2c6d987a66";
// Fixed browser ephemeral P-256 public key (SEC1 uncompressed, base64).
const BROWSER_PUB_B64: &str =
    "BNFx/4Z8lzU++XyRv+uXz4yavlaXFGxKJQ765zysItlS0cop5JUOkwKXnD0Ez0At/rklj/DetH7mQFSeFONtUqg=";
// Expected ECDH shared secret (32-byte X coordinate) from those two keys.
const EXPECTED_SHARED_HEX: &str = "f27228cf21fdcc6e2e3370bdb71a3e7e09ef251d0ad2ea42a4881417b95dc397";
// Expected HKDF-SHA256(shared, info="beebeeb-cli-auth-v1") AES key.
const EXPECTED_AES_KEY_HEX: &str = "d6f84bc0b0d28dd0f7b9959c16662d54a0df753a47c5f4ef6e59b10ca29b9e1c";
// A real WebCrypto AES-256-GCM payload encrypted under that AES key.
const NONCE_B64: &str = "AQIDBAUGBwgJCgsM";
const CIPHERTEXT_B64: &str = "zPY+LzA6/FJLZH5z/AR+orn3ZmWicxT4+m/ssstYzp0nVvWXjmmS4ndKfZsR15C8WMXUxuaEFxVhqgkelub+32NwZgyL2pJfwPS8LfBdax1J+qXVRKef+Xbm/+2Ngpb38FKbtoS+lJQ=";
const EXPECTED_PLAINTEXT: &str =
    r#"{"session_token":"test-token","master_key_b64":"dGVzdC1rZXk=","email":"test@beebeeb.io"}"#;

#[test]
fn ecdh_shared_secret_matches_webcrypto() {
    // core's CliEphemeralKey, seeded with the fixed CLI private key, must reproduce
    // the exact ECDH shared secret WebCrypto computed.
    let cli = CliEphemeralKey::from_secret_bytes(&hex32(CLI_PRIV_HEX)).unwrap();
    let browser_pub = B64.decode(BROWSER_PUB_B64).unwrap();
    let shared = cli.shared_secret(&browser_pub).unwrap();
    assert_eq!(
        shared.as_slice(),
        hex_decode(EXPECTED_SHARED_HEX).as_slice(),
        "core P-256 ECDH must match WebCrypto"
    );
}

#[test]
fn hkdf_aes_key_matches_webcrypto() {
    let key = derive_cli_auth_key(&hex32(EXPECTED_SHARED_HEX));
    assert_eq!(
        key.to_vec(),
        hex_decode(EXPECTED_AES_KEY_HEX),
        "core HKDF AES key must match WebCrypto (info MUST be beebeeb-cli-auth-v1)"
    );
}

#[test]
fn decrypt_payload_matches_webcrypto() {
    // Decrypt the real WebCrypto ciphertext via the core helper, starting from the
    // shared secret (HKDF path).
    let nonce = B64.decode(NONCE_B64).unwrap();
    let ciphertext = B64.decode(CIPHERTEXT_B64).unwrap();
    let plaintext = decrypt_cli_payload(&hex32(EXPECTED_SHARED_HEX), &nonce, &ciphertext).unwrap();
    assert_eq!(String::from_utf8(plaintext).unwrap(), EXPECTED_PLAINTEXT);
}

#[test]
fn full_flow_ecdh_to_plaintext_via_core() {
    // The exact path `bb login` runs: a fixed CLI key + the browser's public key,
    // nonce, and ciphertext → core does ECDH + HKDF + AES-GCM and returns plaintext.
    let cli = CliEphemeralKey::from_secret_bytes(&hex32(CLI_PRIV_HEX)).unwrap();
    let browser_pub = B64.decode(BROWSER_PUB_B64).unwrap();
    let nonce = B64.decode(NONCE_B64).unwrap();
    let ciphertext = B64.decode(CIPHERTEXT_B64).unwrap();

    let plaintext = cli.decrypt_browser_payload(&browser_pub, &nonce, &ciphertext).unwrap();
    assert_eq!(String::from_utf8(plaintext).unwrap(), EXPECTED_PLAINTEXT);
}

// The legacy v0.4 raw-shared-secret fallback is exercised by core's own unit test
// `beebeeb_core::cli_auth::tests::raw_secret_fallback_roundtrip` (so the CLI test
// suite no longer needs to pull aes-gcm to construct a fallback ciphertext).
