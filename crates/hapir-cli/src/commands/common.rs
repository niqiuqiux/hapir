use std::io::{self, BufRead, Write};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use atty::Stream;
use tracing::{debug, info, warn};

use crate::agent::session_factory::build_machine_metadata;
use crate::api::ApiClient;
use crate::config::Configuration;
use crate::persistence;
use crate::runner::control_client;

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

    // Persist for future runs
    persistence::update_settings(&config.settings_file, |s| {
        s.cli_api_token = Some(token.clone());
    })?;

    config.cli_api_token = token;
    Ok(())
}

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
        let id = uuid::Uuid::new_v4().to_string();
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

    // Mark confirmed
    persistence::update_settings(&config.settings_file, |s| {
        s.machine_id_confirmed_by_server = Some(true);
    })?;

    Ok(machine_id)
}

/// Ensure the runner process is alive, auto-starting it if needed.
pub async fn ensure_runner(config: &Configuration, machine_id: String) -> Result<Option<u16>> {
    if let Some(port) =
        control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
            .await
    {
        debug!(port, "runner already running");
        return Ok(Some(port));
    }

    // Check settings to see if auto-start is enabled
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

    // Give the runner a moment to bind its port and write state
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Re-check if it came up
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

/// Spawn the runner as a fully detached background process.
///
/// Runs `<current_exe> runner start-sync` with stdin/stdout/stderr
/// redirected to null and (on Unix) a new session via `setsid`.
pub fn spawn_runner_background() -> Result<()> {
    let exe = std::env::current_exe().context("failed to determine current executable path")?;
    debug!(exe = %exe.display(), "spawning runner background process");

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["runner", "start-sync"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Safety: setsid() creates a new session so the child is fully
        // detached from the parent's process group and terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()
        .context("failed to spawn runner background process")?;
    Ok(())
}
