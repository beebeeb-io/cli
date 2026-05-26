use aes_gcm::aead::Aead;
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use hkdf::Hkdf;
use sha2::Sha256;

fn hex_decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn hkdf_matches_webcrypto() {
    // Test vector from Node.js WebCrypto (test_ecdh_compat.mjs)
    let shared_secret = hex_decode("f27228cf21fdcc6e2e3370bdb71a3e7e09ef251d0ad2ea42a4881417b95dc397");
    let expected_aes_key = hex_decode("d6f84bc0b0d28dd0f7b9959c16662d54a0df753a47c5f4ef6e59b10ca29b9e1c");

    // Replicate the CLI's HKDF derivation
    let hk = Hkdf::<Sha256>::new(None, &shared_secret);
    let mut key_bytes = [0u8; 32];
    hk.expand(b"beebeeb-cli-auth-v1", &mut key_bytes)
        .expect("HKDF expand failed");

    assert_eq!(
        key_bytes.to_vec(),
        expected_aes_key,
        "HKDF-derived AES key must match WebCrypto output"
    );
}

#[test]
fn aes_gcm_decrypt_matches_webcrypto() {
    // Same test vector: use the known AES key, nonce, and ciphertext
    let aes_key = hex_decode("d6f84bc0b0d28dd0f7b9959c16662d54a0df753a47c5f4ef6e59b10ca29b9e1c");
    let nonce_b64 = "AQIDBAUGBwgJCgsM";
    let ciphertext_b64 = "zPY+LzA6/FJLZH5z/AR+orn3ZmWicxT4+m/ssstYzp0nVvWXjmmS4ndKfZsR15C8WMXUxuaEFxVhqgkelub+32NwZgyL2pJfwPS8LfBdax1J+qXVRKef+Xbm/+2Ngpb38FKbtoS+lJQ=";
    let expected_plaintext =
        r#"{"session_token":"test-token","master_key_b64":"dGVzdC1rZXk=","email":"test@beebeeb.io"}"#;

    let cipher = Aes256Gcm::new(GenericArray::from_slice(&aes_key));
    let nonce_bytes = B64.decode(nonce_b64).unwrap();
    let ciphertext = B64.decode(ciphertext_b64).unwrap();

    let plaintext = cipher
        .decrypt(GenericArray::from_slice(&nonce_bytes), ciphertext.as_ref())
        .expect("AES-GCM decryption should succeed with matching key/nonce/ciphertext");

    assert_eq!(
        String::from_utf8(plaintext).unwrap(),
        expected_plaintext,
        "Decrypted plaintext must match original"
    );
}

#[test]
fn full_ecdh_hkdf_decrypt_flow() {
    // Full flow: start from browser's public key, do ECDH, HKDF, decrypt
    use p256::PublicKey;
    use p256::ecdh::EphemeralSecret;

    // Browser's public key from the test vector (base64)
    let browser_pub_b64 = "BNFx/4Z8lzU++XyRv+uXz4yavlaXFGxKJQ765zysItlS0cop5JUOkwKXnD0Ez0At/rklj/DetH7mQFSeFONtUqg=";
    let browser_pub_bytes = B64.decode(browser_pub_b64).unwrap();
    let browser_pub_key =
        PublicKey::from_sec1_bytes(&browser_pub_bytes).expect("Browser public key must be valid SEC1");

    // CLI private key (d parameter from JWK, base64url-encoded)
    // We can't construct EphemeralSecret from bytes, but we can use SecretKey
    let d_b64url = "PuDchU6J25zQ-Hf8yCvR85Vax0lvDo2r1FLOLG2YemY";
    let d_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(d_b64url)
        .unwrap();
    let secret_key =
        p256::SecretKey::from_bytes(GenericArray::from_slice(&d_bytes)).expect("CLI secret key must be valid");

    // ECDH
    let shared_secret = p256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), browser_pub_key.as_affine());
    let shared_bytes = shared_secret.raw_secret_bytes();

    // Verify ECDH shared secret matches
    let expected_shared = hex_decode("f27228cf21fdcc6e2e3370bdb71a3e7e09ef251d0ad2ea42a4881417b95dc397");
    assert_eq!(
        shared_bytes.as_slice(),
        expected_shared.as_slice(),
        "ECDH shared secret must match WebCrypto"
    );

    // HKDF
    let hk = Hkdf::<Sha256>::new(None, shared_bytes.as_slice());
    let mut key_bytes = [0u8; 32];
    hk.expand(b"beebeeb-cli-auth-v1", &mut key_bytes).unwrap();

    // AES-GCM decrypt
    let cipher = Aes256Gcm::new(GenericArray::from_slice(&key_bytes));
    let nonce_bytes = B64.decode("AQIDBAUGBwgJCgsM").unwrap();
    let ciphertext = B64.decode("zPY+LzA6/FJLZH5z/AR+orn3ZmWicxT4+m/ssstYzp0nVvWXjmmS4ndKfZsR15C8WMXUxuaEFxVhqgkelub+32NwZgyL2pJfwPS8LfBdax1J+qXVRKef+Xbm/+2Ngpb38FKbtoS+lJQ=").unwrap();

    let plaintext = cipher
        .decrypt(GenericArray::from_slice(&nonce_bytes), ciphertext.as_ref())
        .expect("Full ECDH+HKDF+AES-GCM decryption must succeed");

    let expected = r#"{"session_token":"test-token","master_key_b64":"dGVzdC1rZXk=","email":"test@beebeeb.io"}"#;
    assert_eq!(String::from_utf8(plaintext).unwrap(), expected);
}

#[test]
fn fallback_to_raw_shared_secret() {
    use p256::PublicKey;

    // Simulate a v0.4 web app: encrypt with raw ECDH shared secret (no HKDF)
    let browser_pub_b64 = "BNFx/4Z8lzU++XyRv+uXz4yavlaXFGxKJQ765zysItlS0cop5JUOkwKXnD0Ez0At/rklj/DetH7mQFSeFONtUqg=";
    let browser_pub_bytes = B64.decode(browser_pub_b64).unwrap();
    let browser_pub_key = PublicKey::from_sec1_bytes(&browser_pub_bytes).unwrap();

    let d_b64url = "PuDchU6J25zQ-Hf8yCvR85Vax0lvDo2r1FLOLG2YemY";
    let d_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(d_b64url)
        .unwrap();
    let secret_key = p256::SecretKey::from_bytes(GenericArray::from_slice(&d_bytes)).unwrap();

    let shared_secret = p256::ecdh::diffie_hellman(secret_key.to_nonzero_scalar(), browser_pub_key.as_affine());
    let shared_bytes = shared_secret.raw_secret_bytes();

    // Encrypt with RAW shared secret (v0.4 web behavior)
    let nonce = GenericArray::from_slice(&[1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    let payload = b"test-payload";
    let raw_cipher = Aes256Gcm::new(GenericArray::from_slice(&shared_bytes[..32]));
    let ciphertext = raw_cipher.encrypt(nonce, payload.as_ref()).unwrap();

    // CLI's new fallback logic: try HKDF first, then raw
    let hk = Hkdf::<Sha256>::new(None, shared_bytes.as_slice());
    let mut hkdf_key = [0u8; 32];
    hk.expand(b"beebeeb-cli-auth-v1", &mut hkdf_key).unwrap();

    let plaintext = {
        let cipher = Aes256Gcm::new(GenericArray::from_slice(&hkdf_key));
        cipher
            .decrypt(nonce, ciphertext.as_ref())
            .or_else(|_| {
                let cipher = Aes256Gcm::new(GenericArray::from_slice(&shared_bytes[..32]));
                cipher.decrypt(nonce, ciphertext.as_ref())
            })
            .expect("Fallback to raw shared secret must work")
    };

    assert_eq!(plaintext, payload, "Must decrypt v0.4 web app payload via fallback");
}
