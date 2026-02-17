use super::settings::{read_settings, settings_file_path, write_settings};
use anyhow::Result;
use std::path::Path;

pub struct ServerSettings {
    pub telegram_bot_token: Option<String>,
    pub telegram_notification: bool,
    pub listen_host: String,
    pub listen_port: u16,
    pub public_url: String,
    pub cors_origins: Vec<String>,
}

pub struct ServerSettingsResult {
    pub settings: ServerSettings,
    pub saved_to_file: bool,
}

fn parse_cors_origins(s: &str) -> Vec<String> {
    let entries: Vec<String> = s
        .split(',')
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty())
        .collect();
    if entries.iter().any(|e| e == "*") {
        return vec!["*".into()];
    }
    entries
}

fn derive_cors_origins(public_url: &str) -> Vec<String> {
    url::Url::parse(public_url)
        .ok()
        .map(|u| vec![u.origin().ascii_serialization()])
        .unwrap_or_default()
}

pub fn load_server_settings(data_dir: &Path) -> Result<ServerSettingsResult> {
    let settings_path = settings_file_path(data_dir);
    let mut settings = read_settings(&settings_path)?
        .ok_or_else(|| anyhow::anyhow!("cannot read settings file"))?;
    let mut needs_save = false;

    // telegram_bot_token
    let telegram_bot_token = if let Ok(v) = std::env::var("TELEGRAM_BOT_TOKEN") {
        if settings.telegram_bot_token.is_none() {
            settings.telegram_bot_token = Some(v.clone());
            needs_save = true;
        }
        Some(v)
    } else {
        settings.telegram_bot_token.clone()
    };

    // telegram_notification
    let telegram_notification = if let Ok(v) = std::env::var("TELEGRAM_NOTIFICATION") {
        let val = v == "true";
        if settings.telegram_notification.is_none() {
            settings.telegram_notification = Some(val);
            needs_save = true;
        }
        val
    } else {
        settings.telegram_notification.unwrap_or(true)
    };

    // listen_host (with legacy migration from webapp_host)
    let listen_host = if let Ok(v) = std::env::var("HAPIR_LISTEN_HOST") {
        if settings.listen_host.is_none() {
            settings.listen_host = Some(v.clone());
            needs_save = true;
        }
        v
    } else if let Some(ref v) = settings.listen_host {
        v.clone()
    } else if let Some(v) = settings.webapp_host.take() {
        settings.listen_host = Some(v.clone());
        needs_save = true;
        v
    } else {
        "127.0.0.1".into()
    };

    // listen_port (with legacy migration from webapp_port)
    let listen_port = if let Ok(v) = std::env::var("HAPIR_LISTEN_PORT") {
        let port: u16 = v
            .parse()
            .map_err(|_| anyhow::anyhow!("HAPIR_LISTEN_PORT must be a valid port"))?;
        if settings.listen_port.is_none() {
            settings.listen_port = Some(port);
            needs_save = true;
        }
        port
    } else if let Some(v) = settings.listen_port {
        v
    } else if let Some(v) = settings.webapp_port {
        settings.listen_port = Some(v);
        settings.webapp_port = None;
        needs_save = true;
        v
    } else {
        3006
    };

    // public_url (with legacy migration from webapp_url)
    let public_url = if let Ok(v) = std::env::var("HAPIR_PUBLIC_URL") {
        if settings.public_url.is_none() {
            settings.public_url = Some(v.clone());
            needs_save = true;
        }
        v
    } else if let Some(ref v) = settings.public_url {
        v.clone()
    } else if let Some(v) = settings.webapp_url.take() {
        settings.public_url = Some(v.clone());
        needs_save = true;
        v
    } else {
        format!("http://localhost:{listen_port}")
    };

    // cors_origins
    let cors_origins = if let Ok(v) = std::env::var("CORS_ORIGINS") {
        let origins = parse_cors_origins(&v);
        if settings.cors_origins.is_none() {
            settings.cors_origins = Some(origins.clone());
            needs_save = true;
        }
        origins
    } else if let Some(ref v) = settings.cors_origins {
        v.clone()
    } else {
        derive_cors_origins(&public_url)
    };

    if needs_save {
        write_settings(&settings_path, &settings)?;
    }

    Ok(ServerSettingsResult {
        settings: ServerSettings {
            telegram_bot_token,
            telegram_notification,
            listen_host,
            listen_port,
            public_url,
            cors_origins,
        },
        saved_to_file: needs_save,
    })
}
