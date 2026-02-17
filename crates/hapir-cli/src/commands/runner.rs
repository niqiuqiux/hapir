use tracing::info;

use crate::commands::common;
use crate::config::Configuration;
use crate::persistence;
use crate::runner::{control_client, control_server};

pub async fn run(action: Option<&str>) -> anyhow::Result<()> {
    let config = Configuration::create()?;

    match action {
        Some("start") => start_background(&config).await,
        Some("start-sync") => start_sync(&config).await,
        Some("stop") => stop(&config).await,
        Some("status") => status(&config).await,
        Some("logs") => show_logs(&config),
        Some("list") => list(&config).await,
        _ => {
            eprintln!("Unknown runner action. Run 'hapi runner --help' for usage.");
            Ok(())
        }
    }
}

async fn start_background(config: &Configuration) -> anyhow::Result<()> {
    // Check if already running
    if let Some(port) = control_client::check_runner_alive(
        &config.runner_state_file,
        &config.runner_lock_file,
    )
    .await
    {
        eprintln!("Runner is already running on port {port}");
        return Ok(());
    }

    eprintln!("Starting runner in background...");
    common::spawn_runner_background()?;
    eprintln!("Runner started in background");
    Ok(())
}

async fn start_sync(config: &Configuration) -> anyhow::Result<()> {
    // Acquire runner lock
    let _lock = persistence::acquire_runner_lock(&config.runner_lock_file, 5)
        .ok_or_else(|| anyhow::anyhow!("another runner instance is already starting"))?;

    // Create shutdown channel
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    // Start control server
    let state = control_server::RunnerState::new(shutdown_tx);
    let port = control_server::start(state).await?;

    // Write runner state
    let runner_state = persistence::RunnerLocalState {
        pid: std::process::id(),
        http_port: port,
        start_time: chrono_now(),
        started_with_cli_version: env!("CARGO_PKG_VERSION").to_string(),
        started_with_cli_mtime_ms: None,
        last_heartbeat: None,
        runner_log_path: None,
    };
    persistence::write_runner_state(&config.runner_state_file, &runner_state)?;

    info!(port = port, pid = std::process::id(), "runner started");
    eprintln!("Runner started on port {port} (pid {})", std::process::id());

    // Wait for shutdown signal
    let _ = shutdown_rx.await;
    info!("runner shutting down");

    // Clean up state
    persistence::clear_runner_state(&config.runner_state_file, &config.runner_lock_file);
    eprintln!("Runner stopped.");
    Ok(())
}

async fn stop(config: &Configuration) -> anyhow::Result<()> {
    match control_client::check_runner_alive(
        &config.runner_state_file,
        &config.runner_lock_file,
    )
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
    match control_client::check_runner_alive(
        &config.runner_state_file,
        &config.runner_lock_file,
    )
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
    match control_client::check_runner_alive(
        &config.runner_state_file,
        &config.runner_lock_file,
    )
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
                        "  {} (pid: {}, dir: {}, by: {})",
                        s.session_id,
                        s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                        s.directory,
                        s.started_by,
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
    if let Some(state) = persistence::read_runner_state(&config.runner_state_file) {
        if let Some(ref log_path) = state.runner_log_path {
            let path = std::path::Path::new(log_path);
            if path.exists() {
                let content = std::fs::read_to_string(path)?;
                // Show last 50 lines
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
    }
    eprintln!("No runner logs available. Is the runner running?");
    Ok(())
}

/// Simple ISO-8601 timestamp without pulling in chrono.
fn chrono_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s-since-epoch", dur.as_secs())
}
