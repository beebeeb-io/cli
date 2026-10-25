//! Which transport a command may use for file bytes (task 2173).
//!
//! `auto` (the default) lets a command fetch encrypted chunks straight from
//! object storage when the server offers it and the handshake succeeds, and
//! uses the API otherwise. `api` forces today's API transport for every file:
//! the debug switch support and the compatibility matrix need, set with
//! `--transport api` or `BB_TRANSPORT=api` (the flag wins over the variable).

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    /// Direct storage transfers when the server offers them, the API otherwise.
    Auto,
    /// Always the API.
    Api,
}

static SELECTED: OnceLock<Transport> = OnceLock::new();

fn from_env() -> Option<Transport> {
    match std::env::var("BB_TRANSPORT").ok()?.trim().to_ascii_lowercase().as_str() {
        "api" => Some(Transport::Api),
        "auto" => Some(Transport::Auto),
        _ => None,
    }
}

/// Call once from `main()`, after argument parsing.
pub fn init(flag: Option<Transport>) {
    let _ = SELECTED.set(flag.or_else(from_env).unwrap_or(Transport::Auto));
}

/// `true` when the user forced the API transport.
pub fn api_forced() -> bool {
    SELECTED.get().copied() == Some(Transport::Api)
}
