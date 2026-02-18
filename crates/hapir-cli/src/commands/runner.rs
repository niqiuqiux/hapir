use std::sync::Arc;

use serde_json::json;
use tracing::{info, warn};

use crate::agent::session_factory::build_machine_metadata;
use crate::commands::common;
use crate::config::Configuration;
use crate::persistence;
use crate::runner::types::ShutdownSource;
use crate::runner::{control_client, control_server};
use crate::ws::machine_client::WsMachineClient;

/// Get the mtime of the current CLI executable in milliseconds.
pub fn get_cli_mtime_ms() -> Option<i64> {
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(&exe).ok()?;
    let mtime = meta.modified().ok()?;
    let ms = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(ms)
}

pub async fn run(action: Option<&str>) -> anyhow::Result<()> {
    let mut config = Configuration::create()?;
    config.load_with_settings()?;

    match action {
        Some("start") => start_background(&config).await,
        Some("start-sync") => start_sync(&config).await,
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
    // Validate required configuration before starting
    if config.cli_api_token.is_empty() {
        anyhow::bail!(
            "CLI_API_TOKEN is not configured. Run 'hapir auth login' or set the CLI_API_TOKEN environment variable."
        );
    }

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
    // Startup malfunction timeout — force-exit if startup doesn't complete in 1s (matches TS)
    let malfunction_handle = tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        warn!("startup malfunctioned, forcing exit with code 1");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::process::exit(1);
    });

    // Validate required configuration before starting
    if config.cli_api_token.is_empty() {
        anyhow::bail!(
            "CLI_API_TOKEN is not configured. Run 'hapir auth login' or set the CLI_API_TOKEN environment variable."
        );
    }

    // Check if runner is already running with matching version (matches TS run.ts lines 97-107)
    match control_client::is_runner_running_current_version(
        &config.runner_state_file,
        &config.runner_lock_file,
    ).await {
        Some(true) => {
            eprintln!("Runner already running with matching version");
            std::process::exit(0);
        }
        Some(false) => {
            info!("runner version mismatch detected, stopping old runner");
            control_client::stop_runner_gracefully(
                &config.runner_state_file,
                &config.runner_lock_file,
            ).await;
        }
        None => {
            // No runner running, proceed with startup
        }
    }

    // Acquire runner lock
    let _lock = persistence::acquire_runner_lock(&config.runner_lock_file, 5)
        .ok_or_else(|| anyhow::anyhow!("another runner instance is already starting"))?;

    // Create shutdown channel
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<ShutdownSource>(1);

    // Start control server
    let state = control_server::RunnerState::new_mpsc(shutdown_tx.clone());
    let port = control_server::start(state.clone()).await?;

    // Write runner state
    let start_time_ms = epoch_ms();
    let cli_mtime_ms = get_cli_mtime_ms();
    let runner_local_state = persistence::RunnerLocalState {
        pid: std::process::id(),
        http_port: port,
        start_time: format_locale_time(),
        started_with_cli_version: env!("CARGO_PKG_VERSION").to_string(),
        started_with_cli_mtime_ms: cli_mtime_ms.map(|v| v as f64),
        last_heartbeat: None,
        runner_log_path: None,
    };
    persistence::write_runner_state(&config.runner_state_file, &runner_local_state)?;

    // Cancel startup malfunction timeout — startup completed successfully
    malfunction_handle.abort();

    info!(port = port, pid = std::process::id(), "runner started");
    eprintln!("Runner started on port {port} (pid {})", std::process::id());

    // --- Hub WebSocket connectivity (best-effort) ---
    let ws_machine: Option<Arc<WsMachineClient>> = match connect_to_hub(config, port, start_time_ms, &state).await {
        Ok(ws) => Some(ws),
        Err(e) => {
            warn!("hub connection failed, running in local-only mode: {e}");
            None
        }
    };

    // Spawn heartbeat task
    let heartbeat_handle = {
        let runner_state_clone = state.clone();
        let state_file = config.runner_state_file.clone();
        let started_mtime_ms = cli_mtime_ms;
        let heartbeat_ms: u64 = std::env::var("HAPI_RUNNER_HEARTBEAT_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60_000);
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(heartbeat_ms));
            loop {
                interval.tick().await;

                // Prune dead sessions
                {
                    let mut sessions = runner_state_clone.sessions.lock().await;
                    sessions.retain(|_id, s| {
                        if let Some(pid) = s.pid {
                            persistence::is_process_alive(pid)
                        } else {
                            true
                        }
                    });
                }

                // PID ownership check
                if let Some(file_state) = persistence::read_runner_state(&state_file)
                    && file_state.pid != std::process::id()
                {
                    warn!("another runner took over (pid {}), shutting down", file_state.pid);
                    if let Some(tx) = runner_state_clone.shutdown_tx_mpsc.lock().await.as_ref() {
                        let _ = tx.send(ShutdownSource::Exception).await;
                    }
                    return;
                }

                // Version mismatch detection — if CLI binary changed, self-restart
                if let Some(started) = started_mtime_ms
                    && let Some(current) = get_cli_mtime_ms()
                    && current != started
                {
                    warn!("CLI binary changed (mtime {started} -> {current}), triggering self-restart");
                    // Spawn new runner
                    if let Err(e) = common::spawn_runner_background() {
                        warn!("failed to spawn new runner: {e}");
                    }
                    // Give the new runner time to start and take over
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    if let Some(tx) = runner_state_clone.shutdown_tx_mpsc.lock().await.as_ref() {
                        let _ = tx.send(ShutdownSource::Exception).await;
                    }
                    return;
                }

                // Write heartbeat to local state file
                if let Some(mut file_state) = persistence::read_runner_state(&state_file) {
                    file_state.last_heartbeat = Some(format_locale_time());
                    let _ = persistence::write_runner_state(&state_file, &file_state);
                }
            }
        }))
    };

    // Setup OS signal handlers
    let signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");
            let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
            tokio::select! {
                _ = sigint.recv() => {
                    info!("received SIGINT");
                }
                _ = sigterm.recv() => {
                    info!("received SIGTERM");
                }
            }
            let _ = signal_tx.send(ShutdownSource::OsSignal).await;
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("received Ctrl+C");
            let _ = signal_tx.send(ShutdownSource::OsSignal).await;
        }
    });

    // Wait for shutdown signal
    let shutdown_source = shutdown_rx.recv().await.unwrap_or(ShutdownSource::Exception);
    info!(source = ?shutdown_source, "runner shutting down");

    // Graceful shutdown: update hub state
    if let Some(ref ws) = ws_machine {
        let shutdown_at = epoch_ms();
        let source_str = shutdown_source.as_str();
        if let Err(e) = ws.update_runner_state(|mut s| {
            s["status"] = json!("shutting-down");
            s["shutdownRequestedAt"] = json!(shutdown_at);
            s["shutdownSource"] = json!(source_str);
            s
        }).await {
            warn!("failed to update shutdown state on hub: {e}");
        }
        // Give time for the update to send
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Kill all tracked session child processes and notify hub
    let ended_ids = control_server::stop_all_sessions(&state).await;
    if !ended_ids.is_empty() {
        info!(count = ended_ids.len(), "killed tracked session processes");
        if let Some(ref ws) = ws_machine {
            for sid in &ended_ids {
                ws.send_session_end(sid).await;
            }
            // Give time for session-end events to send
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    if let Some(handle) = heartbeat_handle {
        handle.abort();
    }

    if let Some(ws) = ws_machine {
        ws.shutdown().await;
    }

    // Clean up state
    persistence::clear_runner_state(&config.runner_state_file, &config.runner_lock_file);
    eprintln!("Runner stopped.");
    Ok(())
}

/// Connect to the hub via WebSocket, register RPC handlers, and send initial state.
async fn connect_to_hub(
    config: &Configuration,
    http_port: u16,
    start_time_ms: i64,
    runner_state: &control_server::RunnerState,
) -> anyhow::Result<Arc<WsMachineClient>> {
    let initial_runner_state = json!({
        "status": "offline",
        "pid": std::process::id(),
        "httpPort": http_port,
        "startedAt": start_time_ms,
    });

    // Retry machine registration with exponential backoff (matches TS: withRetry, 60 attempts, 1-30s)
    let machine_id = {
        let max_attempts: u32 = 60;
        let min_delay_ms: u64 = 1000;
        let max_delay_ms: u64 = 30000;
        let mut attempt: u32 = 0;
        loop {
            match common::auth_and_setup_machine_with_state(config, Some(&initial_runner_state)).await {
                Ok(id) => break id,
                Err(e) => {
                    attempt += 1;
                    // Only retry on connection-like errors
                    let is_retryable = {
                        let msg = e.to_string().to_lowercase();
                        msg.contains("connection refused")
                            || msg.contains("timed out")
                            || msg.contains("dns")
                            || msg.contains("network")
                            || msg.contains("reset")
                            || msg.contains("status: 5")
                    };
                    if !is_retryable || attempt >= max_attempts {
                        return Err(e);
                    }
                    let exp_delay = min_delay_ms * 2u64.pow(attempt - 1);
                    let capped = exp_delay.min(max_delay_ms);
                    let jitter = (rand::random::<f64>() * 0.3 * capped as f64) as u64;
                    let delay = capped + jitter;
                    warn!(
                        attempt = attempt,
                        delay_ms = delay,
                        "failed to register machine, retrying: {e}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
            }
        }
    };

    let ws = Arc::new(WsMachineClient::new(
        &config.api_url,
        &config.cli_api_token,
        &machine_id,
    ));

    // Register RPC handlers
    let rs = runner_state.clone();
    ws.register_rpc("spawn-happy-session", move |data| {
        let rs = rs.clone();
        Box::pin(async move {
            let req: crate::runner::types::SpawnSessionRequest =
                serde_json::from_value(data).unwrap_or_else(|_| {
                    crate::runner::types::SpawnSessionRequest {
                        directory: String::new(),
                        session_id: None,
                        session_type: None,
                        worktree_name: None,
                        agent: None,
                        approved_new_directory_creation: None,
                        model: None,
                        yolo: None,
                        token: None,
                        resume_session_id: None,
                    }
                });
            // Match TS: validate directory is present
            if req.directory.is_empty() {
                return json!({ "type": "error", "error": "Directory is required" });
            }
            let resp = control_server::do_spawn_session(&rs, req).await;
            match resp.r#type.as_str() {
                "success" => json!({ "type": "success", "sessionId": resp.session_id }),
                "requestToApproveDirectoryCreation" => json!({ "type": "requestToApproveDirectoryCreation", "directory": resp.directory }),
                _ => json!({ "type": "error", "error": resp.error }),
            }
        })
    }).await;

    let rs = runner_state.clone();
    ws.register_rpc("stop-session", move |data| {
        let rs = rs.clone();
        Box::pin(async move {
            let session_id = data.get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if session_id.is_empty() {
                return json!({"error": "Session ID is required"});
            }
            let result = control_server::do_stop_session(&rs, session_id).await;
            if result.get("success").and_then(|v| v.as_bool()) == Some(true) {
                json!({"message": "Session stopped"})
            } else {
                json!({"error": "Session not found or failed to stop"})
            }
        })
    }).await;

    let rs = runner_state.clone();
    ws.register_rpc("stop-runner", move |_data| {
        let rs = rs.clone();
        Box::pin(async move {
            // Delay 100ms so the RPC response gets sent before shutdown
            let rs2 = rs.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                control_server::do_stop_runner(&rs2, ShutdownSource::HapiApp).await;
            });
            json!({"message": "Runner stop request acknowledged"})
        })
    }).await;

    // Register path-exists RPC
    ws.register_rpc("path-exists", move |data| {
        Box::pin(async move {
            let paths = data.get("paths")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            // Deduplicate paths (matches TS: new Set())
            let unique_paths: std::collections::HashSet<String> = paths.iter()
                .filter_map(|p| p.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            // Parallel stat checks (matches TS: Promise.all)
            let handles: Vec<_> = unique_paths.into_iter().map(|path_str| {
                tokio::spawn(async move {
                    let is_dir = tokio::fs::metadata(&path_str).await
                        .map(|m| m.is_dir())
                        .unwrap_or(false);
                    (path_str, is_dir)
                })
            }).collect();
            let mut exists = serde_json::Map::new();
            for handle in handles {
                if let Ok((path_str, is_dir)) = handle.await {
                    exists.insert(path_str, json!(is_dir));
                }
            }
            json!({ "exists": exists })
        })
    }).await;

    // Register common RPC handlers (bash, files, directories, git, ripgrep)
    // Matches TS: registerCommonHandlers(this.rpcHandlerManager, process.cwd())
    {
        let rpc_mgr = crate::rpc::RpcHandlerManager::new("_tmp_");
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".to_string());
        crate::handlers::register_all_handlers(&rpc_mgr, &cwd).await;
        for (method, handler) in rpc_mgr.drain_handlers().await {
            ws.register_rpc(&method, move |params| handler(params)).await;
        }
    }

    // Connect WebSocket and wait for the connection to be established
    ws.connect_and_wait(std::time::Duration::from_secs(10)).await?;

    // Send machine metadata
    let meta = build_machine_metadata(config);
    ws.update_metadata(|_| serde_json::to_value(&meta).unwrap_or(json!({}))).await
        .map_err(|e| warn!("failed to send machine metadata: {e}")).ok();

    // Send runner state
    ws.update_runner_state(|_| json!({
        "status": "running",
        "pid": std::process::id(),
        "httpPort": http_port,
        "startedAt": start_time_ms,
    })).await
        .map_err(|e| warn!("failed to send runner state: {e}")).ok();

    info!(machine_id = %machine_id, "connected to hub via WebSocket");
    Ok(ws)
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
                        "  {} (pid: {}, by: {})",
                        s.happy_session_id,
                        s.pid,
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
    if let Some(state) = persistence::read_runner_state(&config.runner_state_file)
        && let Some(ref log_path) = state.runner_log_path
    {
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
    eprintln!("No runner logs available. Is the runner running?");
    Ok(())
}

/// Current time as epoch milliseconds.
fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Format current time as a human-readable string (matches TS `new Date().toLocaleString()`).
fn format_locale_time() -> String {
    std::process::Command::new("date")
        .arg("+%m/%d/%Y, %I:%M:%S %p")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| epoch_ms().to_string())
}
