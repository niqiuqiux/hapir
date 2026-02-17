use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id_confirmed_by_server: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_auto_start_when_running_happy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_api_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vapid_keys: Option<VapidKeys>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram_bot_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telegram_notification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cors_origins: Option<Vec<String>>,
    // Legacy fields for migration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webapp_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webapp_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webapp_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VapidKeys {
    pub public_key: String,
    pub private_key: String,
}

pub fn settings_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("settings.json")
}

/// Read settings from file. Returns `Ok(Some(Settings::default()))` if file
/// doesn't exist. Returns `Err` if the file exists but cannot be parsed (to
/// avoid silent data loss).
pub fn read_settings(path: &Path) -> Result<Option<Settings>> {
    if !path.exists() {
        return Ok(Some(Settings::default()));
    }
    let content = std::fs::read_to_string(path)?;
    let settings: Settings = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(Some(settings))
}

/// Write settings atomically (temp file + rename).
pub fn write_settings(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(settings)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
