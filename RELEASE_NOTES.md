# bb 0.15.0: shared folders and file requests, more honest about read-only

This release brings shared folders and file requests into focus. The CLI now syncs guest changes in shared folders, keeps owner writes owner-only, and refuses writes it cannot prove. It is more explicit about read-only cases, repair, mount (FUSE), WebDAV, and sync. The codebase was split into smaller modules; no behaviour change is intended beyond the items listed.

### What's New

- **Shared folders.** `bb sync` and the mount adopt guest changes in shared folders. An owner's write into a shared folder stays owner-only. Before a move out of a shared root, the CLI adopts foreign descendants. A shared folder is indexed only for rows whose key proves the row. A shared subtree is adopted before every move surface.
- **File requests.** `bb push` and `bb sync` support uploads to file requests. A request-row rename seals under the string key, never the content key. `bb repair` skips request uploads instead of re-sealing their names. The CLI refuses a content write to a request-class row. A request-link token that is not one path segment is refused.
- **Mount (FUSE).** The mount is ported to fuser 0.18 and compiled in CI. It gets an owner-only session ACL and 0o700/0o600 modes. Writes are marked dirty, content is loaded before a resize, and a file's content is kept when it is truncated. The mount refuses a rename of a read-only repair row. It never trashes a row it has not checked or replaced. `node_perm()` and `mount_config()` are extracted as pure functions.
- **WebDAV.** A plaintext-named v2 file is refused on read instead of guessing its pin. A PUT resolves the request row.
- **Sync.** Manifests are bound to the session account. Canonical versions and shared index tombstones converge. Watch reconciliation stays within its bound root. Partial and deferred search index repairs are reported. Client metadata is sent and read only as ciphertext. The stop commands keep working against older servers. The CLI adopts core's owner file-key envelopes. It remembers authenticated custody in account-bound sync state, retains custody facts when the first sync fails, and retains custody history across safe root rebinding. Sign-out repairs an unreadable account binding. Account-wide custody is persisted before inventory I/O.
- **Downloads and uploads.** `bb push` and `bb sync` use direct S3 uploads (`object-upload-v1`) when the server offers them. A no-header download frame above the maximum frame size is refused. A legacy multi-chunk frame size is probed like the read path. A legacy JSON-blob chunk 0 is decided, not Unknown.

### Bug Fixes / Hardening

- A folder rename is never probed. `bb sync` never trashes a row it cannot prove writable. A kept read-only edit is a sync notice with guidance. The mount never trashes a row it has not checked or replaced. A rename of a file added by someone else seals under the string key, never the content key. The mount's source row is resolved before the write check. The keyed write check is the rename source check.
- A request row's pin is read under its resolved name key. The CLI fails closed when the request list cannot load. A request-row rename over the mount takes core's `RenameTarget`.
- The CLI refuses corrupt move destination metadata. An explicit replacement resume is bound to the intended file. Sync identity is compared before remote versions. A legacy invite owner copy is opened only with a possession proof. One owner-only notice is printed per command on every write surface. The CLI says why a plaintext-named row cannot be changed. A rename the device has no key for is refused.
- Replace and rename are refused when the current version content is repair-sealed. An explicit key never falls back to owner keys. An explicit-key download fails at frame 0 again and wipes key copies. `bb repair` writes a verified new version, never trashing first. A repair row is skipped on `file_request_id` alone. The upper-repair request row's resolution is pinned, not just its refusal.
- The FUSE-only name helpers are gated to the fuse feature. The mount's ACL comment is corrected. The mount's owner-only session ACL and 0o700/0o600 modes are added. The mount's owner-only tests are added.

### Compatibility

- **This CLI with an older server:** the server may not advertise the new shared-folder and file-request endpoints. The CLI falls back to the API where possible. A server without the new upload endpoints uses the API for uploads. A server without the request-row features refuses the relevant operations with a clear message. The CLI keeps the stop commands working against older servers.
- **An older CLI with a newer server:** unchanged. 0.14.4 and earlier never ask for the new endpoints.
- Dependency change: `beebeeb-core` / `beebeeb-types` pinned to `d3b40e3` (core main is `125bce8` with the same tree). New dependencies: `notify 8`, `notify-debouncer-full 0.7`, `open 5.4.5`, `indicatif 0.18.6`, `rpassword 7.5.4`. Updated GitHub Actions.

### Verification

- `cargo test --locked --features fuse` on the remote runner: 210 passed, 0 failed, 1 ignored.
- `cargo test --locked` (default features) on the remote runner: 250 passed, 0 failed, 1 ignored.
- Clippy with warnings denied: rc 0 for default and fuse. `cargo fmt --check`: rc 0.
- Release gates: full required CLI CI, five platform builds, draft asset readback with checksum assertions, and an update test from 0.14.4 before publishing. (The lead does those.)

### Install / Update

```sh
bb update
bb --version   # bb 0.15.0
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.4...v0.15.0
