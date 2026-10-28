use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

const DEFAULT_API_URL: &str = "https://api.beebeeb.io";
static API_URL_OVERRIDE: OnceLock<String> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_url: String,
    pub session_token: Option<String>,
    pub email: Option<String>,
    /// Base64-encoded 32-byte master key derived during login.
    /// Used to derive per-file encryption keys for push/pull.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            session_token: None,
            email: None,
            master_key: None,
        }
    }
}

fn config_path() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("beebeeb");
    config_dir.join("config.json")
}

pub fn load_config() -> Config {
    let path = config_path();
    let mut config = if !path.exists() {
        Config::default()
    } else {
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Config::default(),
        }
    };

    if let Some(api_url) = API_URL_OVERRIDE.get() {
        config.api_url = api_url.clone();
    }
    refuse_non_loopback_when_asked(&config.api_url);
    config
}

/// The variable a TEST harness sets on every run of the binary. With it, a run whose API URL is not on this
/// machine stops before any request (see [`refuse_non_loopback_when_asked`]).
pub const REFUSE_NON_LOOPBACK_ENV: &str = "BB_REFUSE_NON_LOOPBACK_API";

/// Is `url` a server on this machine (`localhost`, `127.0.0.0/8`, `::1`)? Fail closed: anything that does not
/// parse, has no host, or names another host (including `localhost.` and `127.0.0.1.example.com`) is not.
pub(crate) fn api_url_is_loopback(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The CLI's API URL comes from `--api`, the config file, or the built-in default, https://api.beebeeb.io. It is
/// NOT read from the environment (there is no `BB_API_BASE`). A test that runs the binary with a scratch HOME and
/// names no server therefore points it at PRODUCTION. So a harness sets [`REFUSE_NON_LOOPBACK_ENV`], and a run
/// that would use a non-loopback URL exits 2 here instead, before it sends anything. It only ever refuses.
fn refuse_non_loopback_when_asked(api_url: &str) {
    let asked = std::env::var(REFUSE_NON_LOOPBACK_ENV).is_ok_and(|v| {
        let v = v.trim();
        !v.is_empty() && v != "0"
    });
    if !asked || api_url_is_loopback(api_url) {
        return;
    }
    let host = reqwest::Url::parse(api_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "an address that does not parse".to_string());
    eprintln!(
        "Refusing to contact {host}: {REFUSE_NON_LOOPBACK_ENV} is set, so this run may only talk to a server on this \
         machine. Name one with --api, or in the config file."
    );
    std::process::exit(2);
}

pub fn set_api_url_override(api_url: String) -> Result<(), String> {
    let api_url = api_url.trim_end_matches('/').to_string();
    if api_url.is_empty() {
        return Err("--api must not be empty".to_string());
    }
    API_URL_OVERRIDE
        .set(api_url)
        .map_err(|_| "--api was already configured".to_string())
}

pub fn save_config(config: &Config) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create config directory: {e}"))?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("failed to serialize config: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("failed to write config: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("failed to set config permissions: {e}"))?;
    }
    Ok(())
}

pub fn clear_config() -> Result<(), String> {
    crate::custody::clear_current()?;
    let path = config_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("failed to remove config: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod loopback_tests {
    use super::api_url_is_loopback;

    #[test]
    fn only_a_server_on_this_machine_is_loopback() {
        for url in [
            "http://127.0.0.1:3001",
            "http://127.0.0.1",
            "https://127.1.2.3:8443",
            "http://localhost:5173",
            "http://LOCALHOST:5173/path",
            "http://[::1]:3001",
            "https://localhost",
        ] {
            assert!(api_url_is_loopback(url), "{url}");
        }
        for url in [
            "https://api.beebeeb.io",
            "http://api.beebeeb.io:3001",
            "http://127.0.0.1.example.com",
            "http://localhost.example.com",
            "http://localhost.",
            "http://127.0.0.1@evil.example",
            "http://0.0.0.0:3001",
            "http://10.0.0.5",
            "ftp://127.0.0.1",
            "127.0.0.1:3001",
            "",
            "not a url",
        ] {
            assert!(!api_url_is_loopback(url), "{url}");
        }
    }
}
