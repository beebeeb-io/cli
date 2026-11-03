# bb 0.15.0: direct uploads, and safer handling of shared folders and file requests

`bb push` and `bb sync` can now upload a file's encrypted chunks straight to Beebeeb's object storage, the same way 0.14.4 started doing for downloads. This release also changes how the CLI treats files you did not create yourself: files other people add to a folder you share, and files someone sends you through a file request. The CLI opens them with the keys meant for them, keeps your own writes private where it has to, and refuses a change it cannot make safely instead of guessing. The code was also split into smaller modules; that split is not meant to change behaviour beyond what is listed here.

### What's New

- **Direct uploads.** When the server offers them, uploads go to object storage over short-lived links, one file at a time. Chunks are encrypted on your machine before they leave it, exactly as before; the links only ever carry ciphertext. When the server does not offer direct uploads, the file goes through the API as it did in 0.14.4. A file that cannot finish directly starts over through the API. An authorization refusal stops that file instead, because the API would refuse it too. `BB_TRANSPORT=api` (or `--transport api`) still forces the API for every transfer.
- **Folders you share.** Files that other people add to a folder you share now sync, and the CLI opens them with the key the folder gives them, never with a key derived from your own account. A file you add to a shared folder from the CLI stays readable only to you. The CLI prints "Only you can open this. Share it from the web or the app." when that happens.
- **Moving files out of a shared folder.** Before a file someone else added leaves a shared folder, the CLI first takes it over under your own account, so you can still open it after the move. If that cannot be done, the move is refused and nothing changes.
- **Files received through a file request.** These open through the same shared crypto code the web app uses. You can rename them, and the new name is encrypted under a key derived for that file, never under the key the sender used. Their content is read-only. `bb sync` saves an edit as a new file, and the mount refuses to overwrite one. `bb repair` leaves them alone.
- **Mount (FUSE).** It is updated to fuser 0.18. The mount point and its files are now accessible only to your own user (directories 0700, files 0600, plus an owner-only session ACL). Renaming a file received through a file request works over the mount; writing to its content is refused.

### Bug Fixes / Hardening

- `bb sync` only moves a replaced file on the server to the trash after its new copy has been written and checked. It never trashes a file it cannot prove it is allowed to change.
- A file you edited locally that turns out to be read-only is kept and reported with guidance, instead of failing the sync pass.
- When the CLI can read a file but cannot safely change it, for example some files saved from the iOS Files app, the file is read-only, and the CLI tells you why. A rename of a file this device has no key for is refused with a reason.
- Sync state is tied to the account you are signed in with, so switching accounts no longer mixes their state.
- A downloaded chunk larger than the maximum chunk size is refused instead of being read.
- A file request link whose token is not a single path segment is refused.
- Key material the CLI copies while working is wiped after use.

### Compatibility

- **This CLI with a server that offers no direct uploads:** every upload goes through the API, exactly as in 0.14.4. Direct uploads are used only when the server lists `object-upload-v1` in `GET /api/v1/transfer/capabilities`.
- **Shared folders and file requests** use the same server endpoints the web app uses today.
- **An older CLI with this server:** unchanged.
- Dependency changes:
  - `beebeeb-core` / `beebeeb-types` are pinned to `d3b40e3`, the same code as core `main` at the time of this release.
  - fuser 0.14 → 0.18, notify 7 → 8, notify-debouncer-full 0.4 → 0.7, indicatif 0.17 → 0.18.

### Verification

- **Test suites** at the commit this release is built from, on a Linux test machine:
  - default features: 992 passed, 0 failed, 4 ignored (38 test binaries);
  - with `--features fuse`: 1,039 passed, 0 failed, 4 ignored.
- **Lint:** Clippy with warnings denied is clean, with default features and with `fuse`. rustfmt is clean.
- **Crypto review:** the handling of files received through a file request (reads, renames, the mount and `bb repair`) went through three rounds of review. The last round found nothing that could seal a name or content under the sender's key.
- **Release gates:** the full required CLI CI, five platform builds, a readback of the draft assets with checksum assertions, and an update test from 0.14.4 before publishing.

### Install / Update

```sh
bb update
bb --version   # bb 0.15.0
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.4...v0.15.0
