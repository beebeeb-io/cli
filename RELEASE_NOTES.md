# Beebeeb CLI 0.11.0 — `bb share` works again, and scripts stop getting lied to

The headline fix in this release is `bb share`: it could not create a single share on v0.10.0
because it sent the wrapped file key in a format the server no longer accepted. It now mints
shares in the same end-to-end-encrypted wire format the web app uses, so a link made by `bb`
opens correctly in the web viewer. Alongside that P0, every command that used to read EOF from a
non-interactive stdin and quietly do nothing (or silently overwrite a file) now fails loudly with
a distinct, documented exit code — and a sweep of billing, auth-error, help-text, README, and
FUSE-mount correctness issues found along the way is included. 48 commits since v0.10.0
(2026-09-24).

### What's New

- **[P0] `bb share` works again — every share is end-to-end encrypted, in the web's wire
  format.** It previously sent `wrapped_file_key` as a JSON blob under an HKDF-derived key; the
  server (since a prior change) requires standard base64 of a raw AEAD blob, and the web viewer
  unwraps `nonce(12) || ciphertext` under the raw client key `K_c` — so every `bb share` call
  failed outright, and `--no-double-encrypt` was rejected by the server. `bb share` now wraps the
  file key exactly like the web app (`encrypt_chunk_raw` under raw `K_c`), mints the share token
  client-side, and sends `owner_wrapped_key`/`owner_wrapped_token` so `bb shares` can rebuild the
  link later; `--no-double-encrypt` is hidden and returns a clear error instead of a server
  rejection. Pinned by a known-answer test against an independent AES-GCM reference. (`75918a1`)
- **Non-interactive runs fail loudly with dedicated exit codes, instead of silently doing
  nothing.** With stdin not a terminal (cron, CI, a pipe), several commands used to read EOF from
  a confirmation prompt, print a success-looking line, and exit `0` having done nothing. Exit
  codes are now `0` success · `1` any other error (unchanged) · `2` a prompt was needed but stdin
  isn't a terminal · `3` a one-shot run finished with failed or unresolved items:
  - `bb push <name>` on an existing name: exit 2, "already exists — pass --replace or
    --keep-both" (previously printed "skip" and exited 0).
  - `bb rm <target>` without `-f`: exit 2, "refusing to trash without -f in non-interactive
    mode", nothing trashed (previously printed "cancelled" and exited 0). `-f`/`--json`/`--quiet`
    are unchanged.
  - `bb unshare` with no id: exit 2, pointed at `bb shares` (previously crashed with a raw
    "failed to enable raw mode" terminal error).
  - `bb sync --once` with a failed upload or an unresolved conflict: summary reads
    `! incomplete · N failed · M ⚡` and exits 3 with the counts (`--json` carries `ok`/`failed`);
    previously printed `✓ synced` and exited 0. Dry runs and the continuous watch loop are
    unaffected. (`aec35c7`)
- **`bb pull` never silently overwrites a local file again.** `bb pull note.txt` over a local
  `note.txt` used to print `✓ note.txt`, exit 0, and replace the file's content with no prompt, no
  flag, and no backup. The output path is now checked before any bytes download, for both
  single-file and `--zip` output: `-f`/`--force` overwrites (unchanged atomic
  `.tmp` + rename); a rich terminal with a real tty on stdin gets a `y/N` prompt (default no); every
  non-interactive run (`--json`, `--quiet`, piped stdin) refuses with a non-zero exit naming
  `--force` and `-o <path>`; a directory at the output path is always an error. Recursive folder
  pulls into an existing directory are not guarded yet. (`de3c8c4`)
- **`bb share` accepts vault paths and short IDs**, not just full UUIDs — the same resolver
  `bb pull` uses. Previously `bb share note.txt` or `bb share 4c53f27f` failed with "invalid file
  id (expected UUID)", even though `bb ls` only ever prints the 8-character short ID. (`024dc3d`)

### Bug Fixes / Hardening

- `bb push`/`bb sync` on a folder no longer abort the whole folder when one file fails (e.g. a
  zero-byte file the server used to reject): the rest of the folder still uploads, each failure is
  printed with its relative path, the summary shows `N failed`, `--json` gains
  `total_failed`/`failed[{path,error}]`, and the command exits 1 naming which files failed.
  Sub-folder-creation errors still abort, since children would otherwise land in the wrong folder.
  (`65372dc`)
- Auth, session, and connection errors read like English instead of raw codes or reqwest's error
  chain: a generic 401 now says "Your session expired or was revoked. Run `bb login` to sign in
  again."; a connect failure says "Can't reach `<api origin>` — check your connection or `--api`".
  `bb whoami`/`bb status` exit 1 when logged out or when the server rejects the session, instead of
  printing placeholder plan data ("user unknown, plan Free — 5 GB") for a session that no longer
  works. (`e4ba5de`)
- `bb share --passphrase` no longer claims to wrap chunk keys with Argon2id — it doesn't; the
  passphrase is sent to the server as a checked gate on access, and the file's encryption never
  depends on it. Output and `--help` text corrected. (`8d7852d`, `36a36f1`)
- `bb mount` on release binaries (which ship without the `fuse` feature — no FUSE asset has ever
  been published) now says plainly that this build can't mount, points at `bb webdav` (ships in
  every release) and the source-build route, and exits 1 — instead of walking users through
  installing macFUSE/libfuse3 for a feature this binary can never use. `mount`/`unmount` are
  hidden from `--help` on these builds; source builds with `--features fuse` are unchanged.
  (`f884b4b`)
- `bb --help`'s footer now links pages that actually resolve (`github.com/beebeeb-io/cli`,
  `beebeeb.io/security`) instead of two 404s (`beebeeb.io/cli`, `beebeeb.io/fingerprints` — the
  CLI has no fingerprint feature); the README no longer claims `bb --help` shows every flag and
  points at `bb <command> --help` instead. (`d965504`)
- `bb --help` lists every top-level command again — `2fa`, `sessions`, `passkey`, `billing`,
  `account`, `request`, and `unmount` had silently dropped out of the hand-written help screen; the
  "+ ..." overflow line is now computed from the actual clap command tree so this can't recur
  silently. (`aa62be4`)
- Billing sweep (task 1547, four confirmed findings): `bb billing show` shows a distinct
  `Trial: ends <date> · no card on file` line instead of the same price/renewal pair as an
  actively-billed account; `bb billing portal` uses the Mollie-era payment-method-update route
  instead of a Stripe-only alias that 400s "stripe not configured" on every current account; API
  error responses now surface the server's human `message` field instead of only the bare error
  code (~20 error types were previously invisible to CLI users); `bb whoami`/`bb quota`/`bb push`
  read quota straight from the server's `quota_bytes` instead of reconstructing a "bonus" from a
  `bonus_bytes` field the API never actually returns. (`31bb2dc`, `07a5b5b`)
- README links the stable `beebeeb.io/download/cli` redirect alongside the GitHub releases page.
  (`22aac96`)
- Release pipeline hardening: the `RELEASE_NOTES.md` gate now runs as the first step of the tag
  workflow, before anything is built or the GitHub Release is created — a missing or stale file
  can no longer ship a release with cargo-dist's auto-generated changelog body. The Scoop manifest
  bump now opens a PR against `main` instead of pushing directly to it, which used to fail closed
  on the protected branch and could take the whole publish job down with it. (`91619cf`,
  `23f32ab`)

### Verification

- `cargo build --release` — succeeds; `./target/release/bb --version` prints `bb 0.11.0`.
- `cargo test --all-targets` — 8 binaries, all green: `test result: ok. 250 passed; 0 failed;
  2 ignored` (main lib+bin suite) plus `6 passed` (`auth_errors`), `4 passed` (`ecdh_compat`),
  `3 passed` (`mount_availability`), `6 passed` (`non_interactive`), `5 passed`
  (`pull_overwrite`), `1 passed` (`share_resolves_like_pull`), `4 passed` (`zero_byte_files`) —
  279 passed, 0 failed total.
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo fmt -- --check` — clean.
- `dist plan` (cargo-dist 0.31.0, matching the pinned CI version) — plans `v0.11.0` across all 5
  targets (macOS aarch64/x86_64, Linux musl aarch64/x86_64, Windows msvc) plus the shell
  installer and Homebrew formula.
- The release artifacts are built by the tag-triggered cargo-dist workflow; the installer and
  Homebrew formula are published from those builds.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh    # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) also self-update on next run via the
built-in OTA updater. Windows installs do not self-update yet — run `scoop update bb` to get
0.11.0.

**Scripts and CI pipelines that call `bb` non-interactively should check the new exit codes** (`2`
for "needed a prompt", `3` for "finished incomplete") if they only handled `0`/`1` before —
see "What's New" above.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.10.0...v0.11.0
