use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEFAULT_API_URL: &str = "http://localhost:3001";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_url: String,
    pub session_token: Option<String>,
    pub email: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_API_URL.to_string(),
            session_token: None,
            email: None,
        }
    }
}

fn config_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beebeeb");
    config_dir.join("config.json")
}

pub fn load_config() -> Config {
    let path = config_path();
    if !path.exists() {
        return Config::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save_config(config: &Config) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config directory: {e}"))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("failed to serialize config: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("failed to write config: {e}"))?;
    Ok(())
}

pub fn clear_config() -> Result<(), String> {
    let path = config_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("failed to remove config: {e}"))?;
    }
    Ok(())
}
