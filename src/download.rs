//! Streaming, constant-memory download + decrypt — the read-side counterpart to
//! `src/upload.rs`.
//!
//! `GET /api/v1/files/{id}/download` returns the chunks concatenated back to
//! back (`nonce(12) || ciphertext || tag(16)` per chunk) with `X-Chunk-Count`,
//! `X-Original-Size`, and (for V2 files) `X-Chunk-Size` headers. The old path
//! buffered the entire payload (`resp.bytes().await`) before decrypting — peak
//! RAM ≈ file size. This module reads the body incrementally with
//! `Response::chunk()`, reassembles one logical frame at a time, decrypts it
//! through the shared [`ChunkDecryptor`], writes the plaintext, and drops it.
//!
//! ## Legacy compatibility (user-data regression risk — preserved carefully)
//!
//! Only the **current raw format with the string-UUID key** streams. The
//! format is never guessed from the first byte (a random raw nonce starts with
//! `{` 1 time in 256, task 1759): raw streaming is always tried first, and the
//! frame-0 AEAD tag is the arbiter. If the first frame fails to authenticate,
//! the whole payload is buffered and handed to [`decrypt_file_chunks_with_chunk_size`] in
//! `crypto::chunks`,
//! which covers both legacy shapes:
//!
//! - **JSON-blob chunks** (old CLI uploads) — buffered + decrypted as before.
//! - **binary-UUID key derivation** (pre-`bb repair` legacy files) — the
//!   [`ChunkDecryptor`] only derives the string-UUID key, so the buffered path
//!   retries with the binary-UUID key. `bb repair` semantics intact.
//!
//! Framing: the server's `X-Chunk-Size` header (the uniform plaintext chunk
//! size for the file's version) is authoritative — each wire frame is
//! `chunk_size + CHUNK_OVERHEAD` for the first N-1 chunks, remainder for the
//! last. Streaming uploads make every chunk but the last exactly that size, so
//! the old `total / chunk_count` average mis-sized frame 0 whenever the file
//! size was not an exact multiple of the chunk size (e.g. a 150 MiB file with
//! 8 MiB chunks). When the header is absent (legacy V1 responses) the streaming
//! guess `total / chunk_count` is only a first attempt; if it fails, the
//! buffered path recovers the real frame size by AEAD-validated probing.

use std::io::Write;
use std::path::Path;

use beebeeb_core::chunk_stream::{ChunkDecryptor, ExpectedChunkFormat, MAX_CHUNK_SIZE};
use beebeeb_core::kdf::MasterKey;

use crate::api::ApiClient;
use crate::upload::ChunkProgress;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::header, response::IntoResponse, routing::get};
    use beebeeb_core::encrypt::{encrypt_chunk, encrypt_chunk_raw};
    use beebeeb_core::kdf::{derive_file_key, derive_master_key};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const FID: &str = "3e15382b-1111-2222-3333-444455556666";

    /// Counts server-confirmed frames. Streaming reports one per frame; the
    /// buffered fallback reports the whole payload once.
    struct Counting(Arc<AtomicUsize>);
    struct CountingFile(Arc<AtomicUsize>);
    impl ChunkProgress for Counting {
        fn begin_file(&self, _name: &str, _expected: u64) -> Box<dyn crate::upload::FileProgress> {
            Box::new(CountingFile(self.0.clone()))
        }
    }
    impl crate::upload::FileProgress for CountingFile {
        fn chunk_confirmed(&self, _bytes: u64) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        fn finish(self: Box<Self>, _success: bool) {}
    }

    #[derive(Clone)]
    struct Served {
        body: Arc<Vec<u8>>,
        chunk_size: Option<u64>,
        chunk_count: u32,
    }

    async fn serve(State(s): State<Served>) -> impl IntoResponse {
        let mut h = axum::http::HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "application/octet-stream".parse().unwrap());
        h.insert("x-chunk-count", s.chunk_count.to_string().parse().unwrap());
        if let Some(cs) = s.chunk_size {
            h.insert("x-chunk-size", cs.to_string().parse().unwrap());
        }
        (h, s.body.as_ref().clone())
    }

    /// Raw multi-chunk wire bytes; `brace_first` re-rolls frame 0's nonce until
    /// it starts with 0x7b.
    fn raw_wire(m: &MasterKey, pt: &[u8], cs: usize, brace_first: bool) -> Vec<u8> {
        let fk = derive_file_key(m, FID.as_bytes());
        let mut out = Vec::new();
        for (i, chunk) in pt.chunks(cs).enumerate() {
            let frame = if i == 0 && brace_first {
                (0..100_000)
                    .map(|_| encrypt_chunk_raw(&fk, chunk).unwrap())
                    .find(|f| f[0] == b'{')
                    .expect("a 0x7b nonce within 100k tries")
            } else {
                encrypt_chunk_raw(&fk, chunk).unwrap()
            };
            out.extend_from_slice(&frame);
        }
        out
    }

    /// Download `body` through the real streaming path against a local mock;
    /// returns (decrypted file, number of progress confirmations).
    async fn download(body: Vec<u8>, chunk_size: Option<u64>, chunk_count: u32, m: &MasterKey) -> (Vec<u8>, usize) {
        download_with(body, chunk_size, chunk_count, m, FALLBACK_HARD_CEILING).await
    }

    async fn download_with(
        body: Vec<u8>,
        chunk_size: Option<u64>,
        chunk_count: u32,
        m: &MasterKey,
        ceiling: u64,
    ) -> (Vec<u8>, usize) {
        let (got, confirmed) = download_inner(
            body,
            chunk_size,
            chunk_count,
            m,
            FID,
            ExpectedChunkFormat::Unrecorded,
            ceiling,
        )
        .await;
        (got.expect("download + decrypt succeeds"), confirmed)
    }

    /// [`download_inner_typed`] with the error as text, for the many tests that only read the message.
    async fn download_inner(
        body: Vec<u8>,
        chunk_size: Option<u64>,
        chunk_count: u32,
        m: &MasterKey,
        file_id: &str,
        pin: ExpectedChunkFormat,
        ceiling: u64,
    ) -> (Result<Vec<u8>, String>, usize) {
        let (got, confirmed) = download_inner_typed(body, chunk_size, chunk_count, m, file_id, pin, ceiling).await;
        (got.map_err(String::from), confirmed)
    }

    /// The real streaming path for any file id and read pin; returns the
    /// decrypted file (or the error) and the number of progress confirmations.
    async fn download_inner_typed(
        body: Vec<u8>,
        chunk_size: Option<u64>,
        chunk_count: u32,
        m: &MasterKey,
        file_id: &str,
        pin: ExpectedChunkFormat,
        ceiling: u64,
    ) -> (Result<Vec<u8>, DownloadError>, usize) {
        let app = Router::new()
            .route("/api/v1/files/:id/download", get(serve))
            .with_state(Served {
                body: Arc::new(body),
                chunk_size,
                chunk_count,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        let dir = std::env::temp_dir().join(format!("bb-dl-1759-{}-{}", std::process::id(), rand::random::<u32>()));
        let out = dir.join("out.bin");
        let confirmed = Arc::new(AtomicUsize::new(0));
        let r = stream_download_decrypt_with(
            &api,
            m,
            file_id,
            pin,
            chunk_count,
            &out,
            &Counting(confirmed.clone()),
            "out.bin",
            ceiling,
        )
        .await;
        let got = r.and_then(|_| std::fs::read(&out).map_err(|e| DownloadError::Other(e.to_string())));
        let _ = std::fs::remove_dir_all(&dir);
        (got, confirmed.load(Ordering::SeqCst))
    }

    // ── Task 1943: the pinned streaming path ─────────────────────────────────

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The fixture's core-written v2 file: (master key, file id, wire, chunk
    /// count, chunk size, plaintext).
    fn v2_file() -> (MasterKey, String, Vec<u8>, u32, u64, Vec<u8>) {
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/core_1908_vectors.json")).unwrap();
        let v = &doc["v2_file"];
        let mut mk = [0u8; 32];
        mk.copy_from_slice(&unhex(v["master_key_hex"].as_str().unwrap()));
        (
            MasterKey::from_bytes(mk),
            v["file_id"].as_str().unwrap().to_string(),
            unhex(v["wire_hex"].as_str().unwrap()),
            v["chunk_count"].as_u64().unwrap() as u32,
            v["chunk_size"].as_u64().unwrap(),
            unhex(v["plaintext_hex"].as_str().unwrap()),
        )
    }

    /// The core-written v2 fixture streams through `bb pull`'s real path under
    /// its V2 pin, one confirmation per frame; cut before its last frame it is
    /// rejected and nothing is left at the output path.
    #[tokio::test]
    async fn v2_fixture_streams_under_its_pin_and_truncation_is_rejected() {
        let (m, fid, wire, count, cs, plaintext) = v2_file();
        let (got, confirmed) = download_inner(
            wire.clone(),
            Some(cs),
            count,
            &m,
            &fid,
            ExpectedChunkFormat::V2,
            FALLBACK_HARD_CEILING,
        )
        .await;
        assert_eq!(got.unwrap(), plaintext);
        assert_eq!(confirmed, count as usize, "streamed frame by frame");

        let cut = (count as usize - 1) * (cs as usize + 28);
        let (got, _) = download_inner(
            wire[..cut].to_vec(),
            Some(cs),
            count - 1,
            &m,
            &fid,
            ExpectedChunkFormat::V2,
            FALLBACK_HARD_CEILING,
        )
        .await;
        assert!(
            got.is_err(),
            "a v2 file missing its last frame must not download: {got:?}"
        );
    }

    /// Under a V2 pin the streaming path (and its buffered fallback) refuses v1
    /// frames of the same file key; the same bytes open as a legacy file.
    #[tokio::test]
    async fn v2_pin_refuses_v1_frames_on_the_streaming_path() {
        let (m, fid, _, _, _, plaintext) = v2_file();
        let v1 = encrypt_chunk_raw(&derive_file_key(&m, fid.as_bytes()), &plaintext).unwrap();
        let (got, _) = download_inner(
            v1.clone(),
            None,
            1,
            &m,
            &fid,
            ExpectedChunkFormat::V2,
            FALLBACK_HARD_CEILING,
        )
        .await;
        assert!(got.is_err(), "v1 frame opened under a v2 pin");
        let (got, _) = download_inner(
            v1,
            None,
            1,
            &m,
            &fid,
            ExpectedChunkFormat::Unrecorded,
            FALLBACK_HARD_CEILING,
        )
        .await;
        assert_eq!(got.unwrap(), plaintext, "control");
    }

    fn mk() -> MasterKey {
        derive_master_key("test-password", b"test-salt-16bytes").unwrap()
    }

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Task 1759 round 2: raw multi-chunk file, uneven last chunk, frame 0's
    /// nonce starts with '{', X-Chunk-Size present. Must decrypt AND stream
    /// (one confirmation per frame), not be shunted to the buffered path by a
    /// first-byte sniff.
    #[tokio::test]
    async fn streams_raw_uneven_last_chunk_with_brace_first_byte() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        let wire = raw_wire(&m, &pt, 4096, true);
        assert_eq!(wire[0], b'{');
        let (got, confirmed) = download(wire, Some(4096), 4, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 4, "4 frames streamed, not one buffered blob");
    }

    /// Same file with a non-brace nonce streams too (control).
    #[tokio::test]
    async fn streams_raw_uneven_last_chunk() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        let wire = raw_wire(&m, &pt, 4096, false);
        let (got, confirmed) = download(wire, Some(4096), 4, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 4);
    }

    /// Legacy V1 response (no X-Chunk-Size): the total/count guess mis-sizes
    /// frame 0, so the buffered path recovers the real frame size by probing.
    #[tokio::test]
    async fn legacy_response_without_chunk_size_header_uneven_last_chunk() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        for brace in [false, true] {
            let wire = raw_wire(&m, &pt, 4096, brace);
            let (got, confirmed) = download(wire, None, 4, &m).await;
            assert_eq!(got, pt, "brace_first={brace}");
            assert_eq!(confirmed, 1, "buffered fallback reports the payload once");
        }
    }

    /// Legacy CLI JSON-blob multi-chunk file still downloads (buffered).
    #[tokio::test]
    async fn legacy_json_blob_file_still_downloads() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(2 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        assert_eq!(wire[0], b'{');
        let (got, confirmed) = download(wire, None, 3, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 1);
    }

    /// Single raw chunk whose nonce starts with '{' streams (was sniffed away).
    #[tokio::test]
    async fn single_raw_chunk_with_brace_first_byte() {
        let m = mk();
        let pt = payload(500);
        let wire = raw_wire(&m, &pt, 4096, true);
        let (got, confirmed) = download(wire, Some(4096), 1, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 1);
    }

    /// Serve `body` with arbitrary extra headers and WITHOUT a Content-Length
    /// (chunked transfer), then run the real download. Returns the result.
    async fn try_download_chunked(
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
        chunk_count: u32,
        m: &MasterKey,
    ) -> Result<Vec<u8>, String> {
        try_download_chunked_with(body, headers, chunk_count, m, FALLBACK_HARD_CEILING).await
    }

    async fn try_download_chunked_with(
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
        chunk_count: u32,
        m: &MasterKey,
        ceiling: u64,
    ) -> Result<Vec<u8>, String> {
        run_chunked_with(body, headers, chunk_count, m, ceiling)
            .await
            .map(|(bytes, _)| bytes)
    }

    /// [`try_download_chunked_with`] that also returns the download's stats.
    async fn run_chunked_with(
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
        chunk_count: u32,
        m: &MasterKey,
        ceiling: u64,
    ) -> Result<(Vec<u8>, DownloadStats), String> {
        let body = Arc::new(body);
        let headers = Arc::new(headers);
        let app = Router::new().route(
            "/api/v1/files/:id/download",
            get(move || {
                let body = body.clone();
                let headers = headers.clone();
                async move {
                    let mut h = axum::http::HeaderMap::new();
                    for (k, v) in headers.iter() {
                        h.insert(*k, v.parse().unwrap());
                    }
                    let parts: Vec<Result<bytes::Bytes, std::convert::Infallible>> = body
                        .chunks(8192)
                        .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
                        .collect();
                    (h, axum::body::Body::from_stream(futures_util::stream::iter(parts)))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        let dir = std::env::temp_dir().join(format!("bb-dl-1759-{}-{}", std::process::id(), rand::random::<u32>()));
        let out = dir.join("out.bin");
        let confirmed = Arc::new(AtomicUsize::new(0));
        let r = stream_download_decrypt_with(
            &api,
            m,
            FID,
            ExpectedChunkFormat::Unrecorded,
            chunk_count,
            &out,
            &Counting(confirmed),
            "out.bin",
            ceiling,
        )
        .await;
        let got = r
            .map(|stats| (std::fs::read(&out).unwrap(), stats))
            .map_err(String::from);
        let _ = std::fs::remove_dir_all(&dir);
        got
    }

    #[test]
    fn a_download_error_reads_as_its_message_and_text_errors_are_plain_failures() {
        for e in [
            DownloadError::Unreadable("a".into()),
            DownloadError::Changed("a".into()),
            DownloadError::Other("a".into()),
        ] {
            assert_eq!(e.to_string(), "a");
            assert_eq!(String::from(e), "a");
        }
        assert_eq!(
            DownloadError::from("boom".to_string()),
            DownloadError::Other("boom".into())
        );
    }

    /// The API path types what sync must not abort a pass for: a frame after the first that does not
    /// authenticate, and a v2 stream cut before its last frame, are `Unreadable`; a plain network or
    /// size problem is not.
    #[tokio::test]
    async fn the_api_path_types_an_unreadable_file() {
        let (m, fid, wire, count, cs, _) = v2_file();
        let frame = (cs + 28) as usize;
        // Frame 1 flipped: frame 0 authenticates, so this is not a legacy file; it is damaged.
        let mut tampered = wire.clone();
        tampered[frame + 20] ^= 1;
        let (got, _) = download_inner_typed(
            tampered,
            Some(cs),
            count,
            &m,
            &fid,
            ExpectedChunkFormat::V2,
            FALLBACK_HARD_CEILING,
        )
        .await;
        assert!(
            matches!(got, Err(DownloadError::Unreadable(ref m)) if m.contains("decrypt chunk 1")),
            "{got:?}"
        );
        // The last frame missing: every frame authenticates, the stream never reaches the one marked last.
        let (got, _) = download_inner_typed(
            wire[..(count as usize - 1) * frame].to_vec(),
            Some(cs),
            count - 1,
            &m,
            &fid,
            ExpectedChunkFormat::V2,
            FALLBACK_HARD_CEILING,
        )
        .await;
        assert!(
            matches!(got, Err(DownloadError::Unreadable(ref m)) if m.contains("download incomplete")),
            "{got:?}"
        );
    }

    /// Both API shapes (the raw stream and the buffered legacy decrypt) report `api`:
    /// `"direct"` and `"mixed"` are only ever set by the direct driver.
    #[tokio::test]
    async fn the_api_paths_report_the_api_transport() {
        let m = mk();
        let pt = payload(2 * 4096 + 5);
        let raw = raw_wire(&m, &pt, 4096, false);
        let (got, stats) = run_chunked_with(
            raw,
            vec![("x-chunk-size", "4096".to_string())],
            3,
            &m,
            FALLBACK_HARD_CEILING,
        )
        .await
        .unwrap();
        assert_eq!((got, stats.transport), (pt.clone(), "api"), "the streaming path");

        let fk = derive_file_key(&m, FID.as_bytes());
        let mut legacy = Vec::new();
        for chunk in pt.chunks(300) {
            legacy.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let count = pt.chunks(300).count() as u32;
        let (got, stats) = run_chunked_with(legacy, vec![], count, &m, FALLBACK_HARD_CEILING)
            .await
            .unwrap();
        assert_eq!((got, stats.transport), (pt, "api"), "the buffered legacy path");
    }

    /// Round 3 P2-1: a hostile X-Chunk-Size (2^40, u64::MAX) must be treated as
    /// absent — no giant frame buffering, no overflow — and the file still
    /// decrypts through the probing fallback.
    #[tokio::test]
    async fn hostile_chunk_size_header_is_ignored() {
        let m = mk();
        let pt = payload(3 * 4096 + 77);
        for hostile in [1u64 << 40, u64::MAX, u64::MAX - 27, MAX_CHUNK_SIZE + 1] {
            let wire = raw_wire(&m, &pt, 4096, false);
            let (got, _) = download(wire, Some(hostile), 4, &m).await;
            assert_eq!(got, pt, "hostile X-Chunk-Size {hostile}");
        }
    }

    /// Review-5 F2 (P2): a legacy stream with no `X-Chunk-Size` sized each
    /// frame as `total / count` with no bound, so a hostile `X-Original-Size`
    /// could ask the reader to buffer gigabytes. Core's downloader refuses
    /// such a frame; this path must too, before writing anything.
    #[tokio::test]
    async fn a_no_header_frame_above_the_max_frame_size_is_refused_before_allocating() {
        let m = mk();
        let total = ((beebeeb_core::download::MAX_FRAME_SIZE as u64) + 1) * 2;
        let err = try_download_chunked(vec![0x5au8; 4096], vec![("x-original-size", total.to_string())], 2, &m)
            .await
            .expect_err("a frame above MAX_FRAME_SIZE must be refused");
        assert!(
            err.contains("exceeds the maximum"),
            "clear frame-size error, got: {err}"
        );
    }

    /// Round 3 P2-3: V2 header present but frame 0 is short (a JSON-blob file
    /// served with the header) takes the buffered fallback, not "truncated".
    #[tokio::test]
    async fn short_frame_zero_with_chunk_size_header_falls_back() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(2 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let (got, confirmed) = download(wire, Some(1 << 20), 3, &m).await;
        assert_eq!(got, pt);
        assert_eq!(confirmed, 1, "buffered fallback");
    }

    /// Round 3 P2-2: the frame-0-failure fallback buffer is capped at
    /// original_size + 28*count; a body that keeps coming is aborted.
    #[tokio::test]
    async fn fallback_buffer_is_capped() {
        let m = mk();
        // Garbage that fails frame-0 auth, far larger than the declared size.
        let body = vec![0x5au8; 2 * 1024 * 1024];
        let err = try_download_chunked(
            body,
            vec![("x-original-size", "1000".into()), ("x-chunk-size", "4096".into())],
            4,
            &m,
        )
        .await
        .expect_err("oversized fallback body must be refused");
        assert!(err.contains("chunked-download limit"), "clear cap error, got: {err}");
    }

    /// The cap function. Content-Length wins outright (no ceiling); chunked is
    /// the JSON bound clamped to the ceiling.
    #[test]
    fn fallback_cap_bounds() {
        let c = FALLBACK_HARD_CEILING;
        assert_eq!(
            fallback_cap(c, Some(500), Some(100), 3),
            FallbackCap {
                cap: 500,
                chunked: false
            }
        );
        // Round 5: a declared length above the ceiling is NOT clamped.
        assert_eq!(
            fallback_cap(c, Some(c + 1), Some(100), 3),
            FallbackCap {
                cap: c + 1,
                chunked: false
            }
        );
        let ch = fallback_cap(c, None, Some(100), 3);
        assert_eq!(
            ch,
            FallbackCap {
                cap: json_wire_bound(100, 3),
                chunked: true
            }
        );
        assert!(ch.cap >= 100 + 28 * 3);
        // Chunked is clamped to the ceiling.
        assert_eq!(fallback_cap(c, None, Some(u64::MAX), 3).cap, c);
        assert_eq!(fallback_cap(1000, None, Some(10_000), 3).cap, 1000);
        assert_eq!(fallback_cap(c, None, None, 3).cap, c);
    }

    /// A body past its own declared length is refused with the Content-Length
    /// message; chunked overrun gets the legacy-format message.
    #[test]
    fn check_cap_messages() {
        let cl = FallbackCap {
            cap: 10,
            chunked: false,
        };
        assert!(check_cap(cl, 10).is_ok());
        let e = check_cap(cl, 11).unwrap_err();
        assert!(e.to_string().contains("declared Content-Length of 10"), "{e}");
        let ch = FallbackCap { cap: 10, chunked: true };
        let e = check_cap(ch, 11).unwrap_err();
        assert!(
            e.to_string()
                .contains("legacy-format file larger than the chunked-download limit"),
            "{e}"
        );
        assert!(
            e.to_string().contains("retry") && e.to_string().contains("Content-Length"),
            "{e}"
        );
    }

    /// Round 5 P1: legacy JSON-blob body WITH Content-Length above the (test
    /// lowered) ceiling downloads: memory is bounded by what the server sends.
    #[tokio::test]
    async fn legacy_json_with_content_length_above_ceiling_downloads() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(3 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let ceiling = 1000u64;
        assert!(wire.len() as u64 > ceiling, "test body must exceed the lowered ceiling");
        let (got, _) = download_with(wire, None, 4, &m, ceiling).await;
        assert_eq!(got, pt);
    }

    /// Round 5 P1: the same body chunked (no Content-Length) over the ceiling
    /// is refused with the legacy-format message.
    #[tokio::test]
    async fn legacy_json_chunked_over_ceiling_is_refused_with_new_message() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(3 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        let ceiling = 1000u64;
        assert!(wire.len() as u64 > ceiling);
        let err = try_download_chunked_with(wire, vec![("x-original-size", pt.len().to_string())], 4, &m, ceiling)
            .await
            .expect_err("chunked over ceiling must be refused");
        assert!(
            err.contains("legacy-format file larger than the chunked-download limit"),
            "got: {err}"
        );
    }

    /// Round 4 P1: chunked transfer (no Content-Length) + X-Original-Size +
    /// legacy JSON-blob chunks. The wire is several times the plaintext, so the
    /// raw-frame cap refused a valid download.
    #[tokio::test]
    async fn chunked_legacy_json_blob_with_original_size_downloads() {
        let m = mk();
        let fk = derive_file_key(&m, FID.as_bytes());
        let pt = payload(3 * 300 + 5);
        let mut wire = Vec::new();
        for chunk in pt.chunks(300) {
            wire.extend(serde_json::to_vec(&encrypt_chunk(&fk, chunk).unwrap()).unwrap());
        }
        assert!(
            wire.len() as u64 > pt.len() as u64 + 28 * 4,
            "JSON must exceed the raw cap"
        );
        let got = try_download_chunked(wire, vec![("x-original-size", pt.len().to_string())], 4, &m)
            .await
            .expect("legacy JSON download must not be refused by the cap");
        assert_eq!(got, pt);
    }

    /// A body far past the JSON worst case is still refused.
    #[tokio::test]
    async fn chunked_body_far_beyond_json_bound_is_refused() {
        let m = mk();
        let pt_len = 1000u64;
        let bound = json_wire_bound(pt_len, 4);
        let body = vec![0x5au8; (bound as usize) * 10 + 4096];
        let err = try_download_chunked(body, vec![("x-original-size", pt_len.to_string())], 4, &m)
            .await
            .expect_err("must be refused");
        assert!(err.contains("chunked-download limit"), "got: {err}");
    }

    /// The JSON bound constants dominate real serialisation, worst case
    /// included (all-0xFF bytes serialise to 3 digits + comma). Prints the
    /// measured factors.
    #[test]
    fn json_bound_covers_serialised_blobs() {
        use beebeeb_types::{CipherSuite, EncryptedBlob};
        let mut max_ratio = 0f64;
        let mut envelope = 0usize;
        for n in [0usize, 1, 16, 300, 4096, 65536] {
            // Real random blob.
            let m = mk();
            let fk = derive_file_key(&m, FID.as_bytes());
            let real = serde_json::to_vec(&encrypt_chunk(&fk, &vec![7u8; n]).unwrap()).unwrap();
            // Synthetic worst case: every byte is 0xFF.
            let worst = serde_json::to_vec(&EncryptedBlob {
                cipher_suite: CipherSuite::V1Aes256Gcm,
                nonce: vec![0xff; 12],
                ciphertext: vec![0xff; n + 16],
            })
            .unwrap();
            assert!(real.len() <= worst.len());
            let digits_len = 4 * (12 + n + 16);
            envelope = envelope.max(worst.len().saturating_sub(digits_len));
            max_ratio = max_ratio.max(worst.len() as f64 / (n as f64 + 1.0));
            assert!(
                worst.len() as u64 <= json_wire_bound(n as u64, 1),
                "n={n}: {} > {}",
                worst.len(),
                json_wire_bound(n as u64, 1)
            );
        }
        eprintln!("measured JSON envelope overhead = {envelope} bytes; max wire/plaintext ratio = {max_ratio:.2}");
        assert!(envelope as u64 <= JSON_CHUNK_ENVELOPE);
    }

    /// SF-0002 round 3 (K1): the probe reads at least a whole legacy JSON blob 0
    /// for the geometry the listing gives (single chunk, uniform chunks with a
    /// shorter last one, a last chunk of one byte).
    #[test]
    fn json_chunk0_probe_len_covers_blob_zero() {
        let fk = derive_file_key(&mk(), FID.as_bytes());
        for (size, count, chunk0) in [
            (0u64, 1u32, 0usize),
            (500, 1, 500),
            (605, 3, 300),
            (3 * 4096 + 77, 4, 4096),
            (10, 2, 9),
        ] {
            let blob0 = serde_json::to_vec(&encrypt_chunk(&fk, &payload(chunk0)).unwrap()).unwrap();
            let want = json_chunk0_probe_len(Some(size), count);
            assert!(
                blob0.len() <= want,
                "size={size} count={count}: blob 0 is {} bytes, the probe reads {want}",
                blob0.len()
            );
        }
    }

    /// SF-0002 round 2 (C6): a download opened with an explicit key (shared
    /// folder, owner wrap, file request) whose frame 0 does not authenticate
    /// fails at once. The body after frame 0 never ends here, so a download that
    /// buffers the rest before failing hangs, and the timeout turns that into a
    /// failure of this test.
    #[tokio::test]
    async fn an_explicit_key_frame0_failure_fails_before_reading_the_rest() {
        use futures_util::StreamExt as _;
        let sealed_under = derive_file_key(&mk(), FID.as_bytes());
        let pt = payload(3 * 4096 + 77);
        let wire: Vec<u8> = pt
            .chunks(4096)
            .flat_map(|c| encrypt_chunk_raw(&sealed_under, c).unwrap())
            .collect();
        // Frame 0 and a little of frame 1, then a body that never ends.
        let head = bytes::Bytes::from(wire[..4096 + 28 + 100].to_vec());
        let app = Router::new().route(
            "/api/v1/files/:id/download",
            get(move || {
                let head = head.clone();
                async move {
                    let mut h = axum::http::HeaderMap::new();
                    h.insert("x-chunk-size", "4096".parse().unwrap());
                    h.insert("x-chunk-count", "4".parse().unwrap());
                    let body = futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(head) })
                        .chain(futures_util::stream::pending());
                    (h, axum::body::Body::from_stream(body))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let api = ApiClient::new_for_test(format!("http://{addr}"));
        let dir = std::env::temp_dir().join(format!("bb-dl-c6-{}-{}", std::process::id(), rand::random::<u32>()));
        let out = dir.join("out.bin");
        let not_this_rows_key = Arc::new(beebeeb_core::kdf::FileKey::from_bytes([9u8; 32]));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            stream_download_decrypt_with_file_key(
                &api,
                not_this_rows_key,
                FID,
                ExpectedChunkFormat::Unrecorded,
                4,
                &out,
                &Counting(Arc::new(AtomicUsize::new(0))),
                "out.bin",
            ),
        )
        .await;
        let exists = out.exists();
        let _ = std::fs::remove_dir_all(&dir);
        let result = result.expect("an explicit-key frame-0 failure must not wait for the rest of the body");
        match result {
            Err(DownloadError::Unreadable(_)) => {}
            Err(other) => panic!("an explicit key that does not open frame 0 is unreadable, got {other:?}"),
            Ok(_) => panic!("a key that does not open frame 0 downloaded the file"),
        }
        assert!(!exists, "nothing is published for a failed download");
    }
}

mod cap;
use cap::*;

mod error;
pub use error::{DownloadError, DownloadStats};

mod stream;
pub use stream::download_buffered;
pub use stream::download_frame_prefix;
pub use stream::stream_download_decrypt;
pub use stream::stream_download_decrypt_with_file_key;
#[cfg(test)]
use stream::{json_chunk0_probe_len, stream_download_decrypt_with};

mod atomic;
pub(crate) use atomic::AtomicFile;
