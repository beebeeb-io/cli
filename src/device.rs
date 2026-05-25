use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: Uuid,
    pub hostname: String,
    pub platform: String,
    pub registered_at: String,
}

fn device_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("beebeeb")
        .join("device.json")
}

pub fn load_or_create() -> DeviceInfo {
    let path = device_path();
    if path.exists() {
        if let Ok(data) = std::fs::read_to_string(&path) {
            if let Ok(info) = serde_json::from_str::<DeviceInfo>(&data) {
                return info;
            }
        }
    }

    let info = DeviceInfo {
        device_id: Uuid::new_v4(),
        hostname: hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string()),
        platform: current_platform(),
        registered_at: chrono::Utc::now().to_rfc3339(),
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&info).unwrap_or_default(),
    );
    info
}

pub fn get_device_id() -> Uuid {
    load_or_create().device_id
}

fn current_platform() -> String {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
    .to_string()
}
