# beebeeb-io/cli

`bb`: the Beebeeb CLI for end-to-end encrypted cloud storage (PUBLIC repo). Rust, clap derive; `src/main.rs` is the command source of truth.
Moved detail (per-command behaviour, upload/download pipelines, safe_path rules, direct S3 transfer, core pin, config, colours): `docs/agent-notes.md`.
Pre-distillation CLAUDE.md verbatim and per-task narratives: `docs/runbook/cli-task-history.md`.

## Commands

```sh
cargo build                            # target/debug/bb
cargo build --release --features fuse  # FUSE needs macFUSE / libfuse3; release binaries ship WITHOUT FUSE
cargo test 2>&1 | tee /tmp/bb-cli-test.log
cargo clippy -- -D warnings; echo exit=$?
cargo fmt -- --check; echo exit=$?
make check-onboarding-contract         # from the workspace root; drift guard: contracts/onboarding/ vs repos/server/contracts/onboarding/
```

Truth lines: per test binary `test result: ok. N passed; 0 failed`; clippy and fmt assert the exit code. A CLI task is verified by running
the command and pasting its output, not by compiling. The CLI gate needs Linux (Legion; workspace `docs/runbook/legion-lane-host.md`).

## Module map

```
src/main.rs               clap commands; commands/*.rs = one file per command (push, pull, sync, share, request, region, twofa, ...)
src/api.rs, api/          ApiClient: every endpoint, provenance headers (X-Beebeeb-Client: cli + version), 300 s timeout
src/upload.rs             shared streaming driver stream_encrypt_upload (push, sync, webdav, mount, repair) on the v2 /uploads/* route
src/download.rs           stream_download_decrypt (pull, sync); direct_download.rs / direct_upload.rs = S3 grant transfer; transport.rs
src/resume.rs, item_id.rs resume records; L13 own-ids rule (client proposes, server must echo)
src/safe_path.rs          the ONLY way a server-provided name becomes a local path
src/sync_safety.rs, daemon.rs   deletion safety (.bb-trash, mass-delete breaker), service manager guard
src/account_state.rs      OnboardingDoc (lenient, schema major 1), account-state labels; region.rs, web_url.rs, device.rs
src/config.rs, custody.rs, owned_keys.rs   config.json (session + master key!), key custody
src/exit.rs               exit codes: 0 ok, 1 error, 2 prompt needed but not a TTY, 3 finished with failed/unresolved items
src/colors.rs, ui.rs, tui/  amber #f5b800, sage #8fc18b, coral #e07a6a: always colors::*, never raw .truecolor()
src/test_server.rs        shared loopback mock for tests; tests/ = integration binaries (api_guard, sync_never_deletes, pull_*, ...)
```

## Traps

- **A test run must never reach production.** The CLI reads no `BB_API_BASE`: with no `--api` and no config it talks to `https://api.beebeeb.io`.
  Set `BB_REFUSE_NON_LOOPBACK_API=1` on EVERY run of the binary and pass an explicit local `--api`; `tests/api_guard.rs` fails an unguarded
  `CARGO_BIN_EXE_bb` spawn. Unit tests use `ApiClient::new_for_test` (refuses non-loopback). The config file holds the live session and master
  key (macOS `~/Library/Application Support/beebeeb/config.json`, Linux `~/.config/beebeeb/config.json`): tests isolate HOME.
- **Never silently overwrite** on `bb pull`: `-f/--force`, a TTY `y/N`, or exit non-zero naming `--force`/`-o` (`tests/pull_overwrite.rs`).
- **Server-provided names** (file requests: an anonymous stranger) become local paths only via `safe_path`; never `out_dir.join(name)`.
- **Sync never deletes on absence.** Local removal only for a confirmed server tombstone, byte-identical to the manifest, a trustworthy
  listing, under the mass-delete breaker (exit 3); removal is a MOVE into `.bb-trash/<UTC stamp>/<rel>`, never an unlink. An undecryptable download writes nothing, exit 3.
- **Own ids (L13):** every create proposes its own id and fails the op if any answer does not echo it byte for byte; never seal under the echoed id.
- **The CLI never calls the legacy `/api/v1/files/upload`;** all uploads use `/api/v1/uploads/*`. Init sends the plaintext `file_size_bytes`.
- **No city is ever printed** (`bb region`, any command, whichever server answers; `tests/no_city_copy.rs`, `tests/region_choice.rs`). The
  region choices come from `GET /api/v1/storage-regions`, never from the binary.
- **Step-up (`bb trash empty`, `rm --permanent`, `account update`):** OPAQUE confirm; fall back to plaintext `/auth/confirm` ONLY on
  `409 {"opaque_unavailable": true}`; passwords and OPAQUE state are `Zeroizing`.
- **`bb share` is always double-encrypted** (web wire format, `nonce(12) || AES-256-GCM(K_c, FileKey)`, pinned by the core `share_key_wrap` KAT);
  `bb signup` never creates accounts (it prints the web link); `bb 2fa status` reads `totp_enabled`.
- **Direct S3 transfer:** storage requests use their own clients (no default headers, cookies, `Authorization`, redirects; https only);
  `--transport api` / `BB_TRANSPORT=api` force the API; never log a URL. No test may call `bb sync --daemon`; no `launchctl`/`systemctl`
  call unless `$HOME` is the account's home.
- **Core pin:** `beebeeb-core` + `beebeeb-types` are Cargo git deps on a full 40-char SHA; bump `rev` ONLY together with `repos/server/CORE_REV`
  in the SAME commit, then fix what the compiler flags (non-exhaustive `match` on core enums is intentional). Local `[patch]` snippet: `docs/agent-notes.md` → "Dependencies".

## How we work

Workspace `CLAUDE.md` → "How we work" is the rule set (evidence, design before code, done, parallel agents). Here: the truth lines above;
terminal mockups are `../../design/hifi/hifi-cli.jsx` (design wins over code).

## Graphify and shared docs

Knowledge graph at `graphify-out/`. Architecture question: `graphify query "<question>"` / `graphify explain <symbol>` /
`graphify path "<A>" "<B>"`; overview: `graphify-out/GRAPH_SUMMARY.md` (at most 3k tokens); never read `GRAPH_REPORT.md` whole.
Run `graphify update .` for your own navigation; never commit `graphify-out/` on a feature branch (the lead regenerates it on main).
When you add/change/remove endpoints, types, build commands or dependencies, update the matching skill in the workspace
`.claude/skills/` (beebeeb-api.md, beebeeb-designs.md, beebeeb-stack.md, beebeeb-dev.md).
