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

/// What `BB_TRANSPORT` says. `Err` for a value that is neither `api` nor `auto`.
fn parse(value: &str) -> Result<Transport, ()> {
    match value.trim().to_ascii_lowercase().as_str() {
        "api" => Ok(Transport::Api),
        "auto" => Ok(Transport::Auto),
        _ => Err(()),
    }
}

fn from_env() -> Option<Transport> {
    let raw = std::env::var("BB_TRANSPORT").ok()?;
    match parse(&raw) {
        Ok(t) => Some(t),
        Err(()) => {
            // A typo must not silently change how files travel in either direction: say so, once
            // (this runs once per process), and use the default.
            eprintln!("bb: ignoring BB_TRANSPORT={raw:?} (expected \"api\" or \"auto\"); using auto");
            None
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_accepts_api_and_auto_and_nothing_else() {
        assert_eq!(parse("api"), Ok(Transport::Api));
        assert_eq!(parse(" API "), Ok(Transport::Api));
        assert_eq!(parse("auto"), Ok(Transport::Auto));
        assert_eq!(parse("Auto"), Ok(Transport::Auto));
        for bad in ["", "direct", "apii", "1", "off"] {
            assert_eq!(parse(bad), Err(()), "{bad:?}");
        }
    }
}
