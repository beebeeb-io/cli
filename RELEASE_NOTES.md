# Beebeeb CLI 0.13.5 — `bb sync` no longer deletes a local file because it is missing remotely

This is a safety release for `bb sync`, with two fixes. In 0.13.4 and earlier, `bb sync` and the
background sync daemon deleted any synced local file that was missing from the remote listing, and a
download that failed to decrypt moved the remote copy to the vault Trash. A listing that came back
empty or incomplete, or a sync folder that now pointed at a different remote folder, could therefore
remove local files that nobody had deleted. 0.13.5 removes a local file only when the server reports
that exact file as trashed, and even then moves it into a recovery folder instead of deleting it. A
decrypt failure never touches the remote copy. Since v0.13.4 two pull requests landed on `main`:
beebeeb-io/cli#83 (both fixes, tasks 1915 and 1916) and #82 (the Scoop manifest bump for 0.13.4,
which does not touch the binary).

### What's New

- **`.bb-trash/` inside your sync folder.** When `bb sync` removes a local file because it was deleted
  remotely, the file is moved to `.bb-trash/<UTC time>/<path>` inside the sync folder, not deleted.
  The folder starts with a dot, so `bb sync` never scans, uploads or watches it. `bb` does not empty it:
  look there if a file disappeared, and delete the folder yourself when you no longer need it.
- **`bb sync --allow-mass-delete`.** If one pass would remove more than 50 files, or more than 10% of
  the files the folder tracks (whichever is larger: 51 of 501 trips it), it removes nothing and says
  so. Check the Trash in the web app first; if the deletions are intended, run
  `bb sync --once --allow-mass-delete` once to let them through (into `.bb-trash/`). The background
  daemon never gets this flag, and the continuous watch loop never removes over the limit.
- **`bb sync --rebind`.** If the remote folder a sync folder points at changed (the old one was trashed
  and re-created, or you now sync against a different remote path), every pass keeps local removals
  paused, and a file that exists on both sides is reported as a conflict instead of replacing or
  overwriting either copy. `bb sync --rebind` accepts the new remote folder: it drops the old folder's
  sync record, so nothing is removed on its account, and your local files then sync in as new files.
  It cannot be combined with `--daemon`.
- **Exit code 3 from `bb sync --once` in more cases.** A one-shot pass (`--once`, `--json`, `--quiet`)
  now exits 3 when the mass-delete limit held removals back, when a download could not be decrypted,
  and when `--delete` is used while the remote folder is unreconciled. Exit 3 already meant "the run
  finished but left items failed or unresolved"; scripts that treat any non-zero exit as a failure will
  now see these cases.

### Bug Fixes / Hardening

- **A file missing from the remote listing is no longer deleted locally.** (task 1915) A local file is
  removed only when all of these hold:
  - the server's own record for that file (`GET /files/{id}`) says it is trashed. A file that is gone
    without such a record (renamed or moved in the web app, permanently purged, or another account) is
    kept, and `bb sync` reports it on each pass;
  - the local file is still byte-identical to what was last synced, checked again right before the
    move, not only at the start of the pass;
  - the listing can be trusted: it is not empty while the folder tracks files, every remote name
    decrypted and was safe to use, it was not cut short by the 50,000-entry limit (a folder of exactly
    50,000 entries is no longer treated as cut short), and the remote folder is the one this folder last
    synced against;
  - the pass stays under the mass-delete limit above.
- **A download that fails to decrypt no longer moves the remote copy to the Trash.** (task 1916) In
  0.13.4 and earlier, `bb sync` treated a file it could not decrypt as a corrupt remote copy and trashed
  it. Now the remote file is left untouched, no local file or sync record is written, the error names
  the file, a one-shot pass exits 3, and the next full pass tries again.
- **`bb sync --delete` on a changed remote folder trashes nothing.** Its "synced before, missing
  locally" entries belong to the old folder, so it prints a notice naming `bb sync --rebind` and exits 3.
  After `--rebind` it behaves as before.
- **What is not changed.** `--delete` on a folder whose remote root is unchanged still trashes remote
  files that you deleted locally, as before. `--force` keeps its meaning for a conflict: the local copy
  replaces the same-named remote file, and the remote copy goes to the web Trash first; on a changed
  remote folder that applies to every same-named file, so run `bb sync --rebind` first if that is not
  what you want. Moves and renames made in the web app are not reconciled: the old local copy stays next
  to the new download. A permanently purged file (no record on the server) is kept locally, because it
  cannot be told apart from a file in another account.

### Verification

Code under test: `cab3c86` (cli `main`, after #83). The release commit on top of it only changes the
version to 0.13.5, `Cargo.lock` (that one line), this file, and the changelog.

This release is built and published from a maintainer's machine, the same way as 0.13.4, not by the
GitHub Actions release workflow. Artifacts and checksums use the same names and layout `dist`
produces, built with `dist` 0.31.0.

- `cargo test` on Linux, rustc 1.96.0: 573 passed, 0 failed, 2 ignored across 15 test binaries: main
  suite (`bb`) 472, `sync_never_deletes` 13, `account_email_change` 2, `auth_errors` 8, `ecdh_compat` 4,
  `mount_availability` 3, `move_parent` 7, `non_interactive` 6, `onboarding_state` 9, `pull_overwrite`
  5, `pull_path_traversal` 21, `share_resolves_like_pull` 1, `step_up_opaque` 4, `webdav_auth` 14,
  `zero_byte_files` 4. 0.13.4 had 532.
- `cargo clippy --all-targets -- -D warnings` clean. `cargo fmt -- --check` clean. The GitHub CI
  `Tests` job passed on the merged pull request (6m02s).
- `sync_never_deletes` runs the real `bb` binary against an in-process mock API with a scratch `HOME`.
  Every new behaviour test was seen to fail on the code before its fix, and each guard was
  mutation-checked: deliberately breaking it (ignoring the trashed flag, allowing an empty listing,
  turning the limit off, unlinking instead of moving, restoring the decrypt-failure trash, ignoring a
  changed remote folder, skipping the re-check before the move, and others) turned the intended tests
  red.
- Not verified: none of this was run against production or a real account. The 50,000-entry limit is
  tested by feeding 50,001 entries through the listing code, not against a live vault. The plain watch
  loop has one end-to-end test (a changed remote folder removes nothing); the interactive (TUI) watch
  loop has none and shares the same checks. Windows: the `bb.exe` is cross-compiled and is not executed on a Windows
  machine by this release process. The Linux binaries are cross-compiled and not executed here either.
  Please report a problem if you see one.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in OTA
updater. The updater checks when `bb` starts, so a background sync daemon that is already running keeps
running the old version (and the old deletion behaviour) until it is restarted: restart it after
updating. Windows installs do
not self-update yet: run `scoop update bb` to get 0.13.5. The Scoop manifest (`scoop/bb.json`) is
bumped by a follow-up pull request; until that pull request is merged, `scoop update bb` still installs
0.13.4.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.4...v0.13.5
