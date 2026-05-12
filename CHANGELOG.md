# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
