use std::path::PathBuf;

/// Global configuration for HAPIR CLI.
///
/// Centralizes API URL, token, home directory, and path configuration.
#[derive(Debug, Clone)]
pub struct Configuration {
    pub api_url: String,
    pub cli_api_token: String,
    pub home_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub settings_file: PathBuf,
    pub private_key_file: PathBuf,
    pub runner_state_file: PathBuf,
    pub runner_lock_file: PathBuf,
    pub is_experimental: bool,
}

impl Configuration {
    /// Create configuration from environment variables and defaults.
    pub fn create() -> anyhow::Result<Self> {
        let api_url = std::env::var("HAPIR_API_URL")
            .unwrap_or_else(|_| "http://localhost:3006".into());
        let cli_api_token = std::env::var("CLI_API_TOKEN").unwrap_or_default();

        // Home directory: HAPIR_HOME env > ~/.hapir
        let home_dir = if let Ok(home) = std::env::var("HAPIR_HOME") {
            // Expand ~ to home directory
            if home.starts_with('~') {
                if let Some(user_home) = dirs_next::home_dir() {
                    user_home.join(&home[2..])
                } else {
                    PathBuf::from(home)
                }
            } else {
                PathBuf::from(home)
            }
        } else {
            let user_home = dirs_next::home_dir()
                .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
            user_home.join(".hapir")
        };

        std::fs::create_dir_all(&home_dir)?;

        let logs_dir = home_dir.join("logs");
        std::fs::create_dir_all(&logs_dir)?;

        let settings_file = home_dir.join("settings.json");
        let private_key_file = home_dir.join("access.key");
        let runner_state_file = home_dir.join("runner.state.json");
        let runner_lock_file = home_dir.join("runner.state.json.lock");

        let is_experimental = matches!(
            std::env::var("HAPIR_EXPERIMENTAL").as_deref(),
            Ok("true") | Ok("1") | Ok("yes")
        );

        Ok(Self {
            api_url,
            cli_api_token,
            home_dir,
            logs_dir,
            settings_file,
            private_key_file,
            runner_state_file,
            runner_lock_file,
            is_experimental,
        })
    }

    /// Load settings from file and merge with env-based config.
    ///
    /// Priority: env > settings file > default.
    pub fn load_with_settings(&mut self) -> anyhow::Result<()> {
        let settings = super::persistence::read_settings(&self.settings_file)?;

        // CLI API token: env > settings file
        if self.cli_api_token.is_empty() {
            if let Some(ref token) = settings.cli_api_token {
                tracing::debug!("CLI_API_TOKEN loaded from settings file");
                self.cli_api_token = token.clone();
                // Propagate to env so child processes (e.g. spawned sessions) inherit it
                unsafe { std::env::set_var("CLI_API_TOKEN", token) };
            }
        } else {
            tracing::debug!("CLI_API_TOKEN loaded from environment variable");
        }

        // API URL: env > settings file > default
        if std::env::var("HAPIR_API_URL").is_err() {
            if let Some(ref url) = settings.api_url {
                self.api_url = url.clone();
            } else if let Some(ref url) = settings.server_url {
                // Legacy migration
                self.api_url = url.clone();
            }
        }

        Ok(())
    }
}
