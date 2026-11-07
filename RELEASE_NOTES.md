# bb 0.17.0: folder pushes upload several files at once, and live transfer numbers for `bb pull` and `bb sync`

`bb push --parallel N` now also uploads the files of a folder N at once, and it asks every question before the first byte goes out. `bb pull` and `bb sync` now show what a transfer is doing while it runs, the way `bb push` did from 0.16.0: the current speed in Mbps, the time left, and, with `-v`, how many chunks are done and in flight and which storage country the file comes from or goes to (Germany or Finland). The old progress bars are gone.

### What's New

- **A folder push uploads up to N files at once.** `--parallel N` (1 to 8, default 4) used to set how many chunks of one file went out at once. It now also sets how many files of a folder upload at the same time; `--parallel 1` uploads one file at a time. At most N chunks are in flight across all the files together, and every file's buffers still come out of the one 640 MiB upload buffer from 0.16.0. Two things change in how a folder push runs:
  - **Every question comes first.** Before the first byte is sent, the push checks every file and asks everything it needs to ask (for example, a file that already exists). A long folder push no longer stops halfway to ask.
  - **One file failing does not stop the others.** If another `bb push` of the same file is running, or a file with the same name appears in the destination while the push is preparing, that file fails with its own line and the rest carry on. A rate limit (429) pauses all the files once, with one notice; a name check that hits a rate limit waits for that pause and asks again, up to 3 tries, before it fails the file.

  Per-file lines appear as files finish; `--json` lists the files in the folder's order. Ctrl-C stops every file in flight and starts no new one: the push prints one line, exits 130, and the uploads stay resumable, so running the same command continues them. A single-file push and `bb sync` behave as before.
- **`bb pull` shows live numbers.** On a terminal a single-file pull shows one line that moves with every decrypted chunk: a bar, the percent, bytes done of the total, the current speed in Mbps and the time left. A folder pull shows a line without a total or a time left, because the folder is walked while it downloads: bytes done, the number of files and the speed. Each file then ends with its two lines, and the pull ends with a summary line: how many files, the size, the average speed and the total time. `bb pull --zip` draws a live line for each of your own files and ends with the zip's name, size and time, then the number of files with the download and zip times. Files that arrived through a file request are read whole before they are written, so they are counted in the totals but not drawn as a live line.
- **`bb sync` shows live numbers both ways.** The upload phase has its live line, and the download phase that follows has its own. The end line shows the average speed in Mbps and the time as `4.2s` or `7m 21s`. `bb sync --daemon` has no terminal and ignores `-v`.
- **`-v` / `--verbose` for `bb pull` and `bb sync`.** Adds the chunks done of the total, how many are in flight (for `bb pull`, up to 4 when the file comes straight from object storage and 1 through the API), whether chunks came directly or through the API, how many were retried, and the storage country (Germany or Finland; nothing is printed when the server does not name one). It also prints one line per chunk and a detail line under the end lines (for `bb sync`, one for the uploads and one for the downloads). `-v` with `--quiet` is a usage error (exit 2), as for `bb push`; with `--json` it has no effect.
- **`--json` reports the numbers.** `bb pull --json` gains a `transfer` object; `bb sync --json` gains `transfer_upload` and `transfer_download`. It is additive: nothing that was there changes.

### Bug Fixes / Hardening

- **The live numbers of `bb sync` settle.** A file that is skipped or never started leaves the totals. A file that fails stays in them, but its unsent bytes no longer count toward the time left, so the run ends with numbers that add up. The "in flight" figure is never higher than the number of files still to send, and with `-v` it is shown from the start of the upload phase instead of `0/0` while the first file is checked.
- **A failed download takes back what it counted.** A download that fails, is interrupted, or moves from direct storage to the API no longer leaves its chunks counted. `-v` says "counted again after the download moved to the API" only when a download really did move.
- **The percentage never shows more than 100 %**, and a file that failed is no longer drawn as still active. A file-request upload in a folder pull is no longer drawn as a line that jumps from 0 to 100 %.
- **A file row with an enormous size cannot overflow the transfer plan;** the numbers stop at the largest value they can hold.
- **Smaller output fixes.** The zip and folder headers say `1 file` and `1 item`, not `1 files` and `1 items`. The `-v` storage line no longer reads `chunk 0 B` for a file that was read whole. Warnings about skipped files and files that changed while being read now use the error colour instead of amber, and they print above the live lines instead of tearing them.
- **Nothing identifying the storage is printed.** New tests check that `bb sync` (both directions) and `bb pull` of a folder, as a zip, and on the API fallback never print a city, a data center code, a pool id, a storage host, a signed link, a token, a key or a provider name. Only the country is shown.

### Compatibility

- **The output of `bb pull` and `bb sync` changed.** The old progress bars are gone. End lines show speeds in Mbps (megabits per second, to one decimal), so the same speed reads as a number about 8 times larger than it did in MB/s, and times read `4.2s` or `7m 21s`, not `4240ms`. A script that reads these lines must be changed; `--json` is the stable way to read the numbers. Live lines show only when stdout and stderr are both terminals. On a pipe there are none; with `-v` the per-chunk lines and the end lines still print.
- **A folder push now uploads 4 files at once by default,** so its per-file lines come in the order the files finish. `--json` keeps the folder's order. With `--quiet`, the ids are printed in the order files finish, not in the folder's order. `--parallel 1` uploads one file at a time.
- **Not covered yet** (unchanged from 0.16.0): `bb sync`, `bb mount`, `bb webdav`, `bb repair` and `bb restore` don't check the Backups rule. Until a later release adds it, don't use them to change files inside Backups.
- **Known limits, not fixed in this release:**
  - Folder push: `--keep-both` on files without an extension can give two files the same name (`report (1)`); a symlink next to its target can be uploaded twice; the second name check, made just before each upload, doubles the cost of listing and decrypting the destination folder, which matters on a very large one.
  - `bb sync`: if a file changes size between the scan and its upload, the live totals are off by the change.
  - `--zip` members and the backup manifest do not show a storage country.
  - With `BB_TRANSPORT_TRACE=1`, the trace lines can tear the live lines.
  - `bb sync --json`: a run that creates a folder on the server prints a `+ remote folder <name>` line on stdout before the JSON (`would create remote folder` with `--dry-run`), so a JSON parser reading stdout fails on that run. This was already so in 0.16.0.
- Dependency changes: none. The release uses Beebeeb core v0.2.1 (`a5413d2`), as 0.16.0 did.

### Verification

- **Test suite** on Linux in CI, default features, at the head of each of the three changes before it was merged: the folder push, 1,232 passed, 0 failed, 5 ignored; the pull and sync numbers, 1,252 passed, 0 failed, 5 ignored; the follow-up fixes, 1,268 passed, 0 failed, 5 ignored (46 test result lines in each run). The last run is the code of this release: its tree is identical to the commit this release is cut from. This release differs from that commit only in the version line (`Cargo.toml` and `Cargo.lock`), the release notes and the changelog. The build and release configuration (`dist-workspace.toml` and the workflows) is unchanged since 0.16.0. The tests run the real `bb` binary against a local mock of the server; none of them talks to production.
- **Lint:** the CI jobs for Clippy with warnings denied and rustfmt, for Clippy with the `fuse` feature, and for the installer update regression passed on each of the three changes.
- **Review:** each of the three changes had one independent code review before it was merged. The folder-push review found 4 problems that had to be fixed first (Ctrl-C during a name check, a rate limit during a name check, and two behaviours that were tested only with one file at a time) and 9 smaller ones; the 4 are fixed, each with a test that fails without the fix, and the 9 are open (the folder-push limits listed above come from them). The review of the pull and sync numbers found 2 problems that had to be fixed and 7 smaller ones; all nine are fixed in this release. The review of those fixes found 1 more that had to be fixed (`bb pull --zip` drew no live line for your own files) and 9 smaller ones; that one and two of the smaller ones are fixed, and the other 7 are open.
- **Flaky tests seen, not fixed:** in a full run on a build machine, `update::tests::unix::homebrew_failures_are_reported` failed with "Text file busy" in 2 of 4 runs of the unit-test binary and passed alone 5 of 5 and in CI; `src/update.rs` is unchanged since 0.16.0 and the cause is not proven. A timing-based backup test failed once in the first CI run of the follow-up fixes and passed on the re-run; it was not changed.
- **Not run for this release:**
  - The test suite with `--features fuse`. Release binaries ship without FUSE, so the mount is not part of what you install.
  - The live display on a real terminal, against the real service, with a large file. No automated run did this, and the tests did not run natively on macOS or Windows.
  - A measurement of how much faster a folder of small files uploads. None was made, and this release makes no speed claim.
  - The self-update from 0.16.0 can only be tested against the published release, so it is checked after publishing and is not part of the counts above.

### Install / Update

```sh
bb update
bb --version   # bb 0.17.0
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.16.0...v0.17.0
