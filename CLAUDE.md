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


## Graphify

This repo has a knowledge graph at graphify-out/.
- Before exploring code, read graphify-out/GRAPH_REPORT.md for module structure and relationships
- After modifying code, run `graphify update .` and commit the updated graphify-out/

## Keep shared docs in sync

When you add/change/remove endpoints, types, build commands, or dependencies: update the relevant skill file in `/home/guus/code/beebeeb.io/.claude/skills/` (beebeeb-api.md, beebeeb-designs.md, beebeeb-stack.md, beebeeb-dev.md). Other agents depend on these being accurate.
