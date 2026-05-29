# beebeeb-io/cli

`bb` — Beebeeb CLI for end-to-end encrypted cloud storage.

## Build

```sh
cargo build                            # Binary at target/debug/bb
cargo build --release                  # Optimized
cargo build --release --features fuse  # With FUSE support (requires macFUSE / libfuse3)
cargo test
cargo clippy -- -D warnings
cargo fmt -- --check
```

## Commands

Generated from `bb --help`. Source of truth is `src/main.rs` (clap derive).

### Auth & session

- `bb login` — browser-based device authorisation (P-256 ECDH + HKDF-SHA256 + AES-GCM handoff, with raw fallback for v0.4 web apps). Supports `--headless` for SSH boxes. CLI auth sessions stored in Redis (HA-safe across API servers).
- `bb logout` — end the current session.
- `bb whoami` — show email, device, region, quota.
- `bb status` — connection + session + storage status.
- `bb config` — print current configuration with secrets masked.

### Files

- `bb push <path>` (alias `bb upload`) — encrypt and upload a file or folder. Uses V2 upload path (init → chunks → complete) so chunk metadata is stored in `object_versions`.
- `bb pull <id-or-path>` (alias `bb download`) — **streaming** download + decrypt (constant memory, live progress bar). Preserves legacy decrypt: JSON-blob chunks and pre-`bb repair` binary-UUID files fall back to the buffered path.
- `bb ls [path]` — list vault contents.
- `bb quota` — storage usage with a colour-coded bar.

### Sharing

- `bb share <file-id>` — create an encrypted share link (`--expires`, `--max-opens`, `--passphrase`, `--double-encrypted`).
- `bb shares` — list active share links.
- `bb unshare [share-id]` — revoke a share link (interactive picker without args).

### File requests

Account-less links that let **anyone** upload an encrypted file *into* your vault. Crypto via the `core` crate's `file_request` module (per-request X25519 keypair; private half wrapped under the master key, public half only in the link fragment).

- `bb request create [--folder <id|name>] [--max-files N] [--max-bytes 100MB] [--expires 7d] [--title ..] [--desc ..]` — mints a request and prints the link `<APP_URL>/r/<token>#<R_pub>`. The keypair is generated locally; only the wrapped private key reaches the server.
- `bb request list` — table of your requests (title, files, bytes, status, expiry) plus each rebuilt link (derives `R_pub` by unwrapping the stored private key under your master key).
- `bb request rm <id>` — close the request (stops new uploads). `--delete` hard-deletes (note: the server `DELETE` route is not live yet; until it lands, `--delete` returns a clear error and you should close instead).
- `bb request send <url> <file>...` — account-less upload **to** a request. Parses the `token` + `#<R_pub>` from the link, seals a per-file content key to `R_pub` via `core`, and POSTs the encrypted blob to the configured API. Fails loudly if the link has no public-key fragment.

**Key-derivation context:** the HKDF context for both the private-key wrap and the per-upload seal is currently the **empty byte string**, because the server assigns the request id / token / file id only *after* the client crypto must already be done. Every client (web/cli/mobile) must use the same constant — see the module docs in `src/commands/request.rs` and the cross-client decision note.

### Sync & mount

- `bb sync <local> [remote]` — bidirectional folder sync (continuous by default; `--once`, `--daemon`, `--stop`, `--dry-run`, `--force`, `--delete`, `--concurrency`, `--rehash`). V2 **streaming** uploads (constant memory). Remote path auto-strips `~/` home prefix. Gracefully handles 409 stuck uploads and corrupt remote files (trashes + re-uploads next run). Shows a scan spinner, then live per-file + overall progress bars (rich TTY only). `--rehash` forces a full re-hash of every file instead of trusting unchanged `(size, mtime)` entries from the last sync.
- `bb watch <path>` — deprecated alias for `bb sync`.
- `bb mount <mountpoint>` — FUSE mount. Interactive setup wizard guides through macFUSE/libfuse3 installation. V2 uploads.
- `bb unmount <mountpoint>` — unmount a previously mounted vault.
- `bb webdav` — serve the vault as a local WebDAV server (`--port`, `--read-only`, `--cache-ttl`, `--no-cache`, `--verbose`).

### Billing

- `bb billing show` — read-only plan, storage, and renewal info. `--json` outputs the raw API merge.

### Utilities

- `bb speedtest` — benchmark network throughput + crypto speed against the API.
- `bb repair [--dry-run]` — re-encrypt files stuck in the legacy binary-UUID key derivation so they open in the web app.
- `bb completions <shell>` — print a shell completion script (`bash`, `zsh`, `fish`, `powershell`).

## Upload pipeline (`src/upload.rs`)

`bb push` and `bb sync` share **one** streaming upload driver:
`upload::stream_encrypt_upload(api, Arc<MasterKey>, UploadSpec, &dyn ChunkProgress)`.

- Consumes `beebeeb_core::chunk_stream::ChunkEncryptor` (the shared core
  primitive). A producer task owns the encryptor and runs `next_chunk()` inside
  `spawn_blocking` (AES off the reactor), feeding a bounded `tokio::mpsc`
  (cap 1); the consumer PUTs each chunk and advances progress on the 200.
- **Constant memory**: peak ≈ `concurrency × ~4 × chunk_size`, never
  file-size- or tree-size-proportional. (Measured: ~75 MiB RSS for a 5.2 GB
  tree.) Replaces the old read-whole-file + encrypt-all-into-RAM path.
- `upload_init` uses `ChunkEncryptor::expected_total_ciphertext()`
  (`= size + 28·chunk_count`) — no full read. `finish()` guards a file that
  shrank mid-stream; a re-stat before init guards a size change.
- **Cancellation**: a shared `AtomicBool` (set by the Ctrl-C handler) is checked
  at the top of the producer loop and consumer loop; the consumer always
  `rx.close()`s before awaiting the producer (cap-1 deadlock-proof).
- **Progress** uses `indicatif` (now an active dependency): a scan spinner
  around `walk`/`scan_local_files` + a `MultiProgress` (overall bar + transient
  per-file bars), amber accent (xterm 214), bytes advancing only on
  server-confirmed chunks. `--json`/`--quiet`/non-TTY → a no-op `ChunkProgress`
  + the existing plain `println!` lines.
- Thumbnails (image/* only, one bounded ≤64 MiB read) are best-effort and
  non-fatal, encrypted with the per-file key derived in the CLI.
- The `ApiClient` (`api.rs`) now carries a **300 s per-request timeout**; the SSE
  sync stream overrides it per-request with its own 24 h timeout.

## Download pipeline (`src/download.rs`)

`bb pull` and `bb sync`'s `download_to` share `download::stream_download_decrypt`
— the read-side counterpart to `upload.rs`:

- Reads the response body incrementally with `ApiClient::download_stream` +
  `reqwest::Response::chunk()` (no whole-payload buffer), reassembles one logical
  frame at a time, decrypts via `beebeeb_core::chunk_stream::ChunkDecryptor`
  (`push_frame`), and writes to a `.tmp` + atomic rename. Constant memory
  (~one chunk; measured ~40 MiB RSS for a 256 MiB download).
- Frame sizing = `total / chunk_count` for the first N-1 frames, remainder for
  the last (matches `crypto::decrypt_raw_chunks`); `total` from `Content-Length`
  or `X-Original-Size + 28·chunk_count`. The download API sends no
  `X-Chunk-Size`, so `push_frame` (self-describing per frame) is used.
- **Legacy preserved** (user-data safety): JSON-blob chunks (first byte `{`) and
  pre-`bb repair` binary-UUID files fall back to the unchanged buffered
  `crypto::decrypt_file_chunks` (which retries binary-UUID then string-UUID).
  `ChunkDecryptor` only derives the string-UUID key, so a first-frame decrypt
  failure triggers the buffered fallback. `bb repair` semantics intact.
- 600 s download timeout (via `download_stream`), matching `beebeeb-upload`.
- `bb pull --zip` (batch folder zip) stays buffered by design (the zip streamer
  needs random access).

## Dependencies

Uses `beebeeb-core` and `beebeeb-types` from the `core` repo via Cargo git dependency. For local development, add to `.cargo/config.toml`:

```toml
[patch."https://github.com/beebeeb-io/core"]
beebeeb-core = { path = "../core/beebeeb-core" }
beebeeb-types = { path = "../core/beebeeb-types" }
```

CI strips `[patch]` before building (see `release.yml`).

## Config

Stored at:
- macOS: `~/Library/Application Support/beebeeb/config.json`
- Linux: `~/.config/beebeeb/config.json`

Contains `api_url`, `session_token`, `master_key`, `email`. Guard this file like an SSH identity file.

## Design reference

Terminal mockups: `../../design/hifi/hifi-cli.jsx`.

## Colors

Defined in `src/colors.rs` — amber `#f5b800` for branding, sage green `#8fc18b` for success, coral red `#e07a6a` for errors. Always use the `colors::*` constants, not raw `.truecolor()`.

## Graphify

This repo has a knowledge graph at `graphify-out/`:
- Before exploring code, read `graphify-out/GRAPH_REPORT.md` for module structure.
- After modifying code, run `graphify update .` and commit the updated `graphify-out/`.
- Use `graphify query "<question>"` to ask questions about the codebase.
- Use `graphify path "<A>" "<B>"` to find connections between two concepts.

## Keep shared docs in sync

When you add/change/remove endpoints, types, build commands, or dependencies, update the relevant skill file in `.claude/skills/`:
- API endpoints → `beebeeb-api.md`
- Designs → `beebeeb-designs.md`
- Build / deps / ports → `beebeeb-stack.md`, `beebeeb-dev.md`

Other agents depend on these being accurate.
