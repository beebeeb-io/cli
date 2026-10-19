# Beebeeb CLI 0.14.0 — storage is chosen by country, and a push you interrupted can be resumed

`bb region` now works by country, not by city. You choose Europe (the default) or a country, and
`bb` no longer prints a city name anywhere, including in `--json` output. This release also lets you
re-run an interrupted `bb push` to finish it. Since v0.13.6 two pull requests landed on `main`:
beebeeb-io/cli#91 (re-running an interrupted push, task 1950) and #92 (`bb region` by country, task
2050). #90, the Scoop manifest bump for 0.13.6, does not touch the binary.

### What's New

- **`bb region list | get | set` work by country.** `bb region list` shows Europe (the default: no
  specific country, Beebeeb places your files in any of its European datacenters), Germany and
  Finland, with your current choice marked. Netherlands is listed as coming soon and cannot be chosen
  yet. `bb region set` takes a country code or a name, in any case: `bb region set de` and
  `bb region set germany` do the same thing, and `europe` returns to the default. The list comes from
  the server, so a country that goes live appears without a new `bb`. Your choice applies to new
  uploads only: files you already uploaded stay where they are.
- **A city is no longer a choice.** If you type one, `bb`
  refuses it without changing your setting and tells you to choose by country, with the codes from the
  list.
- **`bb billing usage` groups by country.** Usage that has no country is shown under Europe.
- **`--json` gains `country` and `country_code`.** `bb --json region get` and `set` add both keys and
  keep the existing ones (`preferred_region`, `current`, `label` / `name`). `bb --json billing usage`
  keeps `region` and `used_bytes`, and adds `country`. Its `city` key is kept for scripts that read
  it, is deprecated, and now carries the country name.
- **Re-running an interrupted `bb push` resumes it.** (task 1950) Before, a push stopped mid-upload
  left a half-uploaded file, and running the same command again refused with "already exists - pass
  --replace or --keep-both". Now, when that file is your own unfinished first upload (the local resume
  record and the server agree), the same command continues it. The chunks the server already holds are
  skipped, or the upload restarts from the first chunk if the chunk formats differ. A completed file is
  still a conflict, and so is an interrupted `--replace`: a plain push never finishes someone's replace.

### Bug Fixes / Hardening

- **No city name in any output, including `--json`.** `bb account show` and `bb region list` strip
  the server's deprecated city keys, so a server that still sends them does not leak them through `bb`.
  `bb whoami`, `bb account show` and `bb speedtest` name your choice by country (`bb speedtest` used
  to print a raw datacenter id), and `bb billing show` always reads "Europe" for the billing region. A
  test scans the production source and the README for place names, and fails if one comes back.
- **Older servers keep working.** A server that still lists one row per datacenter is folded into one
  row per country, and a stored datacenter id reads as its country. Old datacenter ids typed by hand
  or kept in scripts (the short datacenter ids) are still accepted and sent as they are; the server maps them
  and `bb` shows the resulting country.
- **Known limit, not changed here:** `bb region set` sends only the country, which the server reads
  as a non-strict choice. Setting a country from the CLI therefore clears a strict choice made in the
  web or mobile app. The CLI has no strict flag yet.

### Verification

Code under test: `400b212` (cli `main`, after #92). The release commit on top of it only changes the
version to 0.14.0, `Cargo.lock` (that one line), this file, and the changelog.

- `cargo test --no-fail-fast` on Linux (Legion), from the gate on #92: 19 test binaries, every one
  `test result: ok`, 0 failed. Main unit tests (`bb`): 511 passed, 3 ignored. `region_choice`: 18
  passed. `no_city_copy`: 3 passed. The other 16 integration binaries were unchanged and green.
- `cargo clippy --all-targets -- -D warnings` and `cargo fmt -- --check` were clean. The GitHub CI
  `Tests` and `Clippy & Format` jobs passed on #92.
- New tests were seen to fail first. Against the previous `main`, `no_city_copy` failed 2 of 3 (the
  README and two place names in `billing.rs`) and `region_choice` failed 11 of 18. Eleven mutations on
  the final code each turned the intended tests red, among them: no folding of per-datacenter rows,
  dropping the country-code match, switching the legacy-id passthrough off, billing reading `city`,
  and a city name added to the help text.
- The #91 resume path was tested with the real `bb` binary killed mid-upload and re-run: 3 new
  integration tests, plus a mutation check on the `download_version` guard.
- Not verified: the country list and `set` behaviour were exercised against a mock of the new server
  contract, not against production. The server side of the country change is not deployed yet; until
  it is, `bb` runs in its older-server mode, folding datacenter rows into countries. Windows: the
  `bb.exe` is cross-compiled and is not executed on a Windows machine by this release process.

### Install / Update

```
curl -fsSL https://get.beebeeb.io | sh           # macOS / Linux
brew upgrade beebeeb-io/tap/bb                   # macOS, Homebrew
scoop update bb                                  # Windows, Scoop
```

macOS and Linux installs (shell installer or Homebrew) self-update on next run via the built-in OTA
updater. The updater checks when `bb` starts, so a running `bb sync` daemon keeps the old version
until it restarts: restart it after updating. Windows installs do not self-update yet: run
`scoop update bb` to get 0.14.0. The Scoop manifest (`scoop/bb.json`) is bumped by a follow-up pull
request; until that pull request is merged, `scoop update bb` still installs 0.13.6.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.13.6...v0.14.0
