pub mod cli_api_token;
pub mod jwt_secret;
pub mod owner_id;
pub mod server_settings;
pub mod settings;
pub mod vapid_keys;

use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Env,
    File,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliApiTokenSource {
    Env,
    File,
    Generated,
}

#[derive(Debug, Clone)]
pub struct Configuration {
    pub telegram_bot_token: Option<String>,
    pub telegram_enabled: bool,
    pub telegram_notification: bool,
    pub cli_api_token: String,
    pub cli_api_token_source: CliApiTokenSource,
    pub cli_api_token_is_new: bool,
    pub settings_file: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub listen_port: u16,
    pub listen_host: String,
    pub public_url: String,
    pub cors_origins: Vec<String>,
}

impl Configuration {
    pub async fn create() -> Result<Self> {
        // Resolve data directory: HAPIR_HOME env or ~/.hapi
        let data_dir = if let Ok(home) = std::env::var("HAPIR_HOME") {
            PathBuf::from(home)
        } else {
            let home = dirs_next::home_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
            home.join(".hapir")
        };
        std::fs::create_dir_all(&data_dir)?;

        // Resolve database path: DB_PATH env or {data_dir}/hapi.db
        let db_path = if let Ok(p) = std::env::var("DB_PATH") {
            PathBuf::from(p)
        } else {
            data_dir.join("hapir.db")
        };

        // Settings file path
        let settings_file = settings::settings_file_path(&data_dir);

        // Load server settings (env > file > default)
        let server_result = server_settings::load_server_settings(&data_dir)?;
        let ss = server_result.settings;

        // Load CLI API token (env > file > generate)
        let token_result = cli_api_token::get_or_create_cli_api_token(&data_dir)?;

        let telegram_enabled = ss.telegram_bot_token.is_some();

        Ok(Configuration {
            telegram_bot_token: ss.telegram_bot_token,
            telegram_enabled,
            telegram_notification: ss.telegram_notification,
            cli_api_token: token_result.token,
            cli_api_token_source: token_result.source,
            cli_api_token_is_new: token_result.is_new,
            settings_file,
            data_dir,
            db_path,
            listen_port: ss.listen_port,
            listen_host: ss.listen_host,
            public_url: ss.public_url,
            cors_origins: ss.cors_origins,
        })
    }
}
