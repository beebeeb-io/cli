# bb 0.14.5: uploads straight to storage

`bb push` and `bb sync` can now send a file's encrypted chunks directly to Beebeeb's object storage instead of streaming every byte through the API. The server hands out one short-lived upload link per chunk. The CLI encrypts on your machine exactly as before: the links carry ciphertext only, and your keys never leave the device. When the server does not offer direct uploads, or a direct upload cannot finish, the file goes through the API as it did in 0.14.4.

### What's New

- **Direct uploads.** When the server advertises direct uploads (`GET /api/v1/transfer/capabilities` lists `object-upload-v1`), the CLI asks for one when it opens an upload. If the server opens a direct session, each chunk is sent to storage on a time-limited link the server grants for that chunk, then confirmed with the API (grant, PUT, commit). The file counts as uploaded only after every chunk is confirmed and the upload is completed.
- **Automatic fallback.** If a direct upload cannot finish (a server node without the direct routes, a link the server refuses, storage failing four times for one chunk), the session is ended, its progress is taken back, and the whole file is sent again through the API. That process then starts no further direct upload for ten minutes, so a server where direct uploads are broken costs the files already in flight, not every file.
- **Interrupted uploads continue where they stopped.** A re-run of an interrupted `bb push` continues on the transport the upload began on and skips the chunks the server already holds. A direct upload is given up only when the server says it offers no direct uploads, or when you pass `--transport api`. If the server's capabilities cannot be read at that moment, or your login is no longer valid, the file fails for that run with a message that says which, and the upload and its progress are kept.
- **`bb push --json` reports `transport`** (`direct` or `api`) for every file. This is an additive key; existing keys are unchanged.
- **`BB_TRANSPORT=api`** (or `--transport api`) forces the API for every file, uploads included. `BB_TRANSPORT_TRACE=1` still prints each transport decision to stderr, never a URL.

### Bug Fixes / Hardening

- A chunk the server is still finishing after a failed storage copy is waited out for up to seven and a half minutes. Before, such a chunk was given up on after about half a minute, which would have sent a large file back to the start through the API.
- Storage uploads have no read timeout of their own: a chunk on a slow uplink can take as long as it needs, bounded by one total timeout sized to the chunk. A 128 MiB chunk on an uplink slower than about 18 Mbit/s would otherwise have been cut off after 60 seconds.
- A redirect or an interim answer from a direct upload route is treated as a wrong path: the file goes through the API at once rather than being retried.
- A 401 on a direct upload reads "run `bb login`", like the API route. When the server refuses the capabilities lookup of an interrupted upload with a 403, the message says so with the server's reason instead of calling it an expired session.
- What the server said about direct transfer is remembered per server for the life of the process (ten minutes). `bb sync --daemon` used to ask the capabilities route again on every resync. A refused lookup is remembered for one minute, for that login only, so a new `bb login` is not held back by it.
- Storage requests use an HTTP client of their own, with no cookies, no `Authorization` header, no redirects and https only (plain http only to a local development API). Their error messages never contain a storage URL.
- `BB_REFUSE_NON_LOOPBACK_API=1` makes `bb` exit with status 2, before sending anything, when its API URL is not on your own machine. Nothing changes for anyone who does not set it. Our tests set it on every run of the binary, and a scan fails any test that does not, because a `bb` run with no `--api` and no config would otherwise talk to the default server.

### Compatibility

- **This CLI with an older server, or a server with direct uploads off:** the server advertises no direct uploads, so every file uses the API, exactly as in 0.14.4. The upload request carries no new header, and the CLI asks the capabilities route once (the answer is remembered for ten minutes).
- **An older CLI with a newer server:** unchanged. 0.14.4 and earlier never ask for direct uploads.
- **Downgrading while a direct upload is half-finished:** an older CLI cannot continue it. The re-run fails until the server's upload lease expires; then it starts the file again.
- Dependency change: `beebeeb-core` / `beebeeb-types` pinned to `55d23a89` (the shared direct-transfer rules, now including the upload rules). No other dependency versions changed.

### Verification

- Full test suite on Linux, at the head merged as `d793d46b`, in a sandbox with no network access and an egress recorder: 787 passed, 0 failed, 4 ignored across 25 test binaries; 0 outbound connection attempts; no service manager touched. The same count came out of two separate runs. Clippy with warnings denied and rustfmt are clean.
- Against a real server and an S3-compatible store, with the CLI under test and the released 0.14.3 reading every file back (byte-identical, no signature in any client output): a new CLI with an old server, with direct uploads off, with direct uploads on (7 of 7 chunks direct, 21 of 21 for a 160 MiB file), an old CLI with a new server, with the API forced, a push killed after 3 chunks and re-run (the committed chunks are skipped, 7 commits in all), and a chunk on a throttled link that takes 72 seconds (sent direct; with a 60-second read timeout the same chunk was cut off and the file went through the API). 9 of 9 cells passed on the final build.
- Two runs over TLS against a test bucket, from a client that could reach only the storage host: a 24 MiB file (7 of 7 chunks direct) and a small file went direct with uploads on, and both read back byte-identical with 0.14.4 and with this build; with uploads off they used the API. The bucket held no objects afterwards.
- `tests/api_guard.rs` fails any test that runs the `bb` binary without the guard that refuses a non-local API URL, and its own fixtures prove it turns red; the whole suite ran with no route to the network.
- Release gates: full required CLI CI, five platform builds, and draft asset readback with checksum assertions before publishing.

### Install / Update

```sh
bb update
bb --version   # bb 0.14.5
```

Homebrew: `brew upgrade beebeeb-io/tap/bb`. Scoop: `scoop update bb`. New installs: `curl -fsSL https://get.beebeeb.io | sh`. Restart a running `bb sync` daemon after updating.

Full changelog: https://github.com/beebeeb-io/cli/compare/v0.14.4...v0.14.5
