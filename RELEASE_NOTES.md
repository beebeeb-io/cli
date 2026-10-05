# Beebeeb CLI 0.13.0 — safer `bb pull` and `bb login`, and `bb` reads the server's onboarding document

Two of the changes in this release are security fixes, so upgrade.

- `bb pull` could be made to write a file anywhere you can write, by whoever uploaded it through
  one of your file-request links. That is fixed.
- `bb login` could be used to trick you into handing your session and encryption key to someone
  else's terminal. The server and web page changes for that are already live; this release
  changes what `bb` prints and sends so it fits them.

Alongside those, `bb pull` no longer fails on about 1 file in 256, `bb pull --zip` now works on
file-request uploads, `bb mv <x> /` actually moves to the root, and `bb signup` / `bb whoami`
read the server's new onboarding document. It is a minor release (0.12.0 → 0.13.0) because it
adds output and JSON keys. Six pull requests changed `bb` since v0.12.0
(beebeeb-io/cli#56 to #61), plus a Scoop manifest bump that does not touch the binary.

### What's New

- **`bb whoami` and `bb signup` read the onboarding document.** `bb` now fetches
  `GET /api/v1/onboarding` (schema major 1). `bb whoami` shows the account state label from the
  document, its explanation, and the capabilities your account is denied; `--json` gains an
  `account` key. `bb signup` still only points you at the web signup. It now adds one line
  explaining the server's answer (mode, whether it is allowed, why), and `--json` gains a
  `signup` key with the server's block verbatim. If the document says this version of `bb` is too
  old, `bb signup` tells you to update. (beebeeb-io/cli#61)
  - Plan and quota rows still come from the subscription, as before.
  - A server that has no such endpoint, or any answer that is not schema major 1, gets exactly
    the 0.12.0 behaviour. On such a server `account` and `signup` are `null` in `--json`.
  - When the document says a native signup is allowed, `bb` explains that and does not act on
    it. Accounts are still created on the web.
  - The document's capabilities now decide whether an upload is shown as blocked, in `bb whoami`
    and in the `bb push` pre-check. An account whose capabilities allow uploading no longer sees
    "upload blocked".

### Bug Fixes / Hardening

- **[Security] `bb pull` could write files outside the folder you chose.** File names come from
  the server, and for a file-request upload the name is chosen by whoever uploaded the file,
  anonymously. `bb` joined that name onto your output folder without checking it, so a name such
  as `../../.ssh/authorized_keys` or an absolute path could overwrite a file you own. Every name
  now has to be a single plain path component, the target has to resolve inside the output folder
  (a symlink already there is never followed out of it), and `bb pull <id>` without `-o` refuses
  an unsafe default name and tells you to pass `-o`. In a folder pull, an unsafe item is skipped
  with a warning on stderr and the run exits 3; the rest still downloads. `bb pull --zip` leaves
  such names out of the archive, and `bb sync` skips unsafe remote names and never downloads or
  deletes outside the sync root. A file uploaded through a request also no longer silently
  replaces a local file of the same name in a folder pull; use `--force`. (beebeeb-io/cli#57)
- **[Security] `bb login` tells you to type the code, and says who is asking.** The approval link
  used to carry the code, so someone could start a login on their own machine, send you the link,
  and a single click in your signed-in browser would approve it. The link `bb login` prints no
  longer carries the code. You type the code your own terminal shows; the approval page shows
  where the request came from and asks for your password or passkey again before it releases
  anything. `bb login` now says all of this, and its first message to the server also says what
  is asking (`bb`, its version, the operating system and the machine's hostname), which the page
  shows labelled as reported by the device. This release only changes the `bb` side; the page and
  server changes shipped earlier. One risk remains and no software removes it: if someone
  persuades you to type their code and your password, you have approved them. Approve a login
  only if you started it yourself, just now. (beebeeb-io/cli#60)
- **`bb pull` failed on about 1 file in 256.** `bb` told the two stored chunk formats apart by
  the first byte of the file, and a random first byte of `{` sent a normal file down the wrong
  path (`parse chunk 0: key must be a string`). The first byte is now only a hint. Chunk
  boundaries are found by trying the sizes the file can have and accepting only one under which
  the data authenticates, so a wrong guess fails and cannot return wrong data. Downloads try the
  streaming path first, so such files stay constant-memory. The data on the server was never
  affected; other clients could read these files. (beebeeb-io/cli#59)
- **`bb pull --zip` could not decrypt file-request uploads.** It exited 1 with `decryption
  failed: ciphertext is invalid or key is wrong`. The archive is now written by the CLI so each
  entry uses its own key. Duplicate names, and names that differ only by case, are renamed
  (`name (2).ext`). An entry that cannot be decrypted is skipped with a warning and the run exits
  3, instead of aborting the archive. The archive is written to a temporary file and renamed,
  so `--force` can no longer leave a truncated zip over a good one, and a folder cycle no longer
  overflows the stack. (beebeeb-io/cli#58)
- **`bb mv <x> /` did nothing.** The CLI left the parent out of the request for a move to the
  root, and the server reads that as "unchanged". It now sends the root explicitly, in `bb mv`
  (single and bulk), WebDAV MOVE and FUSE rename. A destination that cannot be resolved is now an
  error and sends nothing, rather than becoming a move to the root. A single move to the root
  prints `/file.txt` instead of `//file.txt`. (beebeeb-io/cli#56)

### Verification

Code under test: `f5a6cea` (cli `main`). The release commit on top of it only changes the version
to 0.13.0, `Cargo.lock` (that one line), this file, and the changelog.

This release was built and published from a maintainer's machine, not by the GitHub Actions
release workflow, whose minutes ran out. The artifacts and checksums use the same names and
layout `dist` produces, built with `dist` 0.31.0.

- `cargo test --locked` — 472 passed, 0 failed, 2 ignored across 11 test binaries:
  main suite (`bb`) 406, `auth_errors` 6, `ecdh_compat` 4, `mount_availability` 3,
  `move_parent` 7, `non_interactive` 6, `onboarding_state` 9, `pull_overwrite` 5,
  `pull_path_traversal` 21, `share_resolves_like_pull` 1, `zero_byte_files` 4. 0.12.0 had 346.
- `cargo clippy --locked --all-targets -- -D warnings` — clean. `cargo fmt -- --check` — clean.
- `dist plan --tag=v0.13.0` — five archives, the installer script, `bb.rb`, `sha256.sum`
  and `source.tar.gz`.
- Before release, the changes were run against a local server with a scratch home directory, never
  production: `bb signup --json` against the server's published onboarding fixture, and
  `bb whoami` for five account states (#61); root moves with 9 API and 9 database checks (#56).
  The security fixes were checked with failing-first tests, and each new test was seen to fail
  against the old behaviour before it was trusted.
- Not verified: `bb pull --zip` on file-request uploads was tested against a mock API only, not
  a live server. This release has not been run against production.

- Builds, one per target, with `dist build --artifacts=local`: the two macOS targets on an Apple
  Silicon Mac (the x86_64 one cross-compiled), the two Linux musl targets cross-compiled with
  Zig, Windows cross-compiled with clang-cl and the Windows SDK. This differs from 0.12.0, whose
  Linux builds used a native musl toolchain on Linux runners, so expect different file sizes.
- What was executed: `bb --version` printed `bb 0.13.0` for macOS arm64, macOS x86_64 (under
  Rosetta), Linux x86_64 and Linux aarch64 (both in containers). The Windows `bb.exe` was **not
  executed**: there was no Windows machine available. It is a PE32+ x86-64 executable whose
  imports are Windows system DLLs only. Please report a Windows problem if you see one.
- Two build differences from 0.12.0 that you may care about: the macOS binaries link `liblzma`
  statically (0.12.0's linked a Homebrew copy that is not present on every Mac), and the Windows
  build compiled the bundled WebP library without its SSE4.1 code paths, which is what the macOS
  and Linux builds do too. No source file in this repository changed for either.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in
OTA updater. Windows installs do not self-update yet — run `scoop update bb` to get 0.13.0.
The Scoop manifest (`scoop/bb.json`) is bumped by a follow-up pull request. Until that pull
request is merged, `scoop update bb` still installs 0.12.0. To get 0.13.0 on Windows before then,
download it from the assets below.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.12.0...v0.13.0
