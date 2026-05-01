<p align="center">
  <img src="https://beebeeb.io/icon.png" alt="Beebeeb" width="60" />
</p>
<h3 align="center">Beebeeb CLI</h3>
<p align="center"><code>bb</code> -- end-to-end encrypted cloud storage from the terminal.</p>

<p align="center">
  <a href="https://github.com/beebeeb-io/cli/blob/main/LICENSE"><img src="https://img.shields.io/github/license/beebeeb-io/cli" alt="License"></a>
  <a href="https://github.com/beebeeb-io/cli/actions"><img src="https://img.shields.io/github/actions/workflow/status/beebeeb-io/cli/ci.yml" alt="CI"></a>
  <a href="https://github.com/beebeeb-io/cli/stargazers"><img src="https://img.shields.io/github/stars/beebeeb-io/cli" alt="Stars"></a>
</p>

---

`bb` gives you full access to your [Beebeeb](https://beebeeb.io) encrypted vault from the terminal. Push files, pull files, create share links, watch folders for changes. Single static binary, no daemon, no root, no telemetry.

Built and operated by [Initlabs B.V.](https://initlabs.nl), Wijchen, Netherlands.

## Installation

### From source

```sh
git clone https://github.com/beebeeb-io/cli.git
cd cli
cargo build --release
# Binary at target/release/bb
```

Add to your PATH:

```sh
cp target/release/bb ~/.local/bin/
```

## Commands

### `bb login`

Authenticate with your Beebeeb account. Stores a session token locally.

```
$ bb login
  email: alice@example.com
  password: ********
  logged in as alice@example.com
```

### `bb whoami`

Show your current session, device, region, and quota.

```
$ bb whoami
  alice@example.com
  device    MacBook Pro
  region    eu-central (Frankfurt)
  plan      Pro
  storage   2.1 GB / 100 GB
```

### `bb status`

Show connection status, session health, and storage usage.

```
$ bb status
  beebeeb status
  api          https://api.beebeeb.io  connected
  session      valid (expires in 13d)
  storage      2.1 GB / 100 GB (2%)
```

### `bb config`

Show current configuration with secrets masked.

```
$ bb config
  beebeeb config
  api_url        https://api.beebeeb.io
  email          alice@example.com
  session_token  abc12345...6789
```

### `bb push <path>`

Upload a file or folder to your vault. Files are encrypted locally before upload.

```
$ bb push ~/documents/report.pdf
  encrypting report.pdf (2.4 MB)
  uploading  [========================================] 100%
  uploaded   report.pdf -> abc123

$ bb push ~/projects/website --parent folder_id_here
  encrypting 47 files...
  uploading  [========================================] 100%
  uploaded   47 files to /website
```

### `bb pull <file_id>`

Download and decrypt a file from your vault.

```
$ bb pull abc123
  downloading report.pdf (2.4 MB)
  decrypting  [========================================] 100%
  saved       ./report.pdf

$ bb pull abc123 -o ~/downloads/report.pdf
  saved       ~/downloads/report.pdf
```

### `bb ls [path]`

List files. Names are decrypted locally -- the server never sees them.

```
$ bb ls
  documents/          4 items     2026-04-28 14:32
  photos/            12 items     2026-04-30 09:15
  report.pdf          2.4 MB      2026-04-27 11:00

$ bb ls /documents
  contract.pdf        1.1 MB      2026-04-20 16:45
  notes.md            12 KB       2026-04-28 14:32
```

### `bb share <file_id>`

Create an encrypted share link. The share key is encoded in the URL fragment, so the server never sees it.

```
$ bb share abc123
  https://beebeeb.io/s/xyz789#key=...

$ bb share abc123 --expires 7d --max-opens 10 --passphrase
  passphrase: ********
  https://beebeeb.io/s/xyz789#key=...
  expires in 7 days, max 10 opens, passphrase-protected
```

### `bb shares`

List all active share links.

```
$ bb shares
  xyz789  report.pdf   expires 2026-05-07  3/10 opens
  abc456  photos/      no expiry           1 open
```

### `bb unshare <share_id>`

Revoke a share link immediately.

```
$ bb unshare xyz789
  revoked share xyz789
```

### `bb watch <path>`

Watch a folder and auto-sync changes to your vault. Useful for keeping a local directory backed up continuously.

```
$ bb watch ~/documents
  watching ~/documents (Ctrl+C to stop)
  synced   notes.md (12 KB)
  synced   contract.pdf (1.1 MB)
```

### `bb rotate`

Rotate your master vault key and re-wrap all file keys. (Coming soon.)

### `bb logout`

End the current session.

```
$ bb logout
  logged out
```

## Configuration

Session data is stored at `~/.config/beebeeb/config.json`:

```json
{
  "api_url": "https://api.beebeeb.io",
  "email": "alice@example.com",
  "session_token": "..."
}
```

For local development, the API URL defaults to `http://localhost:3001`.

## How it works

1. **Login** -- authenticates with the Beebeeb API and stores a session token locally.
2. **Push** -- files are encrypted with AES-256-GCM using a per-file HKDF-derived key before upload. The server only receives ciphertext.
3. **Pull** -- encrypted blobs are downloaded and decrypted locally. File names are also encrypted.
4. **Share** -- creates a time-limited, optionally passphrase-protected link. The share key is in the URL fragment, so the server never sees it.
5. **Watch** -- monitors a directory for changes (500ms debounce) and auto-pushes new or modified files.

## Tech stack

| Component | Technology |
|---|---|
| Language | Rust (edition 2024) |
| Crypto | [beebeeb-core](https://github.com/beebeeb-io/core) (AES-256-GCM, Argon2id, HKDF) |
| CLI framework | clap v4 |
| HTTP | reqwest |
| Progress | indicatif |
| Colors | colored (amber `#f5b800` branding) |

## Security

All encryption happens on your device. The server stores only ciphertext and never has access to your keys or plaintext data.

Found a vulnerability? See [SECURITY.md](./SECURITY.md) or email [security@beebeeb.io](mailto:security@beebeeb.io). We aim to acknowledge reports within 48 hours.

## Contributing

We welcome contributions.

1. Fork the repository
2. Create a feature branch (`git checkout -b feat/your-feature`)
3. Make your changes
4. Run the linter (`cargo clippy -- -D warnings`)
5. Commit -- pre-commit hooks will run secret scanning automatically
6. Open a pull request against `main`

## Part of Beebeeb

| Repository | Description |
|---|---|
| [core](https://github.com/beebeeb-io/core) | Cryptographic core, shared types, sync engine |
| **[cli](https://github.com/beebeeb-io/cli)** | `bb` -- CLI for encrypted cloud storage (you are here) |
| [desktop](https://github.com/beebeeb-io/desktop) | Desktop sync for macOS, Windows, Linux |
| [web](https://github.com/beebeeb-io/web) | Web client |
| [mobile](https://github.com/beebeeb-io/mobile) | iOS and Android app |

## License

[GNU Affero General Public License v3.0 or later](./LICENSE)

Copyright (c) Initlabs B.V.

---

[beebeeb.io](https://beebeeb.io) -- [Security policy](./SECURITY.md) -- [GitHub](https://github.com/beebeeb-io)
