use std::io::{self, BufRead, Write};

use anyhow::{Result, bail};
use atty::Stream;
use tracing::{debug, info, warn};

use hapir_infra::config::Configuration;
use hapir_infra::persistence;
use hapir_infra::process::spawn_runner_background;
use hapir_runner::control_client;

/// Initialize the API token from settings file, falling back to interactive
/// prompt if running in a TTY.
pub fn initialize_token(config: &mut Configuration) -> Result<()> {
    config.load_with_settings()?;

    if !config.cli_api_token.is_empty() {
        debug!("token already set (env or settings)");
        return Ok(());
    }

    if !atty::is(Stream::Stdin) {
        bail!(
            "CLI_API_TOKEN is not set and stdin is not a TTY.\n\
             Run 'hapir auth login' or set the CLI_API_TOKEN environment variable."
        );
    }

    eprintln!("No API token found. Please enter your CLI_API_TOKEN.");
    eprintln!(
        "(You can find it in the hub server startup logs or ~/.hapir/settings.json on the server)"
    );
    eprint!("CLI_API_TOKEN: ");
    io::stderr().flush()?;

    let stdin = io::stdin();
    let token = stdin
        .lock()
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no input"))??;
    let token = token.trim().to_string();

    if token.is_empty() {
        bail!("token cannot be empty");
    }

    persistence::update_settings(&config.settings_file, |s| {
        s.cli_api_token = Some(token.clone());
    })?;

    config.cli_api_token = token;
    Ok(())
}

/// Register (or confirm) the machine with the hub API. Returns the machine_id.
pub async fn auth_and_setup_machine(config: &Configuration) -> Result<String> {
    hapir_infra::auth::auth_and_setup_machine(config).await
}

/// Ensure the runner process is alive, auto-starting it if needed.
pub async fn ensure_runner(config: &Configuration, _machine_id: String) -> Result<Option<u16>> {
    if let Some(port) =
        control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
            .await
    {
        debug!(port, "runner already running");
        return Ok(Some(port));
    }

    let settings = persistence::read_settings(&config.settings_file)?;
    if settings.runner_auto_start_when_running_happy == Some(false) {
        warn!("runner is not running and auto-start is disabled");
        return Ok(None);
    }

    debug!("runner not running, attempting auto-start");
    if let Err(e) = spawn_runner_background() {
        warn!("failed to auto-start runner: {e}");
        return Ok(None);
    }

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    if let Some(port) =
        control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
            .await
    {
        info!(port, "runner auto-started successfully");
        return Ok(Some(port));
    }

    warn!("runner was spawned but is not yet responding");
    Ok(None)
}

/// Full initialization sequence shared by all agent commands:
/// token -> machine registration -> runner check.
pub async fn full_init(config: &mut Configuration) -> Result<Option<u16>> {
    initialize_token(config)?;
    let machine_id = auth_and_setup_machine(config).await?;
    ensure_runner(config, machine_id).await
}
