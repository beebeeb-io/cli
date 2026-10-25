# beebeeb-io/cli — task history archive

Created 2026-10-24 when `repos/cli/CLAUDE.md` was distilled to evergreen rules only
(≤150 lines). This file is the pre-distillation `CLAUDE.md` preserved verbatim so that
nothing was deleted: every rule, ruling, incident narrative, proof reference and command
history survives here. `CLAUDE.md` now holds the evergreen distillation and points here
for the details of moved content.

---

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

- `bb login` — browser-based device authorisation. The handshake crypto (P-256 ECDH + HKDF-SHA256 + AES-GCM handoff, with raw fallback for v0.4 web apps) lives in `beebeeb_core::cli_auth` (`CliEphemeralKey` / `decrypt_browser_payload`) — `login.rs` no longer hand-rolls it (task 0861). Supports `--headless` for SSH boxes. CLI auth sessions stored in Redis (HA-safe across API servers). **Task 1734:** the link `bb login` prints carries NO code — the user TYPES the `Authorization code:` line into the approval page (a link someone else sends has nothing to approve), and the page asks for the account password before it releases anything. The WS init frame (`login.rs::init_frame`) adds `client`/`client_version`/`os`/`device_name` for the page to show as "reported by the device"; the server ignores what it does not recognise. Unit tests: `commands::login::tests`.
- `bb signup` — **does not create accounts** (task 1037: signup starts a trial that needs a payment mandate, collected only by the web checkout). Prints "Create your account at <web>/signup — then run `bb login`.", best-effort `open::that`s it (not when `--json`/`--quiet`/headless), exits 0. The web host comes from `src/web_url.rs` (`APP_URL` env, else derived from the configured API: `api.X` → `app.X`, localhost → `localhost:5173`, unknown host → itself; shared with `bb passkey add`). The CLI has no registration code at all (the old `#[allow(dead_code)] ApiClient::signup` was removed).
- **Onboarding document (task 1750, epic 1725 spec T13).** `ApiClient::get_onboarding(with_session)` reads `GET /api/v1/onboarding` (headers: `X-Beebeeb-Client: cli`, `X-Beebeeb-Onboarding-Schema: 1`, `X-Beebeeb-Client-OS`, `X-Beebeeb-Store-Channel: direct`; 5 s timeout; 404/405 → `Ok(None)`). `src/account_state.rs::OnboardingDoc` is the lenient reader (unknown fields ignored, only schema major 1 parses, unknown `signup.mode` → `web_only`, unknown `account.state` is a label and the capabilities decide). It **changes words, never behaviour**: `bb signup` still only points at the web (the CLI is never lifted for native signup, policy matrix 1741), and adds one explanatory line from `signup.mode`/`allowed`/`reason` plus a `signup` key (the server block verbatim, `null` without the endpoint) in `--json`. `bb whoami` shows the document's `account.state` label, its explanation and the denied capabilities (`--json`: `account`, `null` without the endpoint); plan and quota rows stay the subscription's. Any failure or non-major-1 answer = the legacy `/billing/subscription` logic above, unchanged. **Contract copy:** `contracts/onboarding/` is vendored byte-for-byte from `repos/server/contracts/onboarding/` (drift guard: `make check-onboarding-contract` at the workspace root); unit tests in `account_state.rs` and `tests/onboarding_state.rs` read the vendored fixtures. Re-vendor with `cp -R repos/server/contracts/onboarding contracts/onboarding` and update the fixture count in `every_vendored_fixture_parses_as_schema_major_one`.
- `bb logout` — end the current session.
- `bb whoami` — show email, device, region, quota, and **account state** (`src/account_state.rs`, parsed off `GET /billing/subscription`'s `account_state` / `data_deletion_at`; missing or unknown ⇒ `ok`). A `needs_plan` / `lapsed` account shows plan `none`, upload `blocked` and the notice; `--json` carries `account_state` + `data_deletion_at`. `bb quota` and `bb billing show` show the same notice. Exits 1 (message on stderr, no placeholder plan data) when logged out or when the server rejects the session.
- `bb status` — connection + session + storage status (same exit behaviour as `whoami`).
- Any generic 401 surfaces as "Your session expired or was revoked. Run `bb login` to sign in again." (`api::SESSION_EXPIRED_MESSAGE`; the stable `unauthorized` code is kept on `ApiError` for the `classify_*` helpers). A connect failure surfaces as "Can't reach <api_url> — check your connection or --api".
- `bb config` — print current configuration with secrets masked.

### Files

- `bb push <path>` (alias `bb upload`) — encrypt and upload a file or folder. Uses the **v2 upload-session route** `/api/v1/uploads/*` (`init` → `chunks/{i}` → `complete`), keyed by `upload_session_id`. `bb push --replace` sends the existing file's `file_id` + `base_version_number` so the server versions **by file_id** (snapshots the prior version, bumps `version_number`, UPDATEs in place) — no `name_encrypted` byte-match. A fresh push omits `file_id`.
- `bb pull <id-or-path>` (alias `bb download`) — **streaming** download + decrypt (constant memory, live progress bar). Preserves legacy decrypt: JSON-blob chunks and pre-`bb repair` binary-UUID files fall back to the buffered path. **Never silently overwrites** an existing local file (single-file and `--zip` output): `-f/--force` overwrites; otherwise a rich TTY gets a `y/N` prompt and every non-interactive run (`--json`, `--quiet`, piped stdin) refuses with a non-zero exit naming `--force` / `-o <path>`. Checked before any bytes download. Covered by `tests/pull_overwrite.rs` (real `bb` binary vs an in-process mock API). Recursive folder pulls into an existing directory are guarded only for file-request uploads (an attacker-named file must not replace a local file; skipped, exit 3, `--force` overrides) — ordinary files in a folder pull are not guarded yet.
- **Upload / share refused for account state (task 1037).** The server refuses upload init and share create with `409 { error: "plan_required" | "account_lapsed" }` (older path: `413 quota_exceeded`). `needs_plan` → "Your account has no plan yet — choose one at <web>/choose-plan"; `lapsed` → "Your trial has ended; your vault is read-only and will be deleted on <date>. Subscribe at <web>/billing?view=change". `bb push`'s pre-flight `check_quota` reads it off the subscription GET it already makes. Every uploader (via `ApiClient::upload_init_typed`) and `ApiClient::create_share` go through `explain_account_refusal`: failure path only; `plan_required` needs no lookup, `account_lapsed` fetches the subscription for the deletion date (still explains without it), `quota_exceeded` is rewritten only if the subscription says the account can't upload. The lookup is cached once per `ApiClient` (`tokio::sync::OnceCell`); code + status are kept. Classification is `account_state::Refusal`. The happy path never pays an extra round-trip.
- `bb ls [path]` — list vault contents.
- `bb mv <src> <dst>` — move/rename files and folders, including `/` as the root destination. The shared `ApiClient::move_file` parent argument is tri-state: `None` leaves the parent unchanged (rename-only), `Some(None)` sends `parent_id: null` (root), and `Some(Some(uuid))` sends the destination folder UUID. Single/bulk mv, WebDAV MOVE and FUSE rename preserve this distinction; folder repair omits the parent. Only an explicit root destination may send null; missing destination entries or missing/invalid folder UUIDs abort before PATCH. Restore uses its separate bodyless POST endpoint.
- `bb quota` — storage usage with a colour-coded bar.
- `bb region [list|get|set <europe|country>]` — where NEW uploads are stored, **by COUNTRY (task 2050, Guus 2026-10-19; was by datacenter in task 1919)**. **Model:** `europe` is the default = no specific country, the server places the data in any of its European data centers; below it the user picks a country (`de`, `fi`); a country that is not live (`nl`) is listed "coming soon" and is not selectable. **No city is ever printed**, in any command, whichever server answers (guard: `tests/no_city_copy.rs` scans the production source + README for Falkenstein / Helsinki / Nuremberg / Nürnberg / Ede; `tests/region_choice.rs` drives the real binary against an older-server mock full of cities). **The list comes from the API, never from the binary** (`src/region.rs` reads `GET /api/v1/storage-regions`, a public bare array of `{id: europe|de|fi|nl, kind: region|country, name, country, country_code, status: live|coming_soon, selectable, is_default}`, no city key; `/me/region` carries the same array as `choices`). `set` matches what the user typed against id, then name, then country code (case-insensitive), refuses a non-`selectable` row (coming soon) or an unknown name WITHOUT sending a request, and sends the row's `id` verbatim in `PUT /api/v1/me/region {preferred_region}`. **Legacy datacenter ids** (`hel1`, `fsn1`, `nbg1`): not in the list, but datacenter-SHAPED (2-6 letters + 1-2 digits, recognised by shape, no list in the binary), so they are sent to the server as typed; server #251 maps them to their country for one release (the answer is read back as the country). A **city** typed is not a choice: "Storage is chosen by country now. Try 'de' or 'fi'." with the ids taken from the list, no city list. **Older servers** (before #251) send one `datacenter` row per city: `parse_choices` FOLDS them by `country_code` into one row per country (a row with no country is dropped), remembers the datacenter ids as `aliases`, sends the first selectable datacenter id for the country (that server only stores those; a typed alias is sent as typed) and reads a stored datacenter id as its country. A server `region_unavailable` is shown as "isn't available right now. Your choice is unchanged". The CLI sends no region on upload and exposes no strict flag (`region_strict`, server task 1952, is set in the web/mobile apps; note `bb region set` sends only `preferred_region`, which the server reads as strict=false). `bb whoami` / `bb account show` / `bb speedtest` resolve the stored id against `choices` (`region::current_label` / `current_name`: country only); a stored id that the list no longer carries reads "not available right now". `--json`: `bb region --json` prints the server's rows verbatim (older server: synthetic country rows with the same stable fields), `bb region get --json` and `set --json` keep their old keys and add `country` + `country_code`, `bb billing usage --json` `by_region[]` keeps `region` and a deprecated `city` alias that carries the COUNTRY name and adds `country` (grouped by country; no `storage_location.country` = "Europe"). `bb billing show` prints "Europe" for the subscription's billing region. Tests: `src/region.rs` + `commands::region` unit tests, `tests/region_choice.rs`, `tests/no_city_copy.rs`.

### Sharing

- `bb share <path-or-id>` — create an encrypted share link; accepts a vault path, the short ID `bb ls` prints, or a UUID (same resolver as `bb pull`: `commands::pull::resolve_file_arg`) (`--expires`, `--max-opens`, `--passphrase`). Always double-encrypted, in the **web wire format** (see "Share wire format" in `src/commands/share.rs`): `wrapped_file_key` = STANDARD base64 of `nonce(12) || AES-256-GCM(raw K_c, FileKey)` — no KDF, no AAD, pinned against the core `share_key_wrap` KAT vector; link = `{APP_URL}/s/<token>#key=<base64url K_c>`. The token is client-minted (`core::share_token`) and sent with `owner_wrapped_key` + `owner_wrapped_token` (both wrapped under the master key) so the owner can rebuild the link. `--passphrase` is a server-checked Argon2id gate, not encryption. `--no-double-encrypt` is hidden and errors (the server refuses non-E2E shares). README flags are guarded by `src/readme_flags_tests.rs` (`readme_flags_exist`).
- `bb shares` — list active share links; rebuilds each link from `owner_wrapped_key`/`owner_wrapped_token` (shares made by older clients without those blobs show "link not stored").
- `bb unshare [share-id]` — revoke a share link (interactive picker without args).

### File requests

Account-less links that let **anyone** upload an encrypted file *into* your vault. Crypto via the `core` crate's `file_request` module (per-request X25519 keypair; private half wrapped under the master key, public half only in the link fragment).

- `bb request create [--folder <id|name>] [--max-files N] [--max-bytes 100MB] [--expires 7d] [--title ..] [--desc ..]` — mints a request and prints the link `<APP_URL>/r/<token>#<R_pub>`. The keypair is generated locally; only the wrapped private key reaches the server.
- `bb request list` — table of your requests (title, files, bytes, status, expiry) plus each rebuilt link (derives `R_pub` by unwrapping the stored private key under your master key).
- `bb request rm <id>` — close the request (stops new uploads). `--delete` hard-deletes (note: the server `DELETE` route is not live yet; until it lands, `--delete` returns a clear error and you should close instead).
- `bb request send <url> <file>...` — account-less upload **to** a request. Parses the `token` + `#<R_pub>` from the link, seals a per-file content key to `R_pub` via `core`, and POSTs the encrypted blob to the configured API. Fails loudly if the link has no public-key fragment.

**Key-derivation context:** the HKDF context for both the private-key wrap and the per-upload seal is currently the **empty byte string**, because the server assigns the request id / token / file id only *after* the client crypto must already be done. Every client (web/cli/mobile) must use the same constant — see the module docs in `src/commands/request.rs` and the cross-client decision note.

### Sync & mount

- `bb sync <local> [remote]` — bidirectional folder sync (continuous by default; `--once`, `--daemon`, `--stop`, `--dry-run`, `--force`, `--delete`, `--concurrency`, `--rehash`, `--allow-mass-delete`, `--rebind`). V2 **streaming** uploads (constant memory). Remote path auto-strips `~/` home prefix. Gracefully handles 409 stuck uploads. **Sync never deletes on absence (tasks 1915 + 1916, deletion-safety spec 2026-10-17 R4/R5):** a file missing from the remote listing is not a deletion. A local file is removed only when the server's own record for that id (`GET /files/{id}`, via `sync_safety::confirm_tombstones`) says `is_trashed`, the file is still byte-identical to the manifest (re-checked right before the move, not from the scan), the listing is trustworthy (`sync_safety::untrusted_reason`: not empty while the manifest has files, no skipped/undecryptable/unsafe entries via `ListingHealth`, no 50,000-entry truncation (only when entries were discarded or a cursor remains), and the remote root is the one `.bb-sync.json` last synced against; on a mismatch the old `remote_folder_id` is KEPT, so `root_unreconciled` blocks removals in every later pass and in both watch loops until `bb sync --rebind`, which adopts the new folder and drops the old manifest), and the pass stays under the mass-delete breaker (max(50, floor(10 % of tracked files)) removals, so 51 of 501 trips; over it nothing is removed and the run exits 3 until `--allow-mass-delete`, which the daemon never gets). Removal is a MOVE into `.bb-trash/<UTC stamp>/<rel>` inside the sync folder (dot-prefixed, so never scanned or uploaded), never an unlink. A missing file with no tombstone (moved/renamed remotely, 404, another account) is kept and reported. The watch loops never mass-delete. A download that fails to decrypt (`do_download` -> `DownloadOutcome::Unreadable`) leaves the remote copy untouched, writes no local file or manifest entry, is reported, makes a one-shot pass exit 3, and is retried by the next full pass; no code path in sync trashes a remote file because of a decrypt error. While the root is unreconciled, `trusted_prior` also treats an old manifest entry as absent for any file that exists in the current remote folder, so a same-named file is a both-sides conflict (skipped unless `--force`, which keeps its usual local-replaces-remote meaning) and never a trash, replace-upload or download-over-local on the old entry's account; the one-shot pass and both watch loops use it. An explicit `--delete` on an unreconciled root trashes nothing (its "synced before, missing locally" entries belong to the old tree), prints a notice naming `bb sync --rebind` and exits 3; after `--rebind` it behaves as before. Not covered here: `--delete` on a trusted root (explicit local-to-remote trashing), and sync's trash-then-upload replace (spec H15). Shows a scan spinner, then live per-file + overall progress bars (rich TTY only). `--rehash` forces a full re-hash of every file instead of trusting unchanged `(size, mtime)` entries from the last sync.
- `bb watch <path>` — deprecated alias for `bb sync`.
- `bb mount <mountpoint>` — FUSE mount, **source builds with `--features fuse` only**. Release binaries (cargo-dist) ship WITHOUT FUSE and no FUSE asset is published: there `mount`/`unmount` are hidden from `--help` and the stub exits 1 pointing at `bb webdav` (pinned by `tests/mount_availability.rs`). V2 uploads.
- `bb unmount <mountpoint>` — unmount a previously mounted vault.
- `bb webdav` — serve the vault as a local WebDAV server (`--port`, `--read-only`, `--cache-ttl`, `--no-cache`, `--verbose`).

### Billing

- `bb billing show` — read-only plan, storage, and renewal info. `--json` outputs the raw API merge.
- `bb billing usage` / `bb billing invoices [--open <id>]` / `bb billing portal` / `bb billing addons [purchase <addon_id>]` — per-region usage, invoice list + PDF, Beebeeb web billing page, add-ons.

### Two-factor authentication

- `bb 2fa status` — TOTP enabled/disabled. Reads `totp_enabled` off `GET /api/v1/auth/me` (TOTP-specific), not `GET /api/v1/account/security-score`'s `two_factor_enabled` factor (which is `has_totp || has_passkey` — would misreport a passkey-only account). No live route exposes backup-codes-remaining or a last-verified timestamp yet, so `status` shows enabled/disabled only.
- `bb 2fa setup` — begins setup, prints the QR/secret/backup codes (`src/commands/twofa.rs`, task 0478). `bb 2fa enable --code <6 digits>` activates it; `bb 2fa disable --code <6 digits>` turns it off (task 0479) — both POST only the code, no step-up token (the live server routes enforce the code alone, not the plan's assumed `X-Confirm-Token`).
- `bb 2fa verify` **does not exist** — removed from the clap tree (task 0479). The live `POST /api/v1/auth/2fa/verify` is the login-time `{partial_token, code}` exchange, but `bb login`'s browser handshake never produces a partial token, so there was no CLI caller for it.

### Step-up confirm over OPAQUE (task 0856)

`bb trash empty`, `bb rm --permanent` and `bb account update` ask for the account password and exchange it for a single-use `X-Confirm-Token` (`commands::confirm::acquire_confirm_token` -> `ApiClient::confirm_password`). For an OPAQUE account the plaintext password never reaches the server: `beebeeb_core::opaque_protocol::client_login_start`, `POST /auth/confirm-opaque-start`, `client_login_finish` (with the `ksf_version` the server returns), `POST /auth/confirm-opaque-finish` (mirrors web's `confirmAction`). A wrong password fails inside `client_login_finish`, so no finish request is sent; it is reported as `unauthorized` -> "incorrect password". Only `409 { "opaque_unavailable": true }` on the start call (account with no OPAQUE credential) falls back to the legacy plaintext `POST /auth/confirm`; any other 409 or error is surfaced, never retried in plaintext. The password prompt is a `Zeroizing<String>`, as are the OPAQUE client state and the session/export keys. `bb account update` still sends the password in the body of the legacy `PUT /auth/account/email`; that route is not part of this change. Tests: `commands::confirm::tests` (real OPAQUE server half as the mock, every request body scanned for the password) and `tests/step_up_opaque.rs` (the real `bb` binary under a pseudo-terminal, unix only).

### Exit codes and non-interactive runs (`src/exit.rs`)

`0` success · `1` any error · `2` a prompt was needed but stdin is not a terminal · `3` the run finished but left items failed or unresolved. A command sets `2`/`3` by building its error with `exit::with_code(code, msg)`; `main` exits with `exit::code()`.

With stdin not a terminal (cron, CI, a pipe), nothing prompts and nothing silently no-ops:

- `bb push <name>` where the name exists → exit 2, "pass --replace or --keep-both".
- `bb rm <target>` without `-f` → exit 2, nothing trashed (`--json`/`--quiet` keep skipping the prompt as before).
- `bb unshare` with no id → exit 2, "pass a share id … `bb shares`" (the picker needs a terminal).
- `bb sync` one-shot pass (`--once`, `--json`, `--quiet`) with failed uploads or unresolved conflicts → summary says `! incomplete · N failed · M ⚡`, exit 3; `--json` carries `ok` + `failed`. Dry runs and the continuous watch loop don't fail on it.

Covered by `tests/non_interactive.rs` (runs the real binary with `stdin = null` against an in-process mock API, scratch `HOME`, `BB_NO_UPDATE=1`).

### Utilities

- `bb update` — explicitly check/install the latest CLI release; bypasses automatic-check cooldown and `BB_NO_UPDATE`. Automatic startup updates keep honoring that opt-out. Homebrew/Scoop installations remain package-manager managed.

- `bb speedtest` — benchmark network throughput + crypto speed against the API.
- `bb repair [--dry-run]` — re-encrypt files stuck in the legacy binary-UUID key derivation so they open in the web app.
- `bb completions <shell>` — print a shell completion script (`bash`, `zsh`, `fish`, `powershell`).

## Upload pipeline (`src/upload.rs`)

`bb push` and `bb sync` share **one** streaming upload driver:
`upload::stream_encrypt_upload(api, Arc<MasterKey>, UploadSpec, &dyn ChunkProgress)`.

- **v2 transport.** The driver runs entirely on the v2 `/api/v1/uploads/*`
  route: `upload_init` opens an upload **session** (`POST /uploads/init`,
  returning `upload_session_id` + the durable `file_id` + the server-derived
  chunk plan), each chunk PUTs to `PUT /uploads/{session}/chunks/{index}`, and
  `POST /uploads/{session}/complete` finalises the version. The session id keys
  the chunk/complete calls; the durable `file_id` keys the file (and thumbnails).
  All other direct upload callers (`webdav.rs`, `mount.rs`, `repair.rs`) use the
  same three `ApiClient` methods. The legacy `/api/v1/files/upload` route is **no
  longer called by the CLI** — `repair.rs` was the last caller; it was migrated to
  v2 in the fix/cli-repair-v2-upload branch (task 0689 follow-up).
- **Replace = versioning by file_id.** `UploadSpec.base_version_number` is
  `Some(n)` on a `bb push --replace`; the init sends `file_id` + that base
  version, and the server versions in place (stale-version → 409). `bb sync` and
  `bb mount`/`bb webdav` mint fresh ids (sync trashes + re-uploads), so they pass
  `None`. The init body sends the **plaintext** `file_size_bytes`; the server
  recomputes the stored encrypted total at complete. `profile` is sent as
  `"desktop"` (the v2 route has no `"cli"` profile; the CLI sends an explicit
  `chunk_size_bytes`/`chunk_count` plan which the server uses verbatim, so the
  wire profile is non-load-bearing) — the driver asserts the init response's plan
  equals what it sent before streaming.
- **Resume.** The sidecar (`resume.rs`) persists `(file_id, upload_session_id)`
  keyed by path. On resume (unchanged `size`+`mtime`) the driver skips `init`
  (re-init of an in-progress file is a 409) and re-PUTs every chunk to the stored
  session; the v2 chunk route is **idempotent** (an already-stored chunk returns
  `{skipped:true}` without re-writing the blob), so re-PUTs are cheap. Entries
  written by an older (v1) CLI lack a session id and are treated as
  non-resumable.

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
- Every request the `ApiClient` sends carries `X-Beebeeb-Client: cli` and
  `X-Beebeeb-Client-Version: <CARGO_PKG_VERSION>` (set as `default_headers` on
  the shared `reqwest::Client` in `build_client()`, task 1392) — the same
  version string `bb --version` prints. The server records both on every
  `object_versions` row (server PR #23 / task 1369) for writer-provenance
  queries. The self-update checker's separate client (`update.rs`, talking to
  `api.github.com`, not the beebeeb API) intentionally does not send these.

## Server-provided names become local paths only via `src/safe_path.rs` (task 1733)

File and folder names come from the server — and for **file-request uploads from an anonymous stranger** (the uploader seals the name under a content key *they* chose). `out_dir.join(name)` is therefore never used directly:

- `safe_path::safe_child(dir, name, Rules::Portable)` (`bb pull` folder items): the name must be ONE plain component (no empty/`.`/`..`, `/`, `\`, NUL, control chars, `C:` drive prefix), the target must not be a symlink, and its canonical location must stay inside `dir`. An unsafe item is **skipped with a stderr warning and the run exits 3** (`exit::INCOMPLETE`); the rest still pulls.
- `safe_path::default_output_path(name)` (`bb pull <id>` without `-o`, folder default dir): same check; failure says "pass -o <path>". An explicit `-o` is the user's own choice and is never checked.
- `bb pull --zip`: `check_component` per entry name; unsafe entries are left out of the archive (exit 3) — they would be zip-slip entries for whoever extracts it.
- `bb sync`: `Rules::Host` (only what is dangerous on THIS host, so legitimate local names like `a\b` synced earlier keep working) in `walk_remote`, plus `safe_rel_join` before the local mkdir / download / unlink so a symlinked subdirectory cannot redirect a write outside the sync root.

Windows reserved names / `:` / trailing dots are enforced on Windows hosts only (the rule set is unit-tested with `windows = true` on every host). Core has no such helper at the pinned rev; desktop has its own copy of this problem (task 1735). Covered by `tests/pull_path_traversal.rs` (real `bb` vs an in-process mock API serving malicious names) and the `safe_path` unit tests.

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

Uses `beebeeb-core` and `beebeeb-types` from the `core` repo via Cargo git dependency, pinned to a full 40-char commit SHA in `[dependencies]` (`rev = "..."`) — **not** a floating branch. This is deliberate: an unpinned `branch = "main"` dependency would let an unrelated core commit change the CLI's build (or behaviour) without a corresponding CLI commit, and CI has no separate core-freshness gate the way `repos/server`'s `CORE_REV` file + `ci.yml` check does.

**Keep the pin aligned with `repos/server/CORE_REV`** (not core `main` tip) — that file names the core commit already deploy-verified in production, so pinning the CLI to the same SHA means every backend-consuming Rust client builds against one shared, known-good core snapshot. Bumping core introduces new types/variants (e.g. `Plan::Starter`, task 1386) that the CLI must handle explicitly — non-exhaustive `match` on a core enum is intentional (no `_ =>` catch-all) so a new variant is a compile error here, not a silent gap. To bump: update the `rev` in both `beebeeb-core` and `beebeeb-types` lines below, `cargo build` to refresh `Cargo.lock`, fix whatever the compiler flags, then commit both files together.

For local development, add to `.cargo/config.toml`:

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

## How we work (evidence, design, done, parallel agents)

The full rules live in the workspace `CLAUDE.md` → "How we work" (also summarised in the workspace `AGENTS.md`). Read them; they apply here. The repo-specific instantiation:

- **The count-shaped truth line:** `cargo test 2>&1 | tee /tmp/bb-cli-test.log` → per-binary
  `test result: ok. N passed; 0 failed`; assert the Ns. `cargo clippy -- -D warnings` /
  `cargo fmt -- --check` assert the exit code.
- **CLI tasks are verified by running the command and pasting its output** into the task file —
  the binary compiling is not a verification.
- **The core pin is a reviewed number:** bump `rev` only together with `repos/server/CORE_REV`,
  `cargo build` to refresh the lock, and fix what the compiler flags in the SAME commit.

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

## 2173 direct S3 download (CLI)

`stream_download_decrypt` first calls `direct_download::try_download`. When discovery (`GET /api/v1/transfer/capabilities`, cached per `ApiClient`, 10 min) offers `object-download-v1`, chunks come straight from object storage through grant windows (`POST /files/{id}/download-grants`; `X-Beebeeb-Capabilities` goes on that call only). Storage links are fetched with `ApiClient::storage_client()`: a client of its own with no default headers, cookies or `Authorization`, no redirects, https only (loopback http when the API base is loopback http); errors from it never carry the URL. Chunks are decrypted IN ORDER by core's `ChunkDecryptor` and published through the same `AtomicFile` only after `finish()`. The rules (authority check, API-answer classification, windows, renewals, fallback) are shared pure Rust in `beebeeb_core::direct_transfer`; the CLI performs the HTTP and keeps no policy of its own. Fallbacks: a refusal or unsupported node sends the whole file down the API path (nothing is on disk before the first chunk decrypts, and a mid-transfer fallback deletes the partial temp file first); a chunk storage cannot give is fetched through the pinned `GET /files/{id}/chunks/{i}?object_version_id=`; 401/403/JSON-404 are terminal, `version_unavailable` aborts with nothing written. `--transport api` (hidden) or `BB_TRANSPORT=api` forces the API; `BB_TRANSPORT_TRACE=1` prints each decision to stderr (never a URL). `bb pull --json` reports `transport` (`direct`, `mixed` or `api`): an additive key, existing keys are unchanged. Tests: `tests/direct_download.rs` (real `bb` against a mock API + a hand-written storage socket: faults, redirects, tampering, no signature in any output).

Fallback rule after review (core #33): before the version is pinned a refusal sends the whole file to the API path; after it, a window the server cannot serve moves the rest of the file to the pinned per-chunk route and keeps what was written (`bb pull --json` then reports `transport: "mixed"`); a failed pinned-route fetch ends the file and restarts it on the API stream. Callers that stay on the API: `bb pull --zip`, file-request downloads, `bb repair`, `bb mount`, `bb webdav` (whole-file buffered reads).

Review fixes (PR #102): the direct path returns a typed `download::DownloadError` (`Unreadable`, `Changed`, `Other`). Message text never decides what a sync pass does: `Unreadable` (a chunk that failed authentication from storage and from the API) is the API path's "could not decrypt, retry next pass" and `Changed` (the pinned version went away) defers the file; neither aborts the pass, because under launchd/systemd an aborted pass is a restart every 30 s for ever. `bb pull <file>` starts over once from fresh metadata on `Changed` (the chunk count and read pin may differ), then reports "changed again". The pinned per-chunk route retries 429, 5xx, a held file (409) and a broken read (4 attempts, 60 s of waiting at most, `Retry-After` honoured, otherwise 1/2/4 s) before the file falls back to the API stream; a body of the wrong length, an authorization answer and a vanished version are final. A 409 on a grant with no `Retry-After` goes straight to the API path (a sync pass defers the file at once); with one it waits that long. In a long-running process the discovery cache that gave up because asking failed asks again after its trust period (a JSON 404 stays remembered). Storage requests have an idle (60 s without a byte) timeout, no total timeout. A discarded partial file takes its bytes back off the run-wide progress counter. `--transport api` is passed on to the detached `bb sync --daemon` child (`BB_TRANSPORT` travels in the environment); installed LaunchAgent/systemd units do not persist it. Candidate for core: the "409 with no Retry-After goes to the API" rule belongs in `classify`, so every client shares it.

Review round 3 (PR #102): **no test and no `bb` run under a redirected HOME may touch launchd or systemd.** A test that ran `bb sync --daemon` under a scratch HOME loaded a LaunchAgent that launchd ran with the REAL home and the developer's live session (it created an empty `/SyncUp` in the real vault). Every `launchctl` / `systemctl` call now goes through `daemon::service_manager`, which refuses unless `$HOME` is the account's home from the passwd database; install and uninstall refuse before writing anything. The refusal names the way through: a user whose HOME is intentionally not the account's (`sudo -E`, a relocated home) runs the command again with `BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME=1` (only the value `1` counts; no test ever sets it). `tests/service_guard.rs` proves it with recording `launchctl`/`systemctl` stubs ahead of the real tools on PATH (so even a broken guard touches nothing real) and scans `src/` for any unguarded call. No test may call `--daemon`. Also: frame 0 failing to authenticate on the direct path goes to the API path (pre-`bb repair` binary-UUID files; index > 0 stays corrupt); the pinned route honours `Retry-After` on a 5xx and has idle limits (60 s without an answer or a body byte); a cut-short stream is `Unreadable`; the daemon child, plist and unit carry `BB_TRANSPORT` explicitly; an empty `BB_TRANSPORT` is unset and `--quiet` silences its warning; a folder pull reads one changed file again instead of aborting, and the retry does not re-ask the overwrite question.

Known and accepted: a file deferred (`Changed`) or left unreadable by a sync pass is retried only at the next daemon start, because watch mode never downloads. The CLI-local retry, classification and discovery policies in this PR go away at the core pin bump that carries `Call::PinnedChunk`, `PinnedRetry` and the expiring `DiscoveryCache`.

Service guard, round 4: on Linux the guard also compares the config directory the process uses (`$XDG_CONFIG_HOME` when absolute, else `$HOME/.config`) with the one a systemd user service would use (`<account home>/.config`), because `XDG_CONFIG_HOME=<scratch>` with the real HOME would otherwise pass. An account home that cannot be determined fails closed (the override still works). Removing a service that is not installed is `Ok(false)` and is never refused. A folder pull reads a changed file again once and stops on every other failure, naming the file.

## 2173 direct S3 upload (CLI)

Core owns the rules (`beebeeb_core::direct_transfer::upload`, core PR C); the CLI makes the requests. Flow of one file: discovery (`GET /api/v1/transfer/capabilities`, cached per `ApiClient` by core's `DiscoveryCache`) -> `POST /uploads/init` with `X-Beebeeb-Capabilities: object-upload-v1` (this call only) when the server offers the direction -> the 201's `transport` (`UploadInit::is_direct`, core's `upload_init_selects_direct`) says whether the session is direct. A direct session sends each chunk through `direct_upload::upload_chunk`: `POST .../chunks/{i}/grant {size}` (core validates the body: same index, a link that passes `check_storage_url`, `PUT`, the one `content-type` the server signs, a parseable expiry; `skipped: true` means the chunk is already committed) -> `ApiClient::put_to_storage` (the upload storage client: no `Authorization`, no cookies, no redirects, https only unless the API is loopback http, and no read timeout, see below; errors never carry the link) -> `POST .../chunks/{i}/commit {size}` (core checks the echo). After any failure `UploadChunkState::failed` says what to do: a new grant (a failed PUT, a commit `chunk_not_stored`), a stated wait (`429`, a 5xx with `Retry-After`), the API for the whole file, or stop (authorization answers; a 401 reads "run bb login"). At most 4 grants per chunk (the server's cap); the 8th failure ends the tries; 150 s of waiting; the client's own backoff is jittered per chunk (`with_seed(rand::random())`); an interrupt cuts a wait short. A commit `409 commit_in_progress` is a HOLD (`Outcome::Held`): after a failed copy the server keeps the chunk's commit lock for 420 s, and core waits it out on an allowance of its own (450 s) that leaves the failure cap and the 150 s budget alone.

Transport exclusivity: a direct session takes no proxy PUT and an API session takes no grant, so there is no per-chunk fallback. When a direct session cannot finish, `stream_encrypt_upload` ends it (`upload_abandon`), clears the resume record, rolls the progress counter back and inits again WITHOUT the token (once per file per run: the file's own `direct_given_up` bit keeps that re-init off the direct route, and the failure is also remembered in the process, for that API base, for the trust period on the MONOTONIC clock, which keeps every later new session off it). A resumed upload continues on the transport recorded in `pending-uploads.json` (`direct`, absent = an API session from an older CLI). A recorded direct session is given up ONLY on a definite no (`--transport api`; discovery that lists no upload; a JSON 404): then it is abandoned and re-inited on the API. If the capabilities cannot be read right now (`DirectOffer::Unknown`: a 5xx, a timeout, a bare 404) the file fails for this run with a message naming `--transport api`, and the session and the record stay: giving it up would throw away every chunk it already holds (a 50 GB push killed at 90 % would start again from 0). If the login is not valid (`DirectOffer::Unauthorized`: no session, or a 401/403 on the lookup) the file fails with `SESSION_EXPIRED_MESSAGE` (the words, and so the exit code, of every other expired session) and the record stays too; `--transport api` would be the wrong advice there, because it ends the session. After a direct session had to be given up, the process starts no new direct upload for that API base for the trust period (`direct_upload_failed_recently`), so direct broken server-wide costs one file, not every file and not an abandon (a `FileDeleted` to the user's other devices) per file.

`bb push --json` reports `transport` per file: `direct`, or `api` (also for a file whose direct session was given up). Existing keys are unchanged.

Discovery: the CLI-local wrapper that re-implemented "ask again after the trust period" for failures and pinned a JSON 404 "no direct transport" for ever is gone. Core's `DiscoveryCache` (2aa1f39, in the core the CLI now pins) forgets a failed discovery AND a JSON-404 `NotSupported` after `DISCOVERY_TTL_MS` (10 min), counted from the last failure. The CLI keeps ONE fact core's `Lookup::UseApi` does not carry (did the server SAY no, or did asking fail? `DirectOffer::NotOffered` vs `Unknown`) and keeps the cache per API base for the whole PROCESS (`DirectState::shared`): the watch/daemon builds a new `ApiClient` for every resync, so a per-client cache never outlived one resync. Two CLI tests that pinned the old rule moved to core's. A 401/403 on the lookup is the one answer that is NOT recorded: core's cache counts it as a failed lookup, two of them park discovery for the trust period (downloads too), and the next `bb login` could not undo it (found by the self-review of cli#105); it is told apart as `DirectOffer::Unauthorized` and asked again next time. The lookup is only made when its answer is used.

Storage PUT client (cli#105 review P2-1): `ApiClient::upload_storage_client()` is the download client WITHOUT the read timeout. reqwest 0.12's `read_timeout` is one timer from the moment the request is sent until the response headers arrive and is NOT reset while the request body goes out, so it capped every PUT at 60 s: a 128 MiB chunk on an uplink under about 18 Mbit/s never went direct. The download client keeps its read timeout (on a GET it resets per read: `a_storage_transfer_that_keeps_moving_is_never_cut_off_but_one_that_stalls_is`). `a_storage_put_that_takes_longer_than_the_read_timeout_to_be_answered_still_succeeds` runs the same slow PUT through both clients: the upload client gets 200, the download-style client times out. It runs in real time (reqwest's timers and real sockets do not mix with a paused clock), so the TEST build compresses the two storage budgets (read timeout 3 s, PUT base 8 s, instead of 60 s each); the release values are pinned by `the_storage_budgets_of_a_release_build_are_what_the_docs_say`.

Test safety (finding of 2026-10-25): the CLI reads no `BB_API_BASE`, so a scratch HOME with no config pointed the binary at production. `BB_REFUSE_NON_LOOPBACK_API=1` (set on every spawn of `bb` in `tests/`, enforced by a source scan in `tests/api_guard.rs`: it removes comments, requires `.env("BB_REFUSE_NON_LOOPBACK_API", "1")` between each spawn and the point where its function runs the command, with nothing after it that takes it away (`.env_clear()`, `.env_remove(..)` of the variable, `.envs(..)`; two spawns had `env_clear` AFTER the guard, so the child never saw it), refuses any alias of `CARGO_BIN_EXE_` and a spawn outside a fn body (a `macro_rules!` body, a `static`), exempts two files by path, recurses into `tests/`, and has fixtures (an alias, a split call, a `concat!` name, a macro body, a comment or doc naming the variable, a guard in another function, `env_remove`, `env_clear`, `envs`, the values `0`, empty and `no`) that must turn it red) makes `config::load_config` exit 2 for a non-loopback API URL, `ApiClient::new_for_test` asserts loopback, and the guard tests point every proxy variable at a closed local port. The full suite was run on the Legion inside `unshare -rn` (loopback only): it caught `tests/auth_errors.rs` running `bb whoami` with no `--api`.

Core pin: this PR is built on core PR C (the upload rules) and the cache of 2aa1f39; see the PR for the exact rev. Tests: `src/direct_upload.rs` (the driver against a scripted API and a storage server of its own, through the real `stream_encrypt_upload`: the wire of server #331, retries, the 4-grant cap, fallbacks, resume, interrupt), `tests/api_guard.rs`, `tests/zero_byte_files.rs` (the `transport` key). Not covered by a unit test: a real server with real storage; see the compatibility matrix in the PR.
