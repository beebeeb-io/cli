# Recipes

Three changes that most tasks in this repo are a variant of. Each names the files it touches, the
items to copy from, and the command that says it compiles. `make recipes-check` validates this file
against the tree.

## Add a subcommand

`bb <name>` is one clap variant, one dispatch arm, one module.

- Files: `src/main.rs`, `src/commands/mod.rs`, `src/cli_help.rs`, `src/commands/status.rs`
- Pattern: `Commands` in `src/main.rs`, `HELP_MAIN_COMMANDS` in `src/cli_help.rs`, `run` in `src/commands/status.rs`
- Steps:
  1. Add the variant (and its args struct) to `Commands` in `src/main.rs`; the derive reads the doc
     comment as the `--help` line, so word it for a user.
  2. Add `pub mod <name>;` to `src/commands/mod.rs` and a file with
     `pub async fn run(...) -> Result<(), String>`; return the message, do not exit.
  3. Add the dispatch arm to the `match` in `main()`; a subcommand that must not check for updates
     goes in the `Commands::Update` guard above it.
  4. Add the curated row to `HELP_MAIN_COMMANDS` (or the account table) in `src/cli_help.rs`, and
     open the module with a `//!` header.
  5. `make map` in the workspace root regenerates the maps, and `make map-check` proves them current.
- Check: `cargo check --all-targets`

## Add an API client method

Every endpoint is a method on `ApiClient`; the command never builds a request itself.

- Files: `src/api/client.rs`, `src/api.rs`, `src/commands/quota.rs`
- Pattern: `ApiClient` in `src/api/client.rs`, `ApiError` in `src/api.rs`, `run` in `src/commands/quota.rs`
- Steps:
  1. Write `pub async fn <name>(&self, ...) -> Result<Value, ApiError>` in the `impl ApiClient`
     block of `src/api/client.rs` (or the matching `src/api/*.rs` area file); provenance headers,
     the 300 s timeout and the error body already come from the shared client.
  2. Return `Value` only until a second call site needs a shape; then add the typed struct next to
     the method and deserialise into it.
  3. Add an `ApiError` variant only when the command branches on it (auth, 2FA, refusal); otherwise
     let the message carry the code.
  4. Call it from the command and branch on the error, not on the status number.
  5. No endpoint is called from two places: move the call to the client when a second command needs it.
- Check: `cargo check --all-targets`

## Add a progress report

Progress is a sink: the upload and download loops report chunks, the sink decides what to draw.
The numbers live in a pure model (`src/transfer_stats/`), the lines in `src/transfer_view/` (task 2333).

- Files: `src/upload/progress.rs`, `src/upload.rs`, `src/transfer_stats/mod.rs`, `src/transfer_view/sink.rs`, `src/transfer_view/line.rs`
- Pattern: `ChunkProgress` in `src/upload/progress.rs`, `LiveProgress` in `src/transfer_view/sink.rs`, `NoopProgress` in `src/upload/progress.rs`
- Steps:
  1. Report from the loop through `ChunkProgress` / `FileProgress`: `InFlight::start` around each
     chunk's transfer, `chunk_retried` per retry, `chunk_done` only after the server's answer (an
     upload) or a decrypted frame (a download), so every number counts held bytes.
  2. Draw in the sink, never in the command: `LiveProgress` with `ViewOptions::for_command(verbose)`
     decides the live lines (rich, stdout and stderr terminals), the plain lines and `--quiet`/`--json`.
  3. Create the sink where the run starts, plan every file (`plan_file`, `seal_plan`, `set_limit`), and
     pass it down as `&dyn ChunkProgress`; print through `note` and prompt inside `suspend`. A command
     that runs several files at once hands each file a `FileSink` (its share, under its key) over one
     `RunLimit`, which shows `min(N, Σ used)` of the files in flight (`bb push --parallel`, `bb sync`).
  4. A retry that restarts a file calls `rollback_tally` with the failed attempt's bytes and chunks,
     or the totals drift past the plan.
  5. Re-export a new public sink from `src/upload.rs`; nothing else names `progress::` directly.
- Check: `cargo check --all-targets`
