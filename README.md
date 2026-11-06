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

Grab the archive for your platform from the [latest release](https://github.com/beebeeb-io/cli/releases/latest) — or the stable redirect at [beebeeb.io/download/cli](https://beebeeb.io/download/cli), which always points at the current installer — and put `bb` (or `bb.exe`) on your `PATH`. Prebuilt targets: macOS (Apple Silicon, Intel), Linux x86_64/aarch64 (musl), and Windows x64. On Windows you can also `scoop install https://raw.githubusercontent.com/beebeeb-io/cli/main/scoop/bb.json`.

## Quickstart

```sh
bb signup                   # No account yet? Opens the web signup (then bb login)
bb login                    # Browser handoff — token + master key arrive encrypted
bb push ./report.pdf        # Encrypt locally, then upload
bb ls                       # List your vault (names decrypted on your machine)
bb pull report.pdf          # Download and decrypt by path or UUID
bb sync ~/vault /Documents  # Two-way sync, then watch for live changes
```

## Commands

| Command | What it does |
| --- | --- |
| `bb update` | Check for and install the latest CLI release now |
| `bb signup` | Open the web signup. Accounts are created in the web app (a new account starts a trial that needs a payment method); the CLI only signs in |
| `bb login` / `bb logout` | Start or end a session via browser device authorization |
| `bb whoami` / `bb status` | Show email, device, region, quota, and account state (no plan yet, or trial ended and read-only) |
| `bb quota` | Storage usage with a color-coded bar |
| `bb region <list\|get\|set>` | Where new uploads are stored, by country: Europe (the default, any of our European data centers, we place your files), or one country such as Germany or Finland. The list comes from the server, with a coming-soon country shown but not selectable. `bb region set de` (or `germany`, or `europe`) chooses; files already uploaded stay where they are |
| `bb config` | Print configuration with secrets masked |
| `bb push <path>` | Encrypt and upload a file or folder (alias `bb upload`) |
| `bb pull <path-or-id>` | Download and decrypt by vault path or UUID (alias `bb download`) |
| `bb ls [path]` | List vault contents with locally decrypted names |
| `bb share <path-or-id>` | Create an end-to-end encrypted share link by vault path, the short ID from the listing, or UUID; the key rides in the URL fragment (`--expires`, `--max-opens`, `--passphrase`) |
| `bb shares` / `bb unshare` | List or revoke share links |
| `bb request <create\|list\|send\|rm>` | Account-less links that let anyone upload *into* your vault |
| `bb sync <local> [remote]` | Bidirectional folder sync; continuous by default (`--once`, `--daemon`, `--delete`) |
| `bb backup <add\|remove\|list\|run\|status>` | Back up folders of this machine (a Linux server or NAS) into `Backups/<this machine>/`, read where they are; see [Backups](#backups) |
| `bb webdav` | Serve the vault over local WebDAV (Finder, rclone, Cyberduck, WinSCP; not Windows Explorer, see below). Read-only by default (`--read-write` to allow changes); prints a random per-run password that every client must send |
| `bb mount <point>` | FUSE mount (macFUSE / libfuse3). Source builds with `--features fuse` only; release binaries do not include it, so use `bb webdav` |
| `bb billing <show\|usage\|invoices\|portal\|addons>` | Plan and renewal info, per-region usage, invoices (`bb billing invoices --open <id>` downloads one as PDF), the Beebeeb web billing page, and add-ons |
| `bb speedtest` | Benchmark network throughput and crypto speed |
| `bb completions <shell>` | Print a completion script for bash, zsh, fish, or powershell |

### `bb webdav` and Windows

`bb webdav` listens on `127.0.0.1` over plain HTTP and protects the vault with a random per-run password (HTTP Basic). Windows Explorer ("Map network drive") refuses Basic authentication over plain HTTP by default, so **Windows Explorer is not supported by `bb webdav`**. On Windows, use the Beebeeb desktop app, or connect with rclone, Cyberduck or WinSCP, which send the password over HTTP on loopback. On macOS, Finder's Go, Connect to Server accepts the printed URL and asks for the password.

Command overview: `bb --help`. Every flag of a command: `bb <command> --help` (for example `bb push --help`).

### Backups

`bb backup` backs up folders of a headless Linux machine into `Backups/<this machine>/<folder>/`, end-to-end
encrypted. Files are read where they are and never changed. Each run uploads what is new or changed since the
last run; a changed file becomes a new version of the same file.

```sh
bb backup add /srv/photos   # back up a folder of this machine
bb backup run               # one pass: what is new or changed since the last run
bb backup status            # each folder: on or off, files backed up, the last run's counts
bb backup list
bb backup remove /srv/photos  # stop; what is already in Backups stays there
```

The machine is named by its pretty hostname (else its hostname) and recognised by `/etc/machine-id`, so a
reinstall keeps backing up into the same folder. A folder inside bb's own folders (its settings, a `bb sync`
folder, a `bb mount`) cannot be added; bb's settings and mounts are never walked into. A `bb sync` folder below a
folder you add is backed up like any other folder, so add the folders next to it instead.

Backups is read-only everywhere else: `bb push`, `bb mkdir` and `bb mv` into it are refused, and `bb ls` shows it
as `[Read-only]`. `bb rm` can delete a device's backup or one of its folders after a question (`--yes` for
scripts). If a folder's backup is deleted from another device, it turns off: `bb backup run` uploads nothing of it,
`bb backup status` says why and exits 4, and `bb backup add <folder>` backs it up again.

To run it every hour, as your user, with systemd:

```ini
# ~/.config/systemd/user/beebeeb-backup.service
[Unit]
Description=Beebeeb backup (one pass)
After=network-online.target

[Service]
Type=oneshot
ExecStart=%h/.local/bin/bb backup run
```

```ini
# ~/.config/systemd/user/beebeeb-backup.timer
[Unit]
Description=Run the Beebeeb backup every hour

[Timer]
OnCalendar=hourly
Persistent=true
RandomizedDelaySec=5m

[Install]
WantedBy=timers.target
```

Then `systemctl --user daemon-reload && systemctl --user enable --now beebeeb-backup.timer`. On a server
without a login session, also run `loginctl enable-linger $USER` once, so the timer runs while nobody is signed
in. `journalctl --user -u beebeeb-backup` shows each run's counts. Exit codes: `0` done, `3` some files failed
(tried again next run), `4` a backup is off, `75` another run is still going.

## Security model

beebeeb is zero-knowledge: the server stores only ciphertext and cannot read your file contents, filenames, or share payloads.

- **Keys.** Your master key is derived at login and stays on your device. Per-file keys are derived from it with HKDF using the file's UUID as context, so every file has a unique key.
- **Encryption.** File content and filenames are sealed with AES-256-GCM before upload, in independently-nonced chunks. Chunk size scales with the file, from 4 MiB up to 128 MiB, and shrinks when `bb sync` uploads several files in parallel so memory stays bounded.
- **Login.** The device-authorization flow uses an ephemeral P-256 ECDH keypair, so the session token and master key are delivered encrypted and never exposed in transit.
- **Session storage.** After login, the token and master key live in `~/Library/Application Support/beebeeb/config.json` (macOS) or `~/.config/beebeeb/config.json` (Linux). Guard this file like an SSH identity. Sessions expire after 30 days.
- **Share links.** Every link `bb share` creates is double-encrypted: `bb` generates a random link key `K_c`, wraps the file key under it (AES-256-GCM), and puts the link key in the URL fragment (`#key=…`), which browsers never send to a server. Beebeeb stores only the wrapped key and cannot open the file. Anyone holding the full link can, so send it the way you would send the file. `--passphrase` is an access gate, not extra encryption: the passphrase goes to the server over TLS when the link is created and is stored only as an Argon2id hash, which the server checks before handing out the file. So that `bb shares` can show the link again, `K_c` and the link token are also stored wrapped under your master key, which never leaves your device.

The crypto itself lives in the shared [`core`](https://github.com/beebeeb-io/core) crate, so the CLI, web, and mobile clients all encrypt the same way and read each other's files.

## Build

```sh
cargo build --release                  # bb at target/release/bb
cargo build --release --features fuse  # with FUSE mount support (not in release binaries)
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
