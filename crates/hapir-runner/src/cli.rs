use anyhow::bail;
use hapir_infra::config::Configuration;
use hapir_infra::persistence;
use hapir_infra::process::spawn_runner_background;

use crate::control_client;
use crate::orchestrator;

pub async fn run(action: Option<&str>) -> anyhow::Result<()> {
    let mut config = Configuration::create()?;
    config.load_with_settings()?;

    match action {
        Some("start") => start_background(&config).await,
        Some("start-sync") => orchestrator::start_sync(&config).await,
        Some("stop") => stop(&config).await,
        Some("status") => status(&config).await,
        Some("logs") => show_logs(&config),
        Some("list") => list(&config).await,
        _ => {
            eprintln!("Unknown runner action. Run 'hapir runner --help' for usage.");
            Ok(())
        }
    }
}

async fn start_background(config: &Configuration) -> anyhow::Result<()> {
    if config.cli_api_token.is_empty() {
        bail!(
            "CLI_API_TOKEN is not configured. Run 'hapir auth login' or set the CLI_API_TOKEN environment variable."
        );
    }

    if let Some(port) =
        control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
            .await
    {
        eprintln!("Runner is already running on port {port}");
        return Ok(());
    }

    eprintln!("Starting runner in background...");
    spawn_runner_background()?;
    eprintln!("Runner started in background");
    Ok(())
}

async fn stop(config: &Configuration) -> anyhow::Result<()> {
    match control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
        .await
    {
        Some(port) => {
            control_client::stop_runner(port).await?;
            eprintln!("Runner stop requested.");
        }
        None => {
            eprintln!("Runner is not running.");
        }
    }
    Ok(())
}

async fn status(config: &Configuration) -> anyhow::Result<()> {
    match control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
        .await
    {
        Some(port) => {
            let state = persistence::read_runner_state(&config.runner_state_file);
            if let Some(state) = state {
                eprintln!("Runner is running:");
                eprintln!("  PID:        {}", state.pid);
                eprintln!("  Port:       {port}");
                eprintln!("  Started:    {}", state.start_time);
                eprintln!("  Version:    {}", state.started_with_cli_version);
            } else {
                eprintln!("Runner is running on port {port}");
            }
        }
        None => {
            eprintln!("Runner is not running.");
        }
    }
    Ok(())
}

async fn list(config: &Configuration) -> anyhow::Result<()> {
    match control_client::check_runner_alive(&config.runner_state_file, &config.runner_lock_file)
        .await
    {
        Some(port) => {
            let sessions = control_client::list_sessions(port).await?;
            if sessions.is_empty() {
                eprintln!("No active sessions.");
            } else {
                eprintln!("{} active session(s):", sessions.len());
                for s in &sessions {
                    eprintln!(
                        "  {} (pid: {}, by: {})",
                        s.happy_session_id, s.pid, s.started_by,
                    );
                }
            }
        }
        None => {
            eprintln!("Runner is not running.");
        }
    }
    Ok(())
}

fn show_logs(config: &Configuration) -> anyhow::Result<()> {
    if let Some(state) = persistence::read_runner_state(&config.runner_state_file)
        && let Some(ref log_path) = state.runner_log_path
    {
        let path = std::path::Path::new(log_path);
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(50);
            for line in &lines[start..] {
                println!("{line}");
            }
            return Ok(());
        }
        eprintln!("Log file not found: {log_path}");
        return Ok(());
    }
    eprintln!("No runner logs available. Is the runner running?");
    Ok(())
}
