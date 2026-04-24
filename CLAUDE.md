# beebeeb-io/cli

`bb` — Beebeeb CLI for end-to-end encrypted cloud storage.

## Build

```sh
cargo build          # Binary at target/debug/bb
cargo clippy -- -D warnings
```

## Commands

`bb login`, `bb whoami`, `bb push <path>`, `bb pull <id>`, `bb ls [path]`, `bb share <path>`, `bb rotate`, `bb logout`

## Dependencies

Uses `beebeeb-core` and `beebeeb-types` from the `core` repo via Cargo git dependency. For local development, add to `.cargo/config.toml`:
```toml
[patch."https://github.com/beebeeb-io/core"]
beebeeb-core = { path = "../core/beebeeb-core" }
beebeeb-types = { path = "../core/beebeeb-types" }
```

## Config

Stored at `~/.config/beebeeb/config.json` (session token + API URL + email).

## Design reference

Terminal mockups: `../../design/hifi/hifi-cli.jsx`

## Colors

Amber `#f5b800` for branding, green `#8fc18b` for success, red `#e07a6a` for errors.
