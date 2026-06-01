<p align="center">
  <a href="https://beebeeb.io"><img src="https://beebeeb.io/assets/beebeeb-icon.png" alt="beebeeb" width="72" height="72" /></a>
</p>
<h1 align="center">beebeeb cli</h1>
<p align="center">bb — the command-line client for beebeeb. Encrypt, sync, and share from your terminal; the server never sees plaintext.</p>
<p align="center"><strong>We can't recover your data. Not even if we wanted to.</strong> That's the point.</p>
<p align="center">
  <a href="https://github.com/beebeeb-io/cli/releases/latest"><img src="https://img.shields.io/github/v/release/beebeeb-io/cli?label=release" alt="Release" /></a> &nbsp;
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-555.svg" alt="License" /></a> &nbsp;
  <img src="https://img.shields.io/badge/rust-2024-555.svg" alt="Rust" /> &nbsp;
  <a href="SECURITY.md"><img src="https://img.shields.io/badge/security-policy-555.svg" alt="Security" /></a> &nbsp;
  <a href="https://get.beebeeb.io"><img src="https://img.shields.io/badge/install-get.beebeeb.io-f5b800.svg" alt="Install" /></a>
</p>
<p align="center"><a href="https://beebeeb.io">Website</a> &nbsp;·&nbsp; <a href="https://beebeeb.io/security">How it works</a> &nbsp;·&nbsp; <a href="SECURITY.md">Report a vulnerability</a></p>
<p align="center"><sub>End-to-end encrypted cloud storage, built in Europe. Operated by Initlabs B.V., Wijchen, Netherlands.</sub></p>

---

`bb` authenticates from your terminal, encrypts and uploads files, lists and downloads your vault, creates and revokes share links, runs bidirectional folder sync, and exposes your vault over WebDAV or FUSE. Every file and filename is encrypted on your machine before it leaves — the server only ever stores ciphertext.

## Install

### Homebrew (macOS / Linux)

```sh
brew install beebeeb-io/tap/bb
```

Recommended. Updates come through `brew upgrade bb`.

### One-line installer (macOS / Linux)

```sh
curl -fsSL https://get.beebeeb.io | sh
```

Downloads the latest release, verifies its SHA-256 checksum, and installs `bb` into `~/.cargo/bin`. Re-run to upgrade.

### Release binary (all platforms)

Grab the archive for your platform from the [latest release](https://github.com/beebeeb-io/cli/releases/latest) and put `bb` (or `bb.exe`) on your `PATH`. Prebuilt targets: macOS (Apple Silicon, Intel), Linux x86_64/aarch64 (musl), and Windows x64. On Windows you can also `scoop install https://raw.githubusercontent.com/beebeeb-io/cli/main/scoop/bb.json`.

## Quickstart

```sh
bb login                    # Browser handoff — token + master key arrive encrypted
bb push ./report.pdf        # Encrypt locally, then upload
bb ls                       # List your vault (names decrypted on your machine)
bb pull report.pdf          # Download and decrypt by path or UUID
bb sync ~/vault /Documents  # Two-way sync, then watch for live changes
```

## Commands

| Command | What it does |
| --- | --- |
| `bb login` / `bb logout` | Start or end a session via browser device authorization |
| `bb whoami` / `bb status` | Show email, device, region, and quota |
| `bb quota` | Storage usage with a color-coded bar |
| `bb config` | Print configuration with secrets masked |
| `bb push <path>` | Encrypt and upload a file or folder (alias `bb upload`) |
| `bb pull <path-or-id>` | Download and decrypt by vault path or UUID (alias `bb download`) |
| `bb ls [path]` | List vault contents with locally decrypted names |
| `bb share <file-id>` | Create an encrypted share link (`--expires`, `--max-opens`, `--passphrase`, `--double-encrypted`) |
| `bb shares` / `bb unshare` | List or revoke share links |
| `bb request <create\|list\|send\|rm>` | Account-less links that let anyone upload *into* your vault |
| `bb sync <local> [remote]` | Bidirectional folder sync; continuous by default (`--once`, `--daemon`, `--delete`) |
| `bb webdav` | Serve the vault over local WebDAV (Finder, Explorer, rclone, Cyberduck) |
| `bb mount <point>` | Mount the vault as a filesystem via FUSE (macFUSE / libfuse3) |
| `bb billing show` | Read-only plan, storage, and renewal info |
| `bb speedtest` | Benchmark network throughput and crypto speed |
| `bb completions <shell>` | Print a completion script for bash, zsh, fish, or powershell |

Full reference, including every flag: `bb --help`.

## Security model

beebeeb is zero-knowledge: the server stores only ciphertext and cannot read your file contents, filenames, or share payloads.

- **Keys.** Your master key is derived at login and stays on your device. Per-file keys are derived from it with HKDF using the file's UUID as context, so every file has a unique key.
- **Encryption.** File content and filenames are sealed with AES-256-GCM before upload, in independently-nonced 1 MB chunks.
- **Login.** The device-authorization flow uses an ephemeral P-256 ECDH keypair, so the session token and master key are delivered encrypted and never exposed in transit.
- **Session storage.** After login, the token and master key live in `~/Library/Application Support/beebeeb/config.json` (macOS) or `~/.config/beebeeb/config.json` (Linux). Guard this file like an SSH identity. Sessions expire after 30 days.
- **Share links.** Standard links carry the file key in the URL. `--double-encrypted` wraps the file key under a client key kept in the URL fragment (never sent to the server); `--passphrase` wraps it with Argon2id key material.

The crypto itself lives in the shared [`core`](https://github.com/beebeeb-io/core) crate, so the CLI, web, and mobile clients all encrypt the same way and read each other's files.

## Build

```sh
cargo build --release                  # bb at target/release/bb
cargo build --release --features fuse  # with FUSE mount support
cargo test
cargo clippy -- -D warnings
```

`bb` depends on `beebeeb-core` via a Cargo git dependency. For local development, see [CLAUDE.md](CLAUDE.md) for the `[patch]` config that points at a sibling `core` checkout. Release packaging is driven by `cargo-dist` (`dist-workspace.toml`); see [RELEASING.md](RELEASING.md).

## Security

Found a vulnerability? Email **security@beebeeb.io** — see [SECURITY.md](SECURITY.md).

## Part of beebeeb

End-to-end encrypted, zero-knowledge cloud storage — made in Europe.
[core](https://github.com/beebeeb-io/core) · [cli](https://github.com/beebeeb-io/cli) · [web](https://github.com/beebeeb-io/web) · [mobile](https://github.com/beebeeb-io/mobile) · [desktop](https://github.com/beebeeb-io/desktop) · [website](https://beebeeb.io)

## License

[AGPL-3.0-or-later](LICENSE) — © Initlabs B.V. (KvK 95157565), Wijchen, Netherlands.
