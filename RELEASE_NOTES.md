# Beebeeb CLI 0.13.6 — choose where your files are stored, and a stricter download path

This release adds `bb region`, so you can choose where new uploads are stored: Europe (the default,
any of our European datacenters) or one specific datacenter. It also moves every download onto
beebeeb-core's chunk-format-aware reader, which checks each file's format against its encrypted
metadata before it decrypts anything, and fixes `bb push --replace`, which failed the second time
you replaced the same file. Since v0.13.5 three pull requests landed on `main`: beebeeb-io/cli#84
(`bb region`, task 1919), #87 (the new read path, task 1943) and #88 (the `--replace` fix, task
1951). #86, the Scoop manifest bump for 0.13.5, does not touch the binary.

### What's New

- **`bb region list | get | set`.** `bb region` (or `bb region list`) lists where new uploads can
  be stored, with your current choice marked. Today that is Europe (the default: no specific
  country, Beebeeb places the file in any of its European datacenters) or one datacenter:
  Falkenstein (Germany), Helsinki (Finland) or Nuremberg (Germany). Ede (Netherlands) is listed as
  coming soon and cannot be chosen yet. `bb region set helsinki` (a datacenter id or city, or
  `europe`) changes the choice; `bb region get` shows it. The list comes from the server, so a
  datacenter that goes live appears without a new `bb`. Your choice applies to new uploads only:
  files you already uploaded stay where they are.
- **`bb whoami` and `bb account show` name your storage choice** (for example "Helsinki, Finland",
  or "Europe · default, any datacenter"). `bb whoami --json` has a new `region_id` key (`null` for
  the default), and `region` is now the name of your choice. `bb account show` no longer prints a
  provider name, and `bb billing show` no longer names one city for the billing region.

### Bug Fixes / Hardening

- **A second `bb push --replace` of the same file no longer fails.** (task 1951) In every release
  since 0.10.0 (the upload change of 2026-06-08), replacing a file that had already been replaced once failed with "stale base version
  for replacement upload": `bb` read the file's version from a field the server does not send, so
  it always claimed version 1. Every replace now sends the file's current version and creates the
  next one. The same fix applies to `bb webdav` saving over an existing file (a PUT with
  `--read-write`).
- **Every download now goes through core's format-aware reader.** (task 1943, beebeeb-core
  `337436a`, task 1908) `bb pull` (single file, folder, `--zip`), `bb sync`, `bb webdav`,
  `bb mount` and `bb repair` first read the file's chunk-format pin from its encrypted, authenticated
  metadata (never from a field the server could change) and open every chunk under that pin. A file
  in the newer chunk format opens only as that format, and a truncated one is rejected. This is the
  read side of a format change that no client writes yet: uploads from 0.13.6 still use the current
  format and are readable by 0.13.5 and earlier.
- **Interrupted uploads resume only when the format allows it.** A resumed upload continues only
  when both the interrupted session and this `bb` use the current format. Otherwise the partial
  upload is abandoned on the server and the file is uploaded again from the first chunk. A resume
  record written by 0.13.5 or earlier has no recorded format, so the first resume after updating
  starts that file again from the beginning, with the notice "the interrupted upload cannot be
  continued in this chunk format; starting it again". As before, you resume an interrupted `bb push` with
  `bb push --replace`.
- **Behaviour change: a file whose stored name is not encrypted is refused.** Without encrypted
  metadata there is no format pin to check, so `bb` no longer guesses. `bb pull` of such a file fails
  with the reason; a folder pull or `--zip` skips it with a warning and exits 3; `bb sync` reports it,
  leaves the remote copy alone and exits 3, and tries again on the next pass. We know of no such
  files in real accounts (Beebeeb clients encrypt file names); if you see this error, please report
  it. Folders with plain names are not affected.

### Verification

Code under test: `f7cfd95` (cli `main`, after #88). The release commit on top of it only changes the
version to 0.13.6, `Cargo.lock` (that one line), this file, and the changelog.

This release is built and published from a maintainer's machine, the same way as 0.13.5, not by the
GitHub Actions release workflow. Artifacts and checksums use the same names and layout `dist`
produces, built with `dist` 0.31.0.

- `cargo test` on macOS (Apple Silicon, rustc 1.96.0) on the release commit, scratch `HOME`:
  619 passed, 0 failed, 3 ignored across 17 test binaries: main suite (`bb`) 501 (3 ignored),
  `account_email_change` 2, `auth_errors` 8, `ecdh_compat` 4, `mount_availability` 3, `move_parent` 7,
  `non_interactive` 6, `onboarding_state` 9, `pull_overwrite` 5, `pull_path_traversal` 22,
  `push_replace_versions` 1, `region_choice` 14, `share_resolves_like_pull` 1, `step_up_opaque` 4,
  `sync_never_deletes` 14, `webdav_auth` 14, `zero_byte_files` 4.
- `cargo test` on Linux, rustc 1.96.0, from the gated pull requests: 617 passed, 0 failed, 3 ignored
  over 16 test binaries for #87, and 619 passed, 0 failed, 3 ignored over 17 binaries for #88 (the new
  `push_replace_versions` binary). 0.13.5 had 573. `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt -- --check` were clean on both, and the GitHub CI `Tests` job passed on #84, #87 and #88.
- New tests were seen to fail first. The #87 format tests were red against the previous core, and
  each guard was mutation-checked: ignoring the metadata pin, not enforcing the newer format, skipping
  the truncation check (buffered and streaming), names not going through core, the wrong writer pin,
  resuming unconditionally, not recording the format, not skipping a pin-less file in a folder pull,
  and syncing without the pin check each turned the intended tests red. The #88 test was red on the
  previous `main` ("replace #2 ... stale base version") and under a mutation that reads the old field.
- **Compatibility against a real server (not mocks).** The server's `main` ran locally with a scratch
  database and blob pool. Files uploaded with the released `bb` 0.13.5 (a 1 KiB file, a 20 MiB
  five-chunk file, a folder with a subfolder, a replaced file) were read back with the #87 build by
  `bb pull`, `bb pull --zip` and `bb sync --once`, and two 400 MiB uploads (one interrupted under
  0.13.5, one under the #87 build, both resumed by the #87 build) were read back by both builds:
  15 of 15 sha256 matches, 0 mismatches. A resume with a 0.13.5 resume record
  restarted from chunk 0 and completed; a resume with a new record continued the same session. For
  #88, a 6 MiB file pushed and then replaced three times ended at version 4 and pulled back
  byte-identical, while the previous build failed every replace against that file. `bb region`
  (#84) was run against a local server with the region API: Helsinki, Nuremberg and Europe uploads
  landed in the matching storage pools.
- Not verified: none of this was run against production or a real account. Those end-to-end runs
  were made on Linux (#87, #88) and macOS (#84), not on every platform. `bb webdav` content reads,
  `bb mount` content reads, the interactive (TUI) sync watch loop and `bb repair`'s refusal of a
  newer-format file have no dedicated tests yet. `bb push` does not print which datacenter a file
  landed in. Windows: the `bb.exe` is cross-compiled and is not executed on a Windows machine by
  this release process. The Linux binaries are cross-compiled and not executed here either. Please
  report a problem if you see one.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in OTA
updater. The updater checks when `bb` starts, so a running `bb sync` daemon keeps the old version
until it restarts: restart it after updating. Windows installs do not self-update yet: run
`scoop update bb` to get 0.13.6. The Scoop manifest (`scoop/bb.json`) is bumped by a follow-up pull
request; until that pull request is merged, `scoop update bb` still installs 0.13.5.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.5...v0.13.6
