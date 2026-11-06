# bb 0.16.0: live transfer numbers for `bb push`, and folder pushes you can safely repeat

`bb push` now shows what a transfer is doing while it runs: the current speed in Mbps, the time left, and, with `-v`, how many chunks are done and in flight and which storage country the file goes to (Germany or Finland). Pushing a folder is also safe to repeat. A second push of the same folder reuses the folder that is already there instead of making another one with the same name, skips the files this machine already uploaded unchanged, and an interrupted upload finishes in the folder you asked for, not in the one an earlier run was using.

### What's New

- **Live numbers while `bb push` runs.** On a terminal you get one line that moves each time the server confirms a chunk: a bar, bytes done of the total, the current speed in Mbps, and the time left. The current speed is measured over a window of at least 10 seconds, and the time left counts down between chunks. Until the first chunk is confirmed, speed and time left show a dash. Speeds are decimal megabits per second, to one decimal. `bb speedtest` now prints its speeds the same way. The average and the total time in the summary leave out the time you spent answering a conflict prompt, because that wait is not transfer time; they still include the gaps between files (the name check, starting the upload, finishing it).
- **`-v` / `--verbose` for `bb push`.** Adds the chunks done of the total, how many are in flight, the average speed, the storage country the file goes to (Germany or Finland; nothing is printed when the server does not name one), how many chunks went directly to object storage and how many through the API, and how many were retried. It also prints one line per chunk and a detail line under the summary. `-v` belongs to `bb push` only for now. With `--quiet` it is a usage error (exit 2); with `--json` it has no effect.
- **Clearer end lines.** The finished line shows a duration such as `7m 21s` instead of raw milliseconds, says `1 file` instead of `1 files`, and no longer runs the file name into its size. Long names are shortened in the middle to fit the terminal width. On a pipe or with stderr redirected there are no live lines; with `-v` the per-chunk lines and the end lines still print.
- **`bb push --json` reports a `transfer` object** with the run's totals (bytes, chunks, average speed in `avg_mbps`, `duration_ms`, the direct and API chunk counts, retries) and a `pools` list that names the storage country only: `country`, `country_code` and `chunks`, never a pool id. It is additive: nothing that was there changes, and the existing top-level `total_duration_ms` is still the wall time of the whole run.
- **Pushing a folder again reuses its folder.** `bb push <folder>` and `bb push --folder <name>` look for a folder of the same name at every level and reuse it, instead of creating a second one. A name matches only when it decrypts to exactly the same bytes. If several folders have the same name, the oldest folder that is not shared wins, and a line says how many there are.
- **A folder that is shared is not reused silently.** If the folder you are pushing into already exists and is shared with people or through links, `bb push` asks first at a terminal (the default is No, and the question says how many people and links). With no terminal, `--json` or `--quiet` it refuses with exit 2. The new `--reuse-shared` flag uploads into it without asking. Answering No makes a new folder, and later runs keep using it. With `--folder`, a shared folder is used only when every folder of that name is shared, and the push says so.
- **A re-push skips what was already uploaded.** A file is skipped, and counted as `skipped N already uploaded` (`total_already_uploaded` in `--json`), only when this machine's own record shows it uploaded that file into this folder (same path, size and modification time) and the server still holds it there, finished at that version and at that size. Anything else is treated as before: a changed file, a newer version, a file another machine pushed or a file that was moved or trashed. This applies with `--keep-both` too, so a repeated run does not make a third copy. `--replace` always uploads.

### Bug Fixes / Hardening

- **A resumed upload finishes in the folder you asked for.** Before, a push that was interrupted and run again with a different `--folder` could complete the file in the folder the first run had used, and print nothing about it. Now an interrupted upload continues only when it sits in the folder this push writes into. Otherwise the old upload is ended on the server, which removes its never-completed entry, and the file starts again in the right folder. The file is never moved.
- **No guessing when the CLI cannot tell who can see a folder.** If the server does not say, or the sharing lists do not load, the folder counts as shared: you are asked, or the push refuses without a terminal, unless you passed `--reuse-shared`. When several folders have the same name and the CLI cannot tell which one is private, nothing is uploaded, also with `--reuse-shared`.
- **No duplicate when a name check cannot be read.** If the folder listing for the name check fails, or a same-name entry might be a file-request upload whose keys did not load, that file fails with a reason instead of being uploaded a second time. An entry this device can never open is passed over, so it does not block every push into that folder.
- **The log of finished uploads is owner-only.** `finished-uploads.jsonl`, which names the local paths of files you pushed, is written with mode 0600, and an existing log is brought to 0600 too.
- **A file that changes size after the push planned it** is counted at its new size, and a file that fails stops counting toward the time left.
- **The rate-limit pause message** is printed above the live lines instead of tearing them.

### Compatibility

- **Behaviour you may notice:**
  - **A same-name shared folder now stops a script.** Pushing a folder where a folder of that name already exists and is shared (or the CLI cannot tell who can see it) asks at a terminal. With no terminal, `--json` or `--quiet` it uploads nothing and exits with code 2. In 0.15.0 the same push made a new private folder and exited 0. A script that wants the shared folder passes `--reuse-shared`; one that wants a private folder pushes under a different name.
  - **An unchanged re-push now exits 0.** Running a completed, unchanged `bb push` again used to refuse with "already exists" and exit 2 without a terminal. It now skips the file, prints `skipped N already uploaded` (`total_already_uploaded` in `--json`) and exits 0. A changed file, or one this machine did not upload, is still a conflict and still exits 2. `--replace` always uploads.
- **Interrupted uploads recorded by an older `bb`:** the old record does not name its folder. If the server can tell where the upload sits, that decides. If it cannot, that file fails once for that run and its record is kept.
- **Sharing lists:** the folder check reads who a folder is shared with and its links from the same sharing endpoints the web app uses. A server that does not offer them is treated as "cannot tell", never as "private".
- **An older CLI with this server:** unchanged.
- Dependency changes:
  - `libc` 0.2.186 → 0.2.191, `serde_json` 1.0.149 → 1.0.154, `regex` 1.12.3 → 1.13.1 (with `regex-automata` and `regex-syntax`).
  - `unicode-width` 0.2 is now a direct dependency, used to fit names by display width; it was already in the build.
  - `beebeeb-core` / `beebeeb-types` stay pinned to `d3b40e3`, as in 0.15.0.

### Verification

- **Test suite** at the commit this release is built from, on a Linux test machine, default features: 1,120 passed, 0 failed, 4 ignored (41 test binaries). The tests run the real `bb` binary against a local mock of the server, with a pseudo-terminal where a prompt is involved; none of them talks to production.
- **Lint:** Clippy with warnings denied is clean, and rustfmt is clean.
- **Review:** the folder-push changes went through several rounds of code review before they were merged. A security-focused review of the first version found three blocking problems (`--folder` reusing a shared folder when a private one existed, a file-request list that did not load, and an unknown sharing state treated as a guess); all three are fixed in this release, and each fix has a test that fails without it.
- **Not run for this release:** the suite with `--features fuse`. Release binaries ship without FUSE, so the mount is not part of what you install.

### Install / Update

```sh
bb update
bb --version   # bb 0.16.0
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.15.0...v0.16.0
