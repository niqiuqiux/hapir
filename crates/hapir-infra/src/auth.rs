use anyhow::{Context, Result};
use tracing::info;

use crate::api::ApiClient;
use crate::config::Configuration;
use crate::machine::build_machine_metadata;
use crate::persistence;

/// Register (or confirm) the machine with the hub API. Returns the machine_id.
pub async fn auth_and_setup_machine(config: &Configuration) -> Result<String> {
    auth_and_setup_machine_with_state(config, None).await
}

/// Register (or confirm) the machine with the hub API, optionally sending
/// initial runner state. Returns the machine_id.
pub async fn auth_and_setup_machine_with_state(
    config: &Configuration,
    runner_state: Option<&serde_json::Value>,
) -> Result<String> {
    let api = ApiClient::new(config)?;
    let settings = persistence::read_settings(&config.settings_file)?;

    let machine_id = if let Some(ref id) = settings.machine_id {
        id.clone()
    } else {
        let id = format!("mach_{}", uuid::Uuid::new_v4());
        persistence::update_settings(&config.settings_file, |s| {
            s.machine_id = Some(id.clone());
        })?;
        id
    };

    info!(machine_id = %machine_id, "registering machine");
    let machine_meta = build_machine_metadata(config);
    api.get_or_create_machine(
        &machine_id,
        &serde_json::to_value(&machine_meta).unwrap_or(serde_json::json!({})),
        runner_state,
    )
    .await
    .context("failed to register machine with hub")?;

    persistence::update_settings(&config.settings_file, |s| {
        s.machine_id_confirmed_by_server = Some(true);
    })?;

    Ok(machine_id)
}
