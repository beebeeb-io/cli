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

Source of truth is `src/main.rs` (clap derive); the generated listing is archived (details: docs/runbook/cli-task-history.md).

### Auth & session

- `bb login` — browser device authorisation; handshake crypto (P-256 ECDH + HKDF-SHA256 + AES-GCM, raw fallback for v0.4 web apps) lives in `beebeeb_core::cli_auth` (`CliEphemeralKey`/`decrypt_browser_payload`). `--headless` for SSH boxes; CLI auth sessions stored in Redis (HA-safe). The printed link carries NO code — the user TYPES the `Authorization code:` line into the approval page (a link someone else sends has nothing to approve); the WS init frame (`login.rs::init_frame`) adds `client`/`client_version`/`os`/`device_name` ("reported by the device").
- `bb signup` — does not create accounts (trial needs a payment mandate, collected only by the web checkout). Prints "Create your account at <web>/signup — then run `bb login`.", best-effort `open::that` (not when `--json`/`--quiet`/headless), exit 0. Web host from `src/web_url.rs` (`APP_URL` env, else derived from the API URL). No registration code in the CLI at all.
- **Onboarding document:** `ApiClient::get_onboarding(with_session)` reads `GET /api/v1/onboarding` (headers `X-Beebeeb-Client: cli`, `X-Beebeeb-Onboarding-Schema: 1`, `X-Beebeeb-Client-OS`, `X-Beebeeb-Store-Channel: direct`; 5 s timeout; 404/405 → `Ok(None)`). `src/account_state.rs::OnboardingDoc` is lenient (schema major 1 only; unknown → defaults). It changes words, never behaviour: `bb signup` still only points at the web; adds an explanatory line + `signup` key in `--json`; `bb whoami` shows the account state label + denied capabilities. Contracts vendored at `contracts/onboarding/` from `repos/server/contracts/onboarding/` (drift guard: `make check-onboarding-contract`; re-vendor with `cp -R`).
- `bb logout` / `bb whoami` (email, device, region, quota, account state off `GET /billing/subscription`'s `account_state`/`data_deletion_at`; `needs_plan`/`lapsed` → plan `none`, upload `blocked`, notice; exit 1 when logged out / session rejected) / `bb status` (same exit behaviour) / `bb config` (masked secrets).
- Generic 401 → "Your session expired or was revoked. Run `bb login` to sign in again." (`api::SESSION_EXPIRED_MESSAGE`; stable `unauthorized` code kept on `ApiError`). Connect failure → "Can't reach <api_url> — check your connection or --api".

### Files

- `bb push <path>` (alias `bb upload`) — v2 upload-session route (`/api/v1/uploads/*`); `--replace` sends `file_id` + `base_version_number` (version by file_id, no name byte-match); a fresh push omits `file_id`.
- `bb pull <id-or-path>` — streaming download + decrypt (constant memory); legacy decrypt preserved (JSON-blob chunks, pre-`bb repair` binary-UUID files → buffered path). **Never silently overwrites**: `-f/--force` overwrites; a rich TTY gets `y/N`; non-interactive runs refuse with non-zero exit naming `--force`/`-o`. Guarded before any bytes download (`tests/pull_overwrite.rs`). Folder pulls guard only file-request uploads (attacker-named file must not replace a local file; skip, exit 3).
- Upload/share refused by account state: `409 plan_required | account_lapsed` (older: `413 quota_exceeded`) → typed messages via `explain_account_refusal` (cached once per `ApiClient`; happy path pays no extra round-trip). `bb push` pre-flight `check_quota`.
- `bb ls [path]` / `bb mv <src> <dst>` (shared `ApiClient::move_file` parent is tri-state: `None` unchanged, `Some(None)` root null, `Some(Some(uuid))` folder; only an explicit root destination may send null; folder repair omits the parent) / `bb quota`.
- `bb region [list|get|set <europe|country>]` — where NEW uploads are stored, by COUNTRY (`europe` = default, no specific country). The choices come from `GET /api/v1/storage-regions`, never from the binary; `set` sends the row's `id` verbatim and refuses a non-`selectable` (coming soon) row or an unknown name without sending a request. **No city is ever printed**, in any command, whichever server answers (guards `tests/no_city_copy.rs`, `tests/region_choice.rs`). Detail (legacy datacenter ids, older-server folding, `--json` keys): docs/runbook/cli-task-history.md.

### Sharing

- `bb share <path-or-id>` (`--expires`, `--max-opens`, `--passphrase`) — always double-encrypted, web wire format (`src/commands/share.rs`): `wrapped_file_key` = STANDARD base64 of `nonce(12) || AES-256-GCM(raw K_c, FileKey)` (no KDF, no AAD; pinned by the core `share_key_wrap` KAT); link = `{APP_URL}/s/<token>#key=<base64url K_c>`; token client-minted + `owner_wrapped_key`/`owner_wrapped_token` so the owner can rebuild the link. `--passphrase` is a server-checked Argon2id gate, not encryption. `--no-double-encrypt` hidden and errors (README flags guarded by `src/readme_flags_tests.rs`).
- `bb shares` — rebuilds each link from the owner blobs ("link not stored" for older shares) / `bb unshare [share-id]` (interactive picker without args).

### File requests

Account-less links for uploads into your vault; crypto via core's `file_request` module (per-request X25519 keypair; private half wrapped under the master key, public half only in the link fragment). `bb request create|list|rm|send` (mints `<APP_URL>/r/<token>#<R_pub>`; `--delete` errors until the server DELETE route is live — close instead). **Key-derivation context:** the HKDF context for the private-key wrap and the per-upload seal is the EMPTY byte string (the server assigns ids only after client crypto must be done) — every client must use the same constant (module docs in `src/commands/request.rs`).

### Sync & mount

- `bb sync <local> [remote]` — bidirectional, continuous (`--once`, `--daemon`, `--stop`, `--dry-run`, `--force`, `--delete`, `--concurrency`, `--rehash`, `--allow-mass-delete`, `--rebind`); v2 streaming uploads. **Sync never deletes on absence (deletion-safety spec R4/R5):** a file missing from the remote listing is not a deletion — local removal only when the server's own record says `is_trashed` (confirmed tombstones), the file is byte-identical to the manifest, the listing is trustworthy (`ListingHealth`, no truncation, remote root matches `.bb-sync.json` or `root_unreconciled` blocks until `--rebind`), and under the mass-delete breaker (max(50, 10% of tracked); over it nothing is removed, exit 3, daemon never gets `--allow-mass-delete`). Removal is a MOVE into `.bb-trash/<UTC stamp>/<rel>`, never an unlink. An undecryptable download leaves the remote untouched, writes nothing locally, is reported, exit 3. (details: docs/runbook/cli-task-history.md)
- `bb watch` — deprecated alias for `bb sync`.
- `bb mount` — FUSE, `--features fuse` only; release binaries ship WITHOUT FUSE (hidden, stub exits 1 → `bb webdav`; pinned by `tests/mount_availability.rs`) / `bb unmount` / `bb webdav` (`--port`, `--read-only`, `--cache-ttl`, `--no-cache`, `--verbose`).

### Billing

- `bb billing show` (`--json` = raw API merge) / `bb billing usage` / `invoices [--open <id>]` / `portal` (the Beebeeb web billing page) / `addons [purchase <addon_id>]`.

### Two-factor authentication

- `bb 2fa status` — reads `totp_enabled` off `GET /api/v1/auth/me` (NOT the security-score factor `has_totp || has_passkey`, which misreports passkey-only). Enabled/disabled only (no backup-codes-remaining route).
- `bb 2fa setup` / `enable --code` / `disable --code` — both POST only the code, no step-up token. `bb 2fa verify` does not exist (removed; the live route is the login-time partial-token exchange).

### Step-up confirm over OPAQUE

`bb trash empty`, `bb rm --permanent`, `bb account update` ask for the password and exchange it for a single-use `X-Confirm-Token` (`commands::confirm::acquire_confirm_token`). For OPAQUE accounts the plaintext password never reaches the server (confirm-opaque-start/finish; wrong password fails inside `client_login_finish` → "incorrect password"). Only `409 {"opaque_unavailable": true}` falls back to legacy plaintext `POST /auth/confirm`; never retried in plaintext otherwise. Password prompt and OPAQUE state are `Zeroizing<String>`.

### Exit codes and non-interactive runs (`src/exit.rs`)

`0` success · `1` any error · `2` a prompt was needed but stdin is not a terminal · `3` the run finished but left items failed or unresolved (`exit::with_code`). Non-interactive: `bb push` existing name → 2; `bb rm` without `-f` → 2; `bb unshare` without id → 2; `bb sync --once` with failures → 3 (`! incomplete · N failed · M ⚡`). Covered by `tests/non_interactive.rs`.

### Utilities

`bb update` — explicit check/install of the latest CLI release; bypasses the automatic-check cooldown and `BB_NO_UPDATE`, while automatic startup updates keep honoring that opt-out; Homebrew/Scoop installs stay package-manager managed.

`bb speedtest` / `bb repair [--dry-run]` (re-encrypt legacy binary-UUID derivation files) / `bb completions <shell>`.

## Upload pipeline (`src/upload.rs`)

`bb push` and `bb sync` share one streaming driver: `upload::stream_encrypt_upload(api, Arc<MasterKey>, UploadSpec, &dyn ChunkProgress)` on the v2 `/api/v1/uploads/*` route (session init → `chunks/{i}` → `complete`; durable `file_id` keys the file). All direct-upload callers (webdav, mount, repair) use the same three `ApiClient` methods; the legacy `/api/v1/files/upload` route is never called by the CLI.

- **Replace** = versioning by file_id (`base_version_number`); sync/mount/webdav mint fresh ids (`None`). Init body sends plaintext `file_size_bytes`; `profile` sent as `"desktop"` with an explicit chunk plan (the server uses it verbatim); the driver asserts the response plan equals what it sent.
- **Resume** (`resume.rs`): persists `(file_id, upload_session_id)` keyed by path; unchanged size+mtime skips init and re-PUTs every chunk (v2 chunk route is idempotent: `{skipped:true}`); pre-v2 entries are non-resumable.
- Consumes core's `ChunkEncryptor` in a producer task (`spawn_blocking`) feeding a bounded `tokio::mpsc` (cap 1); constant memory (peak ≈ concurrency × ~4 × chunk_size); `expected_total_ciphertext()` = size + 28·chunk_count; `finish()` guards shrink, re-stat guards size change. Cancellation via shared `AtomicBool` + `rx.close()`. Progress via `indicatif` (amber accent, bytes only on server-confirmed chunks; `--json`/`--quiet`/non-TTY → no-op progress). Thumbnails (image/*, ≤64 MiB read) best-effort, non-fatal. `ApiClient` 300 s timeout (SSE stream 24 h). Provenance headers: `X-Beebeeb-Client: cli` + `X-Beebeeb-Client-Version: <CARGO_PKG_VERSION>` on every request (`build_client()`); the self-update checker's GitHub client intentionally does not send them.

## Server-provided names become local paths only via `src/safe_path.rs`

Names come from the server — for file-request uploads from an anonymous stranger — so `out_dir.join(name)` is never used directly:

- `safe_path::safe_child(dir, name, Rules::Portable)` (bb pull folder items): ONE plain component, no symlink target, canonical inside `dir`; unsafe item = skip + warning + exit 3, the rest still pulls.
- `safe_path::default_output_path(name)` (bb pull without `-o`): same check, failure says "pass -o <path>"; explicit `-o` is never checked.
- `bb pull --zip`: `check_component` per entry; unsafe entries left out of the archive (exit 3).
- `bb sync`: `Rules::Host` in `walk_remote` + `safe_rel_join` before local mkdir/download/unlink.
- Windows reserved names/`:`/trailing dots enforced on Windows hosts only. Covered by `tests/pull_path_traversal.rs` + unit tests.

## Download pipeline (`src/download.rs`)

`bb pull` and `bb sync`'s `download_to` share `download::stream_download_decrypt`: incremental body read + `ChunkDecryptor::push_frame`, `.tmp` + atomic rename, constant memory. Frame sizing = `total / chunk_count` (remainder last); no `X-Chunk-Size` sent. Legacy preserved (user-data safety): JSON-blob chunks and pre-`bb repair` binary-UUID files fall back to buffered `crypto::decrypt_file_chunks`. 600 s download timeout. `bb pull --zip` stays buffered by design.

**Direct S3 transfer (task 2173).** `stream_download_decrypt` first tries `direct_download::try_download`: when discovery offers `object-download-v1`, chunks come straight from object storage through grant windows via `ApiClient::storage_client()` (a client of its own: no default headers/cookies/`Authorization`, no redirects, https only), are decrypted in order and published only after `finish()`. The rules are shared pure Rust in `beebeeb_core::direct_transfer`; the CLI only does the HTTP. `--transport api` / `BB_TRANSPORT=api` force the API; `BB_TRANSPORT_TRACE=1` traces decisions (never a URL); `bb pull --json` reports `transport`. A failed direct download is a typed `DownloadError` (`Unreadable`/`Changed`/`Other`): sync decides on the type, never on message text, and a pass never aborts for an unreadable or changed file. No `launchctl`/`systemctl` call is made unless `$HOME` is the account's home (`daemon::service_manager`; override `BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME=1` for an intended custom HOME; no test may call `bb sync --daemon`). Detail: docs/runbook/cli-task-history.md → "2173 direct S3 download".

**Direct S3 upload (task 2173, `object-upload-v1`).** `stream_encrypt_upload` asks for it at init only: `X-Beebeeb-Capabilities: object-upload-v1` goes on `POST /uploads/init` and nowhere else, when discovery offers the direction and nothing forces the API (`BB_TRANSPORT=api`). The 201's `transport` says what the server chose; a server that does not answer `object-upload-v1` gets today's chunk PUTs. A direct session sends each chunk grant -> `PUT` to the storage link (`ApiClient::put_to_storage`, the same link-only client as downloads) -> commit (`direct_upload::upload_chunk`); every decision (answer validation, per-chunk retries, the 4-grant cap) is core's `direct_transfer::upload`, the CLI only does the HTTP. A session's transport never changes after init: a direct session that cannot finish (a node without the routes, a refused link, storage failing, discovery gone on resume) is abandoned and the file inits again WITHOUT the token, once per file per run, and goes through the API. The resume record carries `direct`, so a resumed upload continues on the transport it began on. `bb push --json` reports `transport` per file (`direct` or `api`), additive. **A test run must never reach production**: the CLI reads no `BB_API_BASE`, so a run with no `--api` and no config uses `https://api.beebeeb.io`. Set `BB_REFUSE_NON_LOOPBACK_API=1` on every run of the binary (it exits 2 for a non-loopback API URL before sending anything; `tests/api_guard.rs` scans every test file for a spawn without it) and give every run an explicit local `--api`; unit tests build clients with `ApiClient::new_for_test`, which refuses a non-loopback URL. Run the full suite on a machine where you can put it in a network namespace (`unshare -rn`) when a change touches how the API URL is chosen. Detail: docs/runbook/cli-task-history.md → "2173 direct S3 upload".

## Dependencies

`beebeeb-core` + `beebeeb-types` via Cargo git dependency pinned to a full 40-char SHA — not a floating branch. **Keep the pin aligned with `repos/server/CORE_REV`** (the deploy-verified snapshot); bump `rev` in both lines + `cargo build` + fix what the compiler flags, committed together. Non-exhaustive `match` on core enums is intentional (a new variant is a compile error). Local dev:

```toml
[patch."https://github.com/beebeeb-io/core"]
beebeeb-core = { path = "../core/beebeeb-core" }
beebeeb-types = { path = "../core/beebeeb-types" }
```

CI strips `[patch]` before building (see `release.yml`).

## Config

macOS `~/Library/Application Support/beebeeb/config.json`; Linux `~/.config/beebeeb/config.json`. Contains `api_url`, `session_token`, `master_key`, `email`. Guard like an SSH identity file.

## Design reference / Colors

Terminal mockups: `../../design/hifi/hifi-cli.jsx`. Colors in `src/colors.rs` — amber `#f5b800`, sage green `#8fc18b`, coral red `#e07a6a`; always use `colors::*`, not raw `.truecolor()`.

## How we work (evidence, design, done, parallel agents)

The full rules live in the workspace `CLAUDE.md` → "How we work". The repo-specific instantiation:

- Truth line: `cargo test 2>&1 | tee /tmp/bb-cli-test.log` → per-binary `test result: ok. N passed; 0 failed`; clippy/fmt assert exit code.
- CLI tasks are verified by running the command and pasting its output — compiling is not a verification.
- The core pin is a reviewed number: bump `rev` only together with `repos/server/CORE_REV`, in the SAME commit.

## Graphify

This repo has a knowledge graph at graphify-out/. Read `graphify-out/GRAPH_REPORT.md` before exploring code; run `graphify update .` after modifying code and commit; `graphify query "<question>"`; `graphify path "<A>" "<B>"`.

## Keep shared docs in sync

When you add/change/remove endpoints, types, build commands, or dependencies: update the matching skill file in the workspace `.claude/skills/` directory (beebeeb-api.md, beebeeb-designs.md, beebeeb-stack.md, beebeeb-dev.md). Other agents depend on these being accurate.

## Task history

Pre-distillation CLAUDE.md verbatim: docs/runbook/cli-task-history.md
