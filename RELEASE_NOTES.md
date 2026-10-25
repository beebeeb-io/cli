# bb 0.14.4: downloads straight from storage

`bb pull` and `bb sync` can now fetch a file's encrypted chunks directly from Beebeeb's object storage instead of streaming every byte through the API. The server hands out short-lived links for one file at a time. The CLI authenticates and decrypts every chunk on your machine exactly as before: the links carry ciphertext only, and your keys never leave the device. When the server does not offer direct downloads, or refuses a link, the file comes through the API as it did in 0.14.3.

### What's New

- **Direct downloads.** The CLI uses time-limited storage links when the server advertises direct downloads (`GET /api/v1/transfer/capabilities`). Chunks are decrypted in order, and the file only appears once it is complete and authenticated.
- **Automatic fallback.** A file that storage cannot serve before the download starts goes through the API instead. A file that fails partway continues on the API's pinned per-chunk route and keeps what was already written.
- **`bb pull --json` reports `transport`** (`direct`, `mixed` or `api`). This is an additive key; existing keys are unchanged.
- **`BB_TRANSPORT=api`** (or `--transport api`) forces the API for every file. `BB_TRANSPORT_TRACE=1` prints each transport decision to stderr, never a URL. `bb sync --daemon` passes the chosen transport on to its child process.
- Uploads still go through the API in this release.

### Bug Fixes / Hardening

- A sync pass no longer aborts on a file it cannot read, or on a file that changes during its download. It defers that file and carries on. Under launchd or systemd an aborted pass meant a restart every 30 seconds.
- `bb pull <file>` starts over once from fresh metadata when the file changes mid-download, then says "changed again" if it changes a second time.
- The pinned per-chunk route retries rate limits (429), server errors (5xx), a file that is still being written (409) and a broken read: 4 attempts, at most 60 seconds of waiting, honouring `Retry-After`. A body of the wrong length, an authorization answer and a vanished version are final.
- `bb sync` no longer mistakes a file ID containing `409` inside a decrypt error for "still uploading". This could make a failed pass exit 0.
- Storage requests use an HTTP client of their own, with no cookies, no `Authorization` header, no redirects and https only. Their error messages never contain a storage URL.
- `bb` refuses to install or remove a LaunchAgent or systemd unit when `HOME` is not your account's home directory (on Linux, also when `XDG_CONFIG_HOME` points elsewhere), because the service would run against a different configuration than the one you see. If your home directory is intentionally relocated (for example `sudo -E`), run the command again with `BB_SERVICE_INSTALL_ALLOW_CUSTOM_HOME=1`. Removing a service that is not installed is never refused.
- A folder pull reads a file that changed during the pull again once instead of aborting, and the retry does not ask the overwrite question twice.

### Compatibility

- **This CLI with an older server:** the server advertises no direct downloads, so every file uses the API, exactly as in 0.14.3.
- **An older CLI with a newer server:** unchanged. 0.14.3 and earlier never ask for storage links.
- Dependency change: `beebeeb-core` / `beebeeb-types` pinned to `3235dc15` (the shared direct-transfer rules). No other dependency versions changed.

### Verification

- Full test suite on Linux, at the commit this release is built from, in a sandbox with no network access and an egress recorder: 730 passed, 0 failed, 4 ignored across 24 test binaries; 0 outbound connection attempts; no service manager touched. Clippy with warnings denied and rustfmt are clean.
- `tests/direct_download.rs` runs the real `bb` binary against a mock API and a hand-written storage socket. It covers faults, redirects, tampered chunks, expired links, the mixed fallback, and that no link signature appears in any output.
- `tests/service_guard.rs` puts recording `launchctl` / `systemctl` stubs ahead of the real tools on `PATH` and scans `src/` for any unguarded call.
- Release gates: full required CLI CI, five platform builds, and draft asset readback with checksum assertions before publishing.

### Install / Update

```sh
bb update
bb --version   # bb 0.14.4
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.3...v0.14.4
