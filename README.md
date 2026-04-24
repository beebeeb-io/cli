<p align="center">
  <img src="https://beebeeb.io/icon.png" alt="Beebeeb" width="60" />
</p>
<h3 align="center">Beebeeb CLI</h3>
<p align="center"><code>bb</code> — end-to-end encrypted cloud storage from the terminal.</p>

<p align="center">
  <a href="https://github.com/beebeeb-io/cli/blob/main/LICENSE"><img src="https://img.shields.io/github/license/beebeeb-io/cli" alt="License"></a>
  <a href="https://github.com/beebeeb-io/cli/actions"><img src="https://img.shields.io/github/actions/workflow/status/beebeeb-io/cli/ci.yml" alt="CI"></a>
  <a href="https://github.com/beebeeb-io/cli/graphs/contributors"><img src="https://img.shields.io/github/contributors/beebeeb-io/cli" alt="Contributors"></a>
  <a href="https://github.com/beebeeb-io/cli/stargazers"><img src="https://img.shields.io/github/stars/beebeeb-io/cli" alt="Stars"></a>
  <a href="https://github.com/beebeeb-io/cli/issues"><img src="https://img.shields.io/github/issues/beebeeb-io/cli" alt="Issues"></a>
</p>

---

## What is Beebeeb?

Beebeeb is end-to-end encrypted cloud storage where your files are encrypted before they leave your device. The server never sees your plaintext data, file names, or encryption keys. Beebeeb is open source and built by [Initlabs B.V.](https://beebeeb.io), Wijchen, Netherlands.

## This repo

`bb` is the Beebeeb command-line interface. It gives you full access to your encrypted vault from the terminal -- push files, pull files, create share links, manage your keys. Single static binary, no daemon, no root, no telemetry.

```
$ bb --help

end-to-end encrypted vault, from the terminal

Usage: bb <COMMAND>

Commands:
  login    Authenticate with your Beebeeb account
  whoami   Show current session, device, region, quota
  push     Upload a file or folder to your vault
  pull     Download a file from your vault
  ls       List files (decrypts names locally)
  share    Create an encrypted share link
  rotate   Rotate your master vault key
  logout   End current session

Options:
  -h, --help     Print help
  -V, --version  Print version

# docs · beebeeb.io/cli · key fingerprints · beebeeb.io/fingerprints
```

## Commands

| Command | Description | Example |
|---|---|---|
| `bb login` | Authenticate with your Beebeeb account | `bb login` |
| `bb whoami` | Show session, device, region, and quota | `bb whoami` |
| `bb push <path>` | Upload a file or folder to your vault | `bb push ~/documents/report.pdf` |
| `bb pull <id>` | Download a file from your vault | `bb pull abc123 -o report.pdf` |
| `bb ls [path]` | List files (names decrypted locally) | `bb ls /projects` |
| `bb share <path>` | Create an encrypted share link | `bb share report.pdf --expires 7d --passphrase` |
| `bb rotate` | Rotate your master vault key | `bb rotate` |
| `bb logout` | End the current session | `bb logout` |

## Tech stack

| Component | Technology |
|---|---|
| Language | Rust (Edition 2024) |
| Crypto | [beebeeb-core](https://github.com/beebeeb-io/core) (AES-256-GCM, Argon2id, HKDF) |
| CLI framework | clap v4 |
| HTTP client | reqwest |
| Progress bars | indicatif |
| Terminal colors | colored (amber `#f5b800` branding) |
| Config storage | `~/.config/beebeeb/config.json` |

## Getting started

### Prerequisites

- [Rust](https://rustup.rs/) (stable, edition 2024)

### Install from source

```sh
# Clone the repo
git clone https://github.com/beebeeb-io/cli.git
cd cli

# Build
cargo build --release

# The binary is at target/release/bb
./target/release/bb --help
```

### Local development with the core repo

If you are working on both `cli` and `core` simultaneously, add a Cargo patch to point at your local checkout. Create or edit `.cargo/config.toml`:

```toml
[patch."https://github.com/beebeeb-io/core"]
beebeeb-core = { path = "../core/beebeeb-core" }
beebeeb-types = { path = "../core/beebeeb-types" }
```

### Lint

```sh
cargo clippy -- -D warnings
```

## How it works

1. **Login** -- `bb login` authenticates with the Beebeeb API and stores a session token at `~/.config/beebeeb/config.json`.
2. **Push** -- files are encrypted locally using your master key (AES-256-GCM with a per-file HKDF-derived key) before being uploaded. The server only ever receives ciphertext.
3. **Pull** -- encrypted blobs are downloaded and decrypted locally. File names are also encrypted and only decrypted on your machine.
4. **Share** -- creates a time-limited, optionally passphrase-protected link. The share key is encoded in the URL fragment, so the server never sees it.
5. **Rotate** -- re-derives your master key and re-wraps all file keys. (Coming soon.)

## Security

All encryption happens on your device. The server stores only ciphertext and has no access to your keys or plaintext data.

**Found a vulnerability?** Please report it responsibly. See [SECURITY.md](./SECURITY.md) for details, or email [security@beebeeb.io](mailto:security@beebeeb.io). We aim to acknowledge reports within 48 hours.

## Contributing

We welcome contributions! Whether it is a bug report, a feature request, or a pull request -- we appreciate your help making Beebeeb better.

1. Fork the repository
2. Create a feature branch (`git checkout -b feat/your-feature`)
3. Make your changes
4. Run the linter (`cargo clippy -- -D warnings`)
5. Commit your changes -- pre-commit hooks will run secret scanning automatically
6. Open a pull request against `main`

## Built with Beebeeb

This CLI is part of the Beebeeb ecosystem:

| Repo | Description |
|---|---|
| [core](https://github.com/beebeeb-io/core) | Cryptographic core, shared types, and sync engine |
| **[cli](https://github.com/beebeeb-io/cli)** | `bb` -- end-to-end encrypted cloud storage from the terminal (you are here) |
| [desktop](https://github.com/beebeeb-io/desktop) | Desktop sync client for macOS, Windows, and Linux |

## License

This project is licensed under the [GNU Affero General Public License v3.0 or later](./LICENSE).

Copyright (c) Initlabs B.V.

## Links

- [Website](https://beebeeb.io)
- [Security policy](./SECURITY.md)
- [GitHub organization](https://github.com/beebeeb-io)
