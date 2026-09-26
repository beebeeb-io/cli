# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.11.0] - 2026-09-26

### Fixed
- **[P0]** `bb share` could not create any share — now mints shares in the web's end-to-end-encrypted wire format (raw-`K_c`-wrapped file key, client-minted token, owner-recoverable), and `bb shares` rebuilds each link.
- `bb pull` never silently overwrites an existing local file — `-f`/`--force` overwrites, a rich TTY prompts `y/N`, every non-interactive run refuses with a non-zero exit.
- `bb share <path-or-id>` accepts vault paths and short IDs (same resolver as `bb pull`), not just full UUIDs.
- `bb push`/`bb sync` on a folder no longer abort on one failed file (e.g. a zero-byte file) — the rest of the folder still uploads, failures are named, `--json` gains `total_failed`/`failed[]`.
- Auth/session/connection errors are human-readable ("Your session expired or was revoked. Run `bb login` to sign in again."; "Can't reach `<api>` — check your connection or `--api`"); `bb whoami`/`bb status` exit 1 when logged out instead of printing placeholder plan data.
- `bb share --passphrase` no longer claims to Argon2id-wrap chunk keys — the passphrase is a server-checked access gate, not encryption.
- `bb mount` on release binaries (no FUSE feature) explains it can't mount and points at `bb webdav` instead of walking through a macFUSE/libfuse3 install this binary can't use; `mount`/`unmount` hidden from `--help` there.
- `bb --help` footer links pages that resolve (`github.com/beebeeb-io/cli`, `beebeeb.io/security`) instead of two 404s.
- `bb --help` lists every top-level command again (`2fa`/`sessions`/`passkey`/`billing`/`account`/`request`/`unmount` had silently dropped out).
- `bb billing show` shows a distinct trial line instead of implying an upcoming charge; `bb billing portal` uses the Mollie-era payment-method route instead of a Stripe-only alias that 400s; API errors surface the server's human message, not just its code; `bb whoami`/`bb quota`/`bb push` read quota straight from the server instead of a nonexistent `bonus_bytes` field.
- README links the stable `beebeeb.io/download/cli` redirect.

### Changed
- **Non-interactive runs (cron, CI, a pipe) fail loudly instead of silently no-oping.** New exit codes: `2` — a prompt was needed but stdin isn't a terminal (`bb push` on an existing name, `bb rm` without `-f`, `bb unshare` with no id); `3` — a one-shot `bb sync --once` finished with failed uploads or unresolved conflicts (summary `! incomplete · N failed · M ⚡`, `--json` carries `ok`/`failed`). `0`/`1` unchanged. Dry runs and the continuous sync watch loop are unaffected.

### Security / Process
- Release pipeline: the `RELEASE_NOTES.md` gate now runs before any build/publish step, not after; the Scoop manifest bump opens a PR against `main` instead of pushing directly to the protected branch.

## [0.10.0] - 2026-09-24

### Added
- `bb mkdir <path>` (`-p` for recursive) — create folders.
- `bb mv <src> <dst>` — rename/move a file or folder; bulk form moves several sources into one destination.
- `bb rm` / `bb restore` / `bb trash list` / `bb trash empty` — trash lifecycle, including multi-target and recursive `rm`, and a `--permanent` irreversible delete gated behind a mandatory step-up token (single target only, since the token is single-use).
- `bb search <query>` — client-side filename search, now backed by a single index fetch + parallel decrypt instead of a per-folder walk.
- `bb ls` gains `-l`/`-a`/`-R`/`--depth`/`--sort`/`-r` and batches name decryption in parallel.
- `bb 2fa status` / `setup` / `enable` / `disable` — TOTP management (ASCII QR + backup codes on setup).
- `bb sessions list` / `revoke` / `revoke-all-others` — session management from the CLI.
- `bb passkey list` / `add` / `remove` — passkey management (`add` opens the environment-correct web passkey page; WebAuthn registration is browser-only).
- `bb billing show` / `usage` / `invoices` — live plan, quota, approximate per-region usage, and invoice PDF download.
- `bb account billing` / `addons` — open the billing portal, view/purchase storage add-ons.
- `bb account update --email <new>` — change account email.

### Changed
- Upload driver (`push`/`sync`/`mount`/`webdav`/`repair`) fully migrated to the v2 `/api/v1/uploads/*` session route.
- `bb sync` reconciles remote deletions instead of resurrecting them, and stops its session cleanly on exit.
- `bb login`'s browser handshake now uses `beebeeb_core::cli_auth` instead of a hand-rolled P-256/AES-GCM/HKDF implementation.
- `bb ls` / `bb search` / `bb trash list` no longer truncate at 200 entries (full cursor pagination).
- Billing prices are now read from the server instead of a local price table.
- Every API request sends `X-Beebeeb-Client` / `X-Beebeeb-Client-Version` headers.
- `bb whoami` handles the `Plan::Starter` variant.

### Removed
- Non-functional `bb account export`/`bb account delete` stub subcommands (those flows live in the web app).

### Security
- The OPAQUE zero-key fallback was removed from login — a failed key derivation now fails loudly instead of silently degrading.
- Decrypted login credentials are wrapped in `Zeroizing` and wiped from memory on drop.

## [0.9.1] - 2026-05-31

### Fixed
- Release pipeline: the CI login smoke test now runs against a fresh 2FA test account (the previous one was stranded by a server-side OPAQUE KSF change), and Dependabot/Release `plan` no longer break on a stray committed `[patch]`. **No changes to the `bb` binary vs 0.9.0** — this is a maintenance re-cut to produce a clean release run.

## [0.9.0] - 2026-05-31

### Added
- `bb request` commands — mint account-less links that let anyone upload an end-to-end-encrypted file into your vault (`create`, `list`, `rm`, `send`). Per-request X25519 keypair; the private half is wrapped under your master key, the public half lives only in the link fragment.
- `bb ls` and `bb pull` now decrypt files received through a file request.
- Transient upload retry with backoff and cross-run resume — an interrupted `bb push`/`bb sync` re-uploads only the chunks that are still missing.

### Changed
- Adopt the shared core N=32 chunk ladder (`ChunkProfile::Cli`): larger files use larger chunks (up to 128 MiB) for fewer round-trips, and sync concurrency now feeds the chunk plan.

### Fixed
- Downloads of files larger than ~64 MiB are framed by the server's `X-Chunk-Size` header instead of a `total / chunk_count` average — fixing undecryptable downloads when the chunk size is not 4 MiB.

## [0.3.2] - 2026-05-13

### Added
- Parallel file uploads in sync — default 4 concurrent, configurable with `--concurrency N`
- Content-hash dedup (SHA-256) — `touch` no longer triggers unnecessary re-uploads
- Upload throttle limits shown in `bb whoami` (per plan: 5-100 GB/hr)
- `bb pull` accepts short ID prefixes (e.g., `bb pull 3e15382b`)
- `--json` support for `bb config`
- Encryption performance transparency page (`docs/encryption-performance.md`)
- Repo badges (CI, version, license, tech stack)

## [0.3.1] - 2026-05-13

### Added
- `bb pull` accepts plaintext file paths (e.g., `bb pull Music/notes.md`) in addition to UUIDs

### Fixed
- `bb sync` now sends `file_id` in upload metadata — filenames are correctly decryptable after sync
- `bb ls` gracefully shows `(encrypted)` for files with undecryptable names instead of crashing
- Share URLs now use `https://app.beebeeb.io` instead of `localhost:5173` (server APP_URL config)

## [0.3.0] - 2026-05-13

### Added
- Interactive share picker for `bb unshare` — arrow keys to select, Enter to confirm
- `bb sync` now does continuous watch after initial sync (merges old `bb watch`)
- `bb sync --daemon` installs a macOS LaunchAgent for auto-start on login
- `bb sync --stop` removes the daemon
- `bb sync --once` for one-shot mode (old default behavior)
- Shared path resolution module — `bb ls Music/` works with plaintext folder names
- Compact WebDAV activity logging (request counter instead of per-request lines, `--verbose` for full log)
- Dashboard UI for sync with box-drawn panel showing status, file count, and speed

### Changed
- `bb status` is now an alias for `bb whoami`
- `bb watch` is now an alias for `bb sync` (with deprecation notice)
- Region display shows user's selected region ("Europe") instead of DC details
- Provider name ("Hetzner") removed from all CLI output
- WebDAV handles URL-encoded paths correctly (no more PROPFIND 404 spam)
- Help screen updated with cleaner layout

### Removed
- `bb rotate` stub (key rotation will be designed separately)

## [0.2.1] - 2026-05-13

### Fixed
- Homebrew upgrade now restores the bin symlink — `bb --version` correctly shows the new version after `brew upgrade`
- Improved Homebrew install detection for the broken-symlink edge case

## [0.2.0] - 2026-05-13

### Added
- Custom branded help screen with box-drawn header, colored sections, column alignment
- `bb speedtest` — network latency, upload/download throughput, crypto benchmarks, effective throughput, tiered verdict with practical estimates
- File type icons in `bb ls` (📁 folders, 🖼 images, 📄 docs, 🎬 video, 🎵 audio, 📦 archives)
- Relative timestamps ("2h ago", "yesterday", "3 days ago") in ls and shares
- Summary footers on ls ("5 items · 3.8 MB · e2ee")
- Visual quota bar in whoami and quota (color transitions: green → amber → red)
- Region latency ping in whoami
- Per-file upload progress with speed metrics in push
- Download speed + decrypt timing in pull
- Passphrase entropy display in share
- Status indicators in shares (● active, ○ expired, ✗ revoked)
- Directional arrows in sync (↑ upload, ↓ download, ⚡ conflict)
- Live event log with timestamps in watch
- `--json` flag on all commands (structured JSON output for scripting)
- `--quiet` flag on all commands (minimal output, no progress)
- `--no-color` flag + `NO_COLOR` env var support

### Changed
- All commands use shared `ui` module for consistent colors and formatting
- WebDAV suppresses macOS metadata 404 noise (.DS_Store, ._, Spotlight)
- Cleaner startup banners for webdav and watch

## [0.1.6] - 2026-05-13

### Added
- `bb repair` command — auto-migrates files encrypted with old binary-UUID key derivation to string-UUID (web-app compatible). Supports `--dry-run`.

## [0.1.5] - 2026-05-13

### Fixed
- Key derivation now uses string UUID (matches web app) — files uploaded via CLI are decryptable in the web app and vice versa
- All commands (push, sync, watch, webdav, share, mount) use consistent key derivation

## [0.1.4] - 2026-05-13

### Added
- OTA self-update with colored status message (`Updated bb v0.1.3 → v0.1.4`)
- Homebrew-aware updates (runs `brew upgrade` when installed via Homebrew)
- Shell installer: `curl -fsSL https://get.beebeeb.io | sh`
- `bb shares` now decrypts filenames

## [0.1.3] - 2026-05-13

### Added
- OTA self-update: checks GitHub releases on startup, auto-downloads and replaces binary
- Comprehensive README with full command reference, sync/watch/WebDAV guides, security model
- CONTRIBUTING.md and CHANGELOG.md

### Fixed
- Filename decryption for web-uploaded files (dual key derivation: binary + string UUID)
- Chunk content decryption for web-uploaded files (raw binary + JSON format support)
- `bb push` now sends client-generated file_id so encryption keys match stored file ID
- `bb shares` decrypts filenames (was showing "unknown" for all entries)
- `bb quota` shows real plan data instead of "unlimited"
- WebDAV filters macOS metadata files (.DS_Store, ._, Spotlight, etc.)
- Push conflict detection works with web-uploaded filenames
- Watch and sync commands handle web-app file format

### Security
- Nothing yet.

## [0.1.2] - 2026-05-13

### Added
- Nothing yet.

### Changed
- Production and open-source readiness improvements

### Fixed
- Nothing yet.

### Removed
- Nothing yet.

### Security
- Nothing yet.

## [0.1.1] - 2026-05-13

### Added
- `bb login` interactive authentication
- `bb upload` and `bb download` for single files and directories
- `bb ls` with human-readable file listing
- `bb mkdir` for remote directory creation
- `bb share` to create and manage shared links
- `bb pull` and `bb push` for bidirectional sync
- WebDAV mount for native filesystem access

### Changed
- Nothing yet.

### Fixed
- Nothing yet.

### Removed
- Nothing yet.

### Security
- Session tokens stored in local config file with restricted permissions
- No plaintext passwords stored on disk
