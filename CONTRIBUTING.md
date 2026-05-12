# Contributing to Beebeeb CLI

Thanks for your interest in contributing to `bb`, the Beebeeb command-line tool.

## Prerequisites

- Rust 1.85+ (stable toolchain)
- Git

## Development setup

```sh
git clone https://github.com/beebeeb-io/cli.git
cd cli
cargo build
cargo test
```

## Code quality checks

Run these before submitting a pull request:

```sh
cargo fmt -- --check
cargo clippy -- -D warnings
cargo test
```

## Releases

Releases are managed via `cargo-dist`. See [RELEASING.md](RELEASING.md) for the full release process.

## Pull request process

1. Fork the repository and create a feature branch from `main`.
2. Make your changes, ensuring all checks above pass.
3. Write or update tests for your changes.
4. Open a pull request with a clear description of what and why.

## Contributor license

Beebeeb does not require a separate Contributor License Agreement at this time.
By opening a pull request, you confirm you have the right to submit the work and
agree that it is licensed under AGPL-3.0-or-later.

## Security

If you discover a security vulnerability, **do not open a public issue**. Email [security@beebeeb.io](mailto:security@beebeeb.io) instead. See [SECURITY.md](SECURITY.md) for details.

## License

By contributing, you agree that your contributions will be licensed under the [AGPL-3.0-or-later](LICENSE).
