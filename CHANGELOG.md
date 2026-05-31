# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
