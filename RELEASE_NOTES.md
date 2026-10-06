# Beebeeb CLI 0.13.1 — the read-only notice stops claiming you can delete

A small follow-up to 0.13.0. When your account is read-only or your trial has ended, `bb` used to
tell you that you can "download and delete". The server's onboarding document now says per
account whether deleting is allowed, and `bb` follows it. Everything else in this release is
vendored contract data that no `bb` code reads. Four pull requests landed on `main` since v0.13.0
(beebeeb-io/cli#64 to #67), plus the Scoop manifest bump for 0.13.0 (#63), which does not touch
the binary. This is a patch release: no new commands, flags or JSON keys.

### What's New

- Nothing user-visible beyond the fix below.

### Bug Fixes / Hardening

- **The account notice no longer says "you can delete" when the server says you cannot.** The
  account summary that `bb whoami` and the `bb push` upload pre-check print for a read-only or
  ended-trial account used to say "your vault is read-only: you can download and delete, not
  upload or share", whatever the account's capabilities were. It now reads
  `capabilities.delete` from the onboarding document. If deletion is denied, the notice says
  "you can download, not upload, share or delete" and adds "Deleting files is not available right
  now", with the server's reason when it gives one (for example "billing read only"). If the
  document does not mention `delete` at all, `bb` treats it as denied, because a capability the
  server does not grant is not a capability you have, and it does not invent a reason. Where the
  document allows deletion the wording is unchanged. (beebeeb-io/cli#66)
- Vendored onboarding contract updates, byte-identical copies of the server's
  `contracts/onboarding`: `delete` present in every account state (#66), `accept_terms` required
  only at `pre_account` (#65), a coupon fixture (#64), and `purchase.checkout.return_kind` (#67).
  These are schema and fixture files. `bb` reads none of those fields, so they change nothing at
  run time. The fixture-count test no longer pins a number that went stale when a fixture was
  added; it now checks that every fixture file on disk is loaded.

### Verification

Code under test: `024d593` (cli `main`). The release commit on top of it only changes the version
to 0.13.1, `Cargo.lock` (that one line), this file, and the changelog.

This release was built and published from a maintainer's machine, not by the GitHub Actions
release workflow, whose minutes ran out. The artifacts and checksums use the same names and layout
`dist` produces, built with `dist` 0.31.0, the same way as 0.13.0.

- `cargo test --locked` — 474 passed, 0 failed, 2 ignored across 11 test binaries: main suite
  (`bb`) 408, `auth_errors` 6, `ecdh_compat` 4, `mount_availability` 3, `move_parent` 7,
  `non_interactive` 6, `onboarding_state` 9, `pull_overwrite` 5, `pull_path_traversal` 21,
  `share_resolves_like_pull` 1, `zero_byte_files` 4. 0.13.0 had 472.
- `cargo clippy --locked --all-targets -- -D warnings` — clean. `cargo fmt -- --check` — clean.
- The behaviour change is covered by two new tests in `src/account_state.rs`: one that a denied
  `delete` changes the read-only notice, and one that an absent `delete` fails closed with no
  invented reason.
- Not verified: this release has not been run against a live server or production. The fix was
  exercised against the vendored onboarding fixtures only.
- The Windows `bb.exe` is cross-compiled and is **not executed** on a Windows machine by this
  release process. Please report a Windows problem if you see one.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in
OTA updater. Windows installs do not self-update yet — run `scoop update bb` to get 0.13.1.
The Scoop manifest (`scoop/bb.json`) is bumped by a follow-up pull request. Until that pull
request is merged, `scoop update bb` still installs 0.13.0.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.0...v0.13.1
