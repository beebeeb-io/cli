//! Shared crypto helpers for the CLI.
//!
//! The Beebeeb server stores `name_encrypted` in three formats depending on
//! which client uploaded the file:
//!
//! 1. **Rust/core EncryptedBlob** — JSON with `cipher_suite`, `nonce` (byte
//!    array), `ciphertext` (byte array).  Produced by the CLI and any client
//!    using `beebeeb-core` via serde.
//!
//! 2. **Web-app blob** — JSON with `nonce` and `ciphertext` as base64 strings,
//!    no `cipher_suite` field.  Produced by the web client which calls the WASM
//!    `encrypt_metadata` function and base64-encodes the raw `Uint8Array`s.
//!
//! 3. **Plaintext** — a bare string (not JSON at all).  Legacy entries or
//!    server-created folders that predate client-side encryption.
//!
//! [`decrypt_name`] tries all three in order and returns the decrypted filename.
//!
//! Chunk content has a similar cross-client problem:
//!
//! - **CLI chunks** are stored as JSON-serialized `EncryptedBlob` objects.
//! - **Web-app chunks** are stored as raw binary: `nonce (12 bytes) | ciphertext`.
//!
//! Additionally, the file key used to encrypt chunks may have been derived from
//! either the UUID's 16-byte binary form (CLI) or the UUID string as UTF-8 bytes
//! (web app).
//!
//! [`decrypt_file_chunks`] handles all combinations transparently. The format is
//! never trusted from the first byte alone (a raw nonce starts with `{` 1 time
//! in 256): both parsers are tried, and AEAD authentication is the arbiter.
//!
//! **Chunk format pin (tasks 1908 / 1943).** Every file-content frame is opened
//! by core's pin-aware reader ([`ChunkDecryptor`], [`open_chunk`]) under the
//! [`ExpectedChunkFormat`] the file's AUTHENTICATED metadata records
//! ([`content_pin`]), never a server field and never the generic
//! `decrypt_chunk(file_key, blob)`. A v2-pinned file opens only as v2: one
//! key (core's string-UUID derivation), raw frames, no JSON parser and no v1
//! fallback. The legacy combinations above apply only to `Unrecorded` files.

use beebeeb_core::chunk_stream::{ChunkDecryptor, ChunkFormat, ExpectedChunkFormat, open_chunk};
use beebeeb_core::kdf::FileKey;
use beebeeb_types::EncryptedBlob;

#[cfg(test)]
mod tests {
    use super::*;
    use beebeeb_core::chunk_stream::ChunkDecryptor;
    use beebeeb_core::encrypt::{encrypt_chunk, encrypt_chunk_raw};
    use beebeeb_core::kdf::{MasterKey, OwnerKeyConvention, derive_file_key, derive_master_key};

    /// Task 1759: a raw-format file whose random nonce starts with 0x7b must
    /// still decrypt (was misrouted to the JSON parser: "key must be a string").
    #[test]
    fn raw_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"nonce starts with a brace";
        let frame = (0..100_000)
            .map(|_| encrypt_chunk_raw(&fk, pt).unwrap())
            .find(|f| f[0] == b'{')
            .expect("a nonce starting with 0x7b within 100k tries");
        assert_eq!(frame[0], 0x7b);
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &frame,
                1
            )
            .unwrap(),
            pt
        );
    }

    /// Multi-chunk variant: only the first chunk's nonce byte matters.
    #[test]
    fn raw_multi_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let first = (0..100_000)
            .map(|_| encrypt_chunk_raw(&fk, b"AAAAAAAA").unwrap())
            .find(|f| f[0] == b'{')
            .unwrap();
        let mut all = first;
        all.extend(encrypt_chunk_raw(&fk, b"BBBBBBBB").unwrap());
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &all,
                2
            )
            .unwrap(),
            b"AAAAAAAABBBBBBBB"
        );
    }

    /// Encrypt `plaintext` as the platform's raw multi-chunk wire format:
    /// uniform `chunk_size` plaintext chunks, the last one shorter. When
    /// `brace_first` is set, the FIRST frame is re-rolled until its random
    /// nonce starts with 0x7b ('{'), the byte that used to misroute the file.
    fn raw_file(fk: &beebeeb_core::kdf::FileKey, plaintext: &[u8], chunk_size: usize, brace_first: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, chunk) in plaintext.chunks(chunk_size).enumerate() {
            let frame = if i == 0 && brace_first {
                (0..100_000)
                    .map(|_| encrypt_chunk_raw(fk, chunk).unwrap())
                    .find(|f| f[0] == b'{')
                    .expect("a nonce starting with 0x7b within 100k tries")
            } else {
                encrypt_chunk_raw(fk, chunk).unwrap()
            };
            out.extend_from_slice(&frame);
        }
        out
    }

    /// Task 1759 round 2 (P1-a): a raw multi-chunk file whose LAST chunk is
    /// shorter than the rest, the normal case for any file that is not an
    /// exact multiple of the chunk size, must decrypt. The old splitter used
    /// `total / count` for every frame and mis-sized frame 0.
    #[test]
    fn raw_multi_chunk_uneven_last_chunk_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(3 * 64 + 10)).map(|i| (i % 251) as u8).collect();
        let wire = raw_file(&fk, &pt, 64, false);
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks(
                &m,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
    }

    /// Same, with a '{' first nonce byte on frame 0: both defects at once.
    #[test]
    fn raw_multi_chunk_uneven_last_chunk_with_brace_first_byte_decrypts() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(3 * 64 + 10)).map(|i| (i % 251) as u8).collect();
        let wire = raw_file(&fk, &pt, 64, true);
        assert_eq!(wire[0], b'{');
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks(
                &m,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .unwrap(),
            pt
        );
    }

    /// Frame size recovered from the power-of-two ladder: a pre-V2 1 MiB chunk
    /// file with an uneven last chunk, and a 4 MiB (planner minimum) one.
    #[test]
    fn raw_multi_chunk_ladder_sizes_uneven_last_chunk() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        for cs in [1usize << 20, 4usize << 20] {
            let pt: Vec<u8> = (0..(2 * cs + 12_345)).map(|i| (i % 253) as u8).collect();
            let wire = raw_file(&fk, &pt, cs, false);
            assert_eq!(
                try_decrypt_all_chunks(
                    &fk,
                    FID,
                    beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                    &wire,
                    3
                )
                .unwrap(),
                pt,
                "chunk size {cs}"
            );
        }
    }

    /// Two chunks with a tiny last chunk: the widest feasible range, where the
    /// uniform split `total / 2` is wrong by a lot.
    #[test]
    fn raw_two_chunks_tiny_last_chunk() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = vec![7u8; 1000 + 1];
        let wire = raw_file(&fk, &pt, 1000, true);
        assert_eq!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                2
            )
            .unwrap(),
            pt
        );
    }

    /// With the caller-supplied chunk size the frame is exact (no probing), and
    /// a WRONG hint is not trusted: it falls back to probing and still decrypts.
    #[test]
    fn raw_multi_chunk_chunk_size_hint_exact_and_wrong() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt: Vec<u8> = (0..(5 * 100 + 3)).map(|i| (i % 249) as u8).collect();
        let wire = raw_file(&fk, &pt, 100, true);
        assert_eq!(
            try_decrypt_all_chunks_with_chunk_size(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                6,
                Some(100)
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            try_decrypt_all_chunks_with_chunk_size(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                6,
                Some(4096)
            )
            .unwrap(),
            pt
        );
        assert_eq!(
            decrypt_file_chunks_with_chunk_size(
                &m,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                6,
                Some(100)
            )
            .unwrap(),
            pt
        );
    }

    /// A raw file with a corrupted middle chunk fails (no partial plaintext),
    /// and a wrong key on an uneven multi-chunk file fails.
    #[test]
    fn raw_multi_chunk_corruption_and_wrong_key_fail() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = vec![9u8; 3 * 64 + 10];
        let mut wire = raw_file(&fk, &pt, 64, false);
        assert!(
            try_decrypt_all_chunks(
                &derive_file_key(&m, b"other"),
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .is_err()
        );
        let mid = 64 + 28 + 20;
        wire[mid] ^= 0xff;
        assert!(
            try_decrypt_all_chunks(
                &fk,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &wire,
                4
            )
            .is_err()
        );
    }

    /// P2-c: when neither parser works, the error names BOTH reasons.
    #[test]
    fn failure_reports_both_parser_errors() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let junk = vec![0x41u8; 200];
        let err = try_decrypt_all_chunks(
            &fk,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &junk,
            2,
        )
        .unwrap_err();
        assert!(err.contains("raw parser:") && err.contains("json parser:"), "{err}");
    }

    /// A wrong key must still fail on both parsers (no false success).
    #[test]
    fn brace_first_byte_wrong_key_still_fails() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let other = derive_file_key(&m, b"other");
        let frame = (0..100_000)
            .map(|_| encrypt_chunk_raw(&fk, b"x").unwrap())
            .find(|f| f[0] == b'{')
            .unwrap();
        assert!(
            try_decrypt_all_chunks(
                &other,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &frame,
                1
            )
            .is_err()
        );
    }

    // A fixed file id used across the legacy/dual-key tests.
    const FID: &str = "3e15382b-1111-2222-3333-444455556666";

    fn mk() -> MasterKey {
        derive_master_key("test-password", b"test-salt-16bytes").unwrap()
    }

    /// Modern raw format, key derived from the UUID **string** (current CLI +
    /// web + post-`bb repair`): the common single-chunk case.
    #[test]
    fn raw_string_uuid_single_chunk_roundtrip() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"hello raw world";
        let frame = encrypt_chunk_raw(&fk, pt).unwrap();
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        )
        .unwrap();
        assert_eq!(out, pt);
    }

    /// LEGACY pre-`bb repair` file: key derived from the 16-byte **binary**
    /// UUID. `decrypt_file_chunks` must fall back to the binary-UUID key — this
    /// is the dual-key path the streaming download relies on for legacy files.
    #[test]
    fn raw_binary_uuid_legacy_dual_key_fallback() {
        let m = mk();
        let uuid: uuid::Uuid = FID.parse().unwrap();
        let fk = derive_file_key(&m, uuid.as_bytes());
        let pt = b"legacy binary-uuid file body";
        let frame = encrypt_chunk_raw(&fk, pt).unwrap();
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        )
        .unwrap();
        assert_eq!(out, pt);
    }

    /// LEGACY CLI JSON-blob chunk format (first byte `{`): decrypted via the
    /// buffered path. The streaming download tries the raw frames first; when
    /// frame 0 fails to authenticate it hands the whole payload to this path.
    #[test]
    fn json_blob_legacy_format_detected_and_decrypted() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"legacy json blob chunk";
        let blob = encrypt_chunk(&fk, pt).unwrap();
        let json = serde_json::to_vec(&blob).unwrap();
        assert_eq!(json[0], b'{', "JSON-blob detection relies on a leading brace");
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &json,
            1,
        )
        .unwrap();
        assert_eq!(out, pt);
    }

    /// JSON-blob format combined with the binary-UUID legacy key — both legacy
    /// dimensions at once (the worst-case pre-repair file).
    #[test]
    fn json_blob_with_binary_uuid_key() {
        let m = mk();
        let uuid: uuid::Uuid = FID.parse().unwrap();
        let fk = derive_file_key(&m, uuid.as_bytes());
        let pt = b"json + binary uuid";
        let blob = encrypt_chunk(&fk, pt).unwrap();
        let json = serde_json::to_vec(&blob).unwrap();
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &json,
            1,
        )
        .unwrap();
        assert_eq!(out, pt);
    }

    /// Multi-chunk raw roundtrip with uniform (exact-multiple) chunks — the
    /// `total / count` framing the streaming + buffered paths share.
    #[test]
    fn raw_multi_chunk_uniform_roundtrip() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let chunk = vec![0xABu8; 4096];
        let mut payload = Vec::new();
        for _ in 0..3 {
            payload.extend_from_slice(&encrypt_chunk_raw(&fk, &chunk).unwrap());
        }
        let out = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &payload,
            3,
        )
        .unwrap();
        let mut expected = Vec::new();
        for _ in 0..3 {
            expected.extend_from_slice(&chunk);
        }
        assert_eq!(out, expected);
    }

    /// A wrong master key fails cleanly (no panic) after trying both
    /// derivations.
    #[test]
    fn wrong_key_fails_cleanly() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let frame = encrypt_chunk_raw(&fk, b"data").unwrap();
        let wrong = derive_master_key("other-password", b"other-salt-16byte").unwrap();
        assert!(
            decrypt_file_chunks(
                &wrong,
                FID,
                beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
                &frame,
                1
            )
            .is_err()
        );
    }

    /// The streaming download decryptor (`ChunkDecryptor::for_push`, string-UUID
    /// key) must produce exactly what the buffered path produces for a modern
    /// raw file — guarantees the streaming rewire is behaviour-preserving.
    #[test]
    fn streaming_string_key_matches_buffered() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = b"streaming equals buffered";
        let frame = encrypt_chunk_raw(&fk, pt).unwrap();
        let mut dec = ChunkDecryptor::for_push(&m, FID, beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded);
        let streamed = dec.push_frame(&frame).unwrap().data;
        let buffered = decrypt_file_chunks(
            &m,
            FID,
            beebeeb_core::chunk_stream::ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        )
        .unwrap();
        assert_eq!(streamed, buffered);
        assert_eq!(streamed, pt);
    }

    /// `decrypt_name` handles the plaintext fallback (non-JSON value).
    #[test]
    fn decrypt_name_plaintext_passthrough() {
        let m = mk();
        assert_eq!(decrypt_name(&m, FID, "Documents").as_deref(), Some("Documents"));
    }

    /// Batch `decrypt_names` is byte-identical to per-item `decrypt_name`, order-
    /// preserving, across encrypted / plaintext / undecryptable items (task 0810).
    #[test]
    fn decrypt_names_batch_matches_single() {
        use beebeeb_core::encrypt::encrypt_name;
        let m = mk();
        let id1 = "11111111-1111-2222-3333-444455556666";
        let id2 = "22222222-1111-2222-3333-444455556666";
        let id3 = "33333333-1111-2222-3333-444455556666";
        // id1: Rust-format encrypted name (string-UUID key form, as encrypt_name uses).
        let enc1 = encrypt_name(&m, id1, "report.pdf", Some("application/pdf")).unwrap();
        // id2: plaintext name (fast path). id3: well-formed-but-undecryptable blob.
        let enc2 = "Documents".to_string();
        let enc3 = r#"{"cipher_suite":"V1Aes256Gcm","nonce":[1,2,3],"ciphertext":[9,9,9,9]}"#.to_string();

        let items: Vec<(&str, &str)> = vec![(id1, enc1.as_str()), (id2, enc2.as_str()), (id3, enc3.as_str())];
        let batch = decrypt_names(&m, &items);
        assert_eq!(batch.len(), 3);
        for ((id, enc), got) in items.iter().zip(&batch) {
            assert_eq!(
                *got,
                decrypt_name(&m, id, enc),
                "batch item must equal single decrypt_name"
            );
        }
        assert_eq!(batch[0].as_deref(), Some("report.pdf"));
        assert_eq!(batch[1].as_deref(), Some("Documents"));
        assert_eq!(batch[2], None);
    }

    // ── Task 1943: cross-client vectors for the 1908 core read path ────────

    /// Regenerates `tests/fixtures/core_1908_vectors.json`'s `v2_file` with
    /// core's PUBLIC v2 writer path (a replace of a v2-pinned file:
    /// `with_recorded_pin(V2)` + `encrypt_name_with_chunk_format(V2)`). Run
    /// only on purpose (`cargo test regenerate_core_1908_v2_fixture -- --ignored`)
    /// and commit the output: the vector tests read the PINNED bytes, so a later
    /// core or CLI that drifts from them fails instead of round-tripping with
    /// itself. Core publishes no v2 content vector in `test-vectors/vectors.json`
    /// yet (v4 has only the v1 `chunk_encrypt_decrypt`).
    #[test]
    #[ignore]
    fn regenerate_core_1908_v2_fixture() {
        use beebeeb_core::chunk_stream::{ChunkEncryptor, ExpectedChunkFormat};
        let master = [0x42u8; 32];
        let m = MasterKey::from_bytes(master);
        let file_id = "1908c0de-0000-4000-8000-000000000943";
        // 3 chunks of 64 B plaintext + a 10 B tail: frame splitting, the last
        // flag and the version id all matter.
        let plaintext: Vec<u8> = (0..202u32).map(|i| (i * 7 + 3) as u8).collect();
        let chunk_size = 64u64;
        let mut enc = ChunkEncryptor::for_push_with_chunk_size(&m, file_id, plaintext.len() as u64, chunk_size)
            .unwrap()
            .with_recorded_pin(ExpectedChunkFormat::V2)
            .unwrap();
        let mut wire = Vec::new();
        for part in plaintext.chunks(chunk_size as usize) {
            wire.extend(enc.push_chunk(part).unwrap().data);
        }
        let summary = enc.finish().unwrap();
        let name_encrypted = beebeeb_core::encrypt::encrypt_name_with_chunk_format(
            &m,
            file_id,
            "core-1908-v2.txt",
            Some("text/plain"),
            ExpectedChunkFormat::V2,
        )
        .unwrap();
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let v2 = serde_json::json!({
            "master_key_hex": hex(&master),
            "file_id": file_id,
            "name": "core-1908-v2.txt",
            "mime_type": "text/plain",
            "name_encrypted": name_encrypted,
            "chunk_size": chunk_size,
            "chunk_count": summary.chunk_count,
            "wire_hex": hex(&wire),
            "plaintext_hex": hex(&plaintext),
        });
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/core_1908_vectors.json");
        let mut doc: serde_json::Value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        doc["v2_file"] = v2;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
        println!("wrote {}", path.display());
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// `tests/fixtures/core_1908_vectors.json` (see its `_provenance`).
    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/core_1908_vectors.json")).unwrap()
    }

    /// The v2 file of the fixture: (master key, file id, name_encrypted, wire,
    /// chunk count, chunk size, plaintext).
    fn v2_file() -> (MasterKey, String, String, Vec<u8>, u32, u64, Vec<u8>) {
        let v = &vectors()["v2_file"];
        let mut mk = [0u8; 32];
        mk.copy_from_slice(&unhex(v["master_key_hex"].as_str().unwrap()));
        (
            MasterKey::from_bytes(mk),
            v["file_id"].as_str().unwrap().to_string(),
            v["name_encrypted"].as_str().unwrap().to_string(),
            unhex(v["wire_hex"].as_str().unwrap()),
            v["chunk_count"].as_u64().unwrap() as u32,
            v["chunk_size"].as_u64().unwrap(),
            unhex(v["plaintext_hex"].as_str().unwrap()),
        )
    }

    /// Cross-client vector (task 1943): a file written by core's v2 writer opens
    /// through the CLI read path: its pin reads as V2, its name decrypts (name
    /// subkey), and its content decrypts with and without the chunk size.
    #[test]
    fn core_1908_v2_fixture_opens_through_the_pin_aware_reader() {
        let (m, fid, name_enc, wire, count, cs, plaintext) = v2_file();
        let pin = content_pin(&m, &fid, &name_enc).expect("the metadata authenticates");
        assert_eq!(pin, ExpectedChunkFormat::V2);
        assert_eq!(decrypt_name(&m, &fid, &name_enc).as_deref(), Some("core-1908-v2.txt"));
        assert_eq!(
            decrypt_file_chunks_with_chunk_size(&m, &fid, pin, &wire, count, Some(cs)).unwrap(),
            plaintext
        );
        // No X-Chunk-Size: the frame size is probed with core's v2 opener.
        assert_eq!(decrypt_file_chunks(&m, &fid, pin, &wire, count).unwrap(), plaintext);
    }

    /// Cross-client vector: core's v1 chunk vector (verbatim from core's
    /// `test-vectors/vectors.json`) still opens, as a legacy (`Unrecorded`)
    /// file. v1 binds no file id, so any id reads it.
    #[test]
    fn core_v1_chunk_vector_still_opens() {
        let v = &vectors()["v1_chunk_vector"];
        let mut key = [0u8; 32];
        key.copy_from_slice(&unhex(v["file_key_hex"].as_str().unwrap()));
        let mut frame = unhex(v["nonce_hex"].as_str().unwrap());
        frame.extend(unhex(v["ciphertext_hex"].as_str().unwrap()));
        let got = try_decrypt_all_chunks(
            &FileKey::from_bytes(key),
            FID,
            ExpectedChunkFormat::Unrecorded,
            &frame,
            1,
        );
        assert_eq!(got.unwrap(), unhex(v["plaintext_hex"].as_str().unwrap()));
    }

    /// Downgrade guard: under a V2 pin, v1 frames of the very same file key and
    /// plaintext do not open (no detection, no v1 fallback, no JSON parser),
    /// while the same bytes open as a legacy file.
    #[test]
    fn a_v2_pinned_file_refuses_v1_frames() {
        let (m, fid, _, _, _, _, plaintext) = v2_file();
        let fk = derive_file_key(&m, fid.as_bytes());
        let raw_v1 = encrypt_chunk_raw(&fk, &plaintext).unwrap();
        let json_v1 = serde_json::to_vec(&encrypt_chunk(&fk, &plaintext).unwrap()).unwrap();
        for (what, wire) in [("raw", &raw_v1), ("json", &json_v1)] {
            assert!(
                decrypt_file_chunks(&m, &fid, ExpectedChunkFormat::V2, wire, 1).is_err(),
                "{what} v1 frame opened under a v2 pin"
            );
            assert_eq!(
                decrypt_file_chunks(&m, &fid, ExpectedChunkFormat::Unrecorded, wire, 1).unwrap(),
                plaintext,
                "{what} control"
            );
        }
    }

    /// v2 truncation: the fixture without its last frame is rejected, though
    /// every frame that is there authenticates (core's `finish`).
    #[test]
    fn a_truncated_v2_file_is_rejected() {
        let (m, fid, _, wire, count, cs, _) = v2_file();
        // The first count-1 frames, all full: everything but the last frame.
        let cut = (count as usize - 1) * (cs as usize + 28);
        let err =
            decrypt_file_chunks_with_chunk_size(&m, &fid, ExpectedChunkFormat::V2, &wire[..cut], count - 1, Some(cs))
                .unwrap_err();
        assert!(err.contains("v2"), "{err}");
    }

    /// The pin comes only from authenticated metadata: v2 envelope → V2, a
    /// legacy envelope → Unrecorded, a plaintext name → refused (never guessed).
    #[test]
    fn content_pin_reads_authenticated_metadata_only() {
        let (m, fid, name_enc, ..) = v2_file();
        assert_eq!(content_pin(&m, &fid, &name_enc), Ok(ExpectedChunkFormat::V2));
        let legacy = beebeeb_core::encrypt::encrypt_name(&m, &fid, "old.txt", None).unwrap();
        assert_eq!(content_pin(&m, &fid, &legacy), Ok(ExpectedChunkFormat::Unrecorded));
        let err = content_pin(&m, &fid, "plain-name.txt").unwrap_err();
        assert!(err.contains("refusing to guess"), "{err}");
        // A key that does not open the envelope is no pin either.
        let other = MasterKey::from_bytes([0x24u8; 32]);
        assert!(content_pin(&other, &fid, &name_enc).is_err());
    }
    fn repair_vectors() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/sf_0002_owner_repair.json")).unwrap()
    }

    fn repair_master(v: &serde_json::Value) -> MasterKey {
        let mut mk = [0u8; 32];
        mk.copy_from_slice(&unhex(v["master_key_hex"].as_str().unwrap()));
        MasterKey::from_bytes(mk)
    }

    /// SF-0002: a row the old iOS File Provider sealed under the uppercase id
    /// opens through the ordinary readers (name, pin, content) and reports the
    /// read-only convention.
    #[test]
    fn owner_repair_v1_opens_and_reports_read_only() {
        let v = &repair_vectors()["upper_repair_v1"];
        let m = repair_master(v);
        let fid = v["file_id"].as_str().unwrap();
        let name_enc = v["name_encrypted"].as_str().unwrap();
        let frame = unhex(v["content_frame_hex"].as_str().unwrap());
        let plaintext = unhex(v["content_plaintext_hex"].as_str().unwrap());

        assert_eq!(
            decrypt_name_with_convention(&m, fid, name_enc),
            Some((
                v["expected_name"].as_str().unwrap().to_string(),
                OwnerKeyConvention::UpperRepair
            ))
        );
        assert_eq!(
            decrypt_name(&m, fid, name_enc).as_deref(),
            Some(v["expected_name"].as_str().unwrap())
        );
        let pin = content_pin(&m, fid, name_enc).expect("the repair metadata authenticates");
        assert_eq!(pin, ExpectedChunkFormat::Unrecorded);
        assert_eq!(decrypt_file_chunks(&m, fid, pin, &frame, 1).unwrap(), plaintext);
        assert_eq!(
            decrypt_file_chunks_owned(&m, fid, pin, None, &frame, 1, None).unwrap(),
            (plaintext, Some(OwnerKeyConvention::UpperRepair))
        );
    }

    /// SF-0002 split row: the name is sealed uppercase while each version uses
    /// its own convention. Both versions must open, per version.
    #[test]
    fn owner_repair_split_opens_every_version() {
        let v = &repair_vectors()["upper_repair_split_v1"];
        let m = repair_master(v);
        let fid = v["file_id"].as_str().unwrap();
        let name_enc = v["name_encrypted"].as_str().unwrap();
        assert_eq!(
            decrypt_name_with_convention(&m, fid, name_enc),
            Some((
                v["expected_name"].as_str().unwrap().to_string(),
                OwnerKeyConvention::UpperRepair
            ))
        );
        for (frame_key, pt_key) in [
            ("content_v1_frame_hex", "content_v1_plaintext_hex"),
            ("content_v2_frame_hex", "content_v2_plaintext_hex"),
        ] {
            let frame = unhex(v[frame_key].as_str().unwrap());
            let plaintext = unhex(v[pt_key].as_str().unwrap());
            let pin = content_pin(&m, fid, name_enc).expect("the repair metadata authenticates");
            let (out, _) = decrypt_file_chunks_owned(&m, fid, pin, None, &frame, 1, None)
                .unwrap_or_else(|e| panic!("{frame_key}: {e}"));
            assert_eq!(out, plaintext, "{frame_key}");
        }
    }

    /// The fallback is a fallback: an ordinary owner row still resolves under
    /// the string convention and is not marked read-only.
    #[test]
    fn an_ordinary_owner_row_is_not_read_only() {
        let m = mk();
        let name_enc = beebeeb_core::encrypt::encrypt_name(&m, FID, "ordinary.txt", None).unwrap();
        assert_eq!(
            decrypt_name_with_convention(&m, FID, &name_enc),
            Some(("ordinary.txt".to_string(), OwnerKeyConvention::String))
        );
    }

    /// SF-0002 (mount): the flag a mounted entry carries comes from the name's
    /// convention alone — true only for a name sealed under the uppercase id.
    #[test]
    fn the_mount_read_only_flag_follows_the_name_convention() {
        use beebeeb_core::chunk_stream::ExpectedChunkFormat;
        let m = mk();
        let id = "5f0c2a9e-3b1d-4c7e-9a2f-6d8e1b4c7a30";
        let upper = beebeeb_core::kdf::derive_file_key(&m, id.to_uppercase().as_bytes());
        let normal = beebeeb_core::kdf::derive_file_key(&m, id.as_bytes());
        let sealed =
            beebeeb_core::encrypt::encrypt_name_for(&upper, "x.pdf", None, ExpectedChunkFormat::Unrecorded).unwrap();
        let ordinary =
            beebeeb_core::encrypt::encrypt_name_for(&normal, "x.pdf", None, ExpectedChunkFormat::Unrecorded).unwrap();
        assert!(name_is_upper_repair(&m, id, &sealed));
        assert!(!name_is_upper_repair(&m, id, &ordinary));
    }
}

mod names;
mod pins;
mod frames;
mod chunks;

pub use names::OWNER_REPAIR_READ_ONLY;
pub use names::decrypt_name;
pub use names::decrypt_name_with_convention;
pub use names::name_is_upper_repair;
#[cfg(test)]
pub use names::decrypt_name_with_key;
pub use pins::owner_content_key;
pub use pins::first_frame_opens;
pub use pins::first_json_blob_frame;
pub use pins::content_pin;
pub use pins::content_pin_with_key;
pub use pins::file_row_pin;
pub(crate) use pins::key_copy;
pub(crate) use frames::probe_first_frame_len;
pub use chunks::decrypt_file_chunks;
pub use chunks::decrypt_file_chunks_with_chunk_size;
pub use chunks::try_decrypt_all_chunks;
pub use chunks::try_decrypt_all_chunks_with_chunk_size;
#[cfg(test)]
use chunks::decrypt_file_chunks_owned;
use frames::raw_frame_size;
use pins::NONCE_LEN;
use pins::TAG_LEN;
#[cfg(test)]
use names::decrypt_names;
