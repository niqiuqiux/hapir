use crate::control_server;
use crate::types::ShutdownSource;
use control_server::RunnerState;
use hapir_infra::auth;
use hapir_infra::config::Configuration;
use hapir_infra::handlers;
use hapir_infra::persistence;
use hapir_infra::utils::machine::build_machine_metadata;
use hapir_infra::ws::machine_client::WsMachineClient;
use hapir_shared::schemas::{MachineRunnerState, MachineRunnerStatus};
use serde_json::json;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use hapir_infra::utils::process::spawn_runner_background;

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

pub async fn start_sync(config: &Configuration) -> anyhow::Result<()> {
    // Startup malfunction timeout
    let malfunction_handle = tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        warn!("startup malfunctioned, forcing exit with code 1");
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(1);
    });

    if config.cli_api_token.is_empty() {
        anyhow::bail!(
            "CLI_API_TOKEN is not configured. Run 'hapir auth login' or set the CLI_API_TOKEN environment variable."
        );
    }

    // Check if runner is already running with matching version
    match crate::control_client::is_runner_running_current_version(
        &config.runner_state_file,
        &config.runner_lock_file,
    )
    .await
    {
        Some(true) => {
            eprintln!("Runner already running with matching version");
            std::process::exit(0);
        }
        Some(false) => {
            info!("runner version mismatch detected, stopping old runner");
            crate::control_client::stop_runner_gracefully(
                &config.runner_state_file,
                &config.runner_lock_file,
            )
            .await;
        }
        None => {}
    }

    let _lock = persistence::acquire_runner_lock(&config.runner_lock_file, 5)
        .ok_or_else(|| anyhow::anyhow!("another runner instance is already starting"))?;

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<ShutdownSource>(1);

    let state = RunnerState::new_mpsc(shutdown_tx.clone());
    let port = control_server::start(state.clone()).await?;

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

    malfunction_handle.abort();

    info!(port = port, pid = std::process::id(), "runner started");
    eprintln!("Runner started on port {port} (pid {})", std::process::id());

    // PLACEHOLDER_HUB_WS

    let ws_holder: Arc<OnceLock<Arc<WsMachineClient>>> = Arc::new(OnceLock::new());

    match connect_to_hub(config, port, start_time_ms, &state).await {
        Ok(ws) => {
            let _ = ws_holder.set(ws);
        }
        Err(e) => {
            warn!("hub connection failed, running in local-only mode: {e}");
            let holder = ws_holder.clone();
            let cfg = config.clone();
            let rs = state.clone();
            tokio::spawn(async move {
                let retry_interval = Duration::from_secs(30);
                loop {
                    tokio::time::sleep(retry_interval).await;
                    if holder.get().is_some() {
                        break;
                    }
                    debug!("retrying hub connection...");
                    match connect_to_hub(&cfg, port, start_time_ms, &rs).await {
                        Ok(ws) => {
                            info!("hub connection established (deferred)");
                            let _ = holder.set(ws);
                            break;
                        }
                        Err(e) => {
                            debug!("hub reconnection attempt failed: {e}");
                        }
                    }
                }
            });
        }
    };

    // PLACEHOLDER_HEARTBEAT

    let heartbeat_handle = {
        let runner_state_clone = state.clone();
        let state_file = config.runner_state_file.clone();
        let started_mtime_ms = cli_mtime_ms;
        let heartbeat_ms: u64 = std::env::var("HAPI_RUNNER_HEARTBEAT_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60_000);
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(heartbeat_ms));
            loop {
                interval.tick().await;

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

                if let Some(file_state) = persistence::read_runner_state(&state_file)
                    && file_state.pid != std::process::id()
                {
                    warn!(
                        "another runner took over (pid {}), shutting down",
                        file_state.pid
                    );
                    if let Some(tx) = runner_state_clone.shutdown_tx_mpsc.lock().await.as_ref() {
                        let _ = tx.send(ShutdownSource::Exception).await;
                    }
                    return;
                }

                if let Some(started) = started_mtime_ms
                    && let Some(current) = get_cli_mtime_ms()
                    && current != started
                {
                    warn!(
                        "CLI binary changed (mtime {started} -> {current}), triggering self-restart"
                    );
                    if let Err(e) = spawn_runner_background() {
                        warn!("failed to spawn new runner: {e}");
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    if let Some(tx) = runner_state_clone.shutdown_tx_mpsc.lock().await.as_ref() {
                        let _ = tx.send(ShutdownSource::Exception).await;
                    }
                    return;
                }

                if let Some(mut file_state) = persistence::read_runner_state(&state_file) {
                    file_state.last_heartbeat = Some(format_locale_time());
                    let _ = persistence::write_runner_state(&state_file, &file_state);
                }
            }
        }))
    };

    // PLACEHOLDER_SIGNALS

    let signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
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

    let shutdown_source = shutdown_rx
        .recv()
        .await
        .unwrap_or(ShutdownSource::Exception);
    info!(source = ?shutdown_source, "runner shutting down");

    if let Some(ws) = ws_holder.get() {
        let shutdown_at = epoch_ms();
        let source_str = shutdown_source.as_str().to_string();
        if let Err(e) = ws
            .update_runner_state(|mut s| {
                s.status = MachineRunnerStatus::ShuttingDown;
                s.shutdown_requested_at = Some(shutdown_at);
                s.shutdown_source = Some(source_str);
                s
            })
            .await
        {
            warn!("failed to update shutdown state on hub: {e}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let ended_ids = control_server::stop_all_sessions(&state).await;
    if !ended_ids.is_empty() {
        info!(count = ended_ids.len(), "killed tracked session processes");
        if let Some(ws) = ws_holder.get() {
            for sid in &ended_ids {
                ws.send_session_end(sid).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    if let Some(handle) = heartbeat_handle {
        handle.abort();
    }

    if let Some(ws) = ws_holder.get() {
        ws.shutdown().await;
    }

    persistence::clear_runner_state(&config.runner_state_file, &config.runner_lock_file);
    eprintln!("Runner stopped.");
    Ok(())
}

// PLACEHOLDER_CONNECT_TO_HUB

/// Connect to the hub via WebSocket, register RPC handlers, and send initial state.
async fn connect_to_hub(
    config: &Configuration,
    http_port: u16,
    start_time_ms: i64,
    runner_state: &RunnerState,
) -> anyhow::Result<Arc<WsMachineClient>> {
    let initial_runner_state = MachineRunnerState {
        status: MachineRunnerStatus::Offline,
        pid: std::process::id(),
        http_port,
        started_at: start_time_ms,
        shutdown_requested_at: None,
        shutdown_source: None,
    };

    let machine_id = {
        let max_attempts: u32 = 60;
        let min_delay_ms: u64 = 1000;
        let max_delay_ms: u64 = 30000;
        let mut attempt: u32 = 0;
        loop {
            match auth::auth_and_setup_machine_with_state(config, Some(&initial_runner_state)).await
            {
                Ok(id) => break id,
                Err(e) => {
                    attempt += 1;
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
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    };

    // PLACEHOLDER_WS_SETUP

    let ws = Arc::new(WsMachineClient::new(
        &config.api_url,
        &config.cli_api_token,
        &machine_id,
    ));

    let rs = runner_state.clone();
    ws.register_rpc("spawn-happy-session", move |data| {
        let rs = rs.clone();
        Box::pin(async move {
            let req: crate::types::SpawnSessionRequest =
                serde_json::from_value(data).unwrap_or_else(|_| {
                    crate::types::SpawnSessionRequest {
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
            let session_id = data.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
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
    })
    .await;

    // PLACEHOLDER_MORE_RPC

    let rs = runner_state.clone();
    ws.register_rpc("stop-runner", move |_data| {
        let rs = rs.clone();
        Box::pin(async move {
            let rs2 = rs.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                control_server::do_stop_runner(&rs2, ShutdownSource::HapiApp).await;
            });
            json!({"message": "Runner stop request acknowledged"})
        })
    })
    .await;

    ws.register_rpc("path-exists", move |data| {
        Box::pin(async move {
            let paths = data
                .get("paths")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let unique_paths: std::collections::HashSet<String> = paths
                .iter()
                .filter_map(|p| p.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let handles: Vec<_> = unique_paths
                .into_iter()
                .map(|path_str| {
                    tokio::spawn(async move {
                        let is_dir = tokio::fs::metadata(&path_str)
                            .await
                            .map(|m| m.is_dir())
                            .unwrap_or(false);
                        (path_str, is_dir)
                    })
                })
                .collect();
            let mut exists = serde_json::Map::new();
            for handle in handles {
                if let Ok((path_str, is_dir)) = handle.await {
                    exists.insert(path_str, json!(is_dir));
                }
            }
            json!({ "exists": exists })
        })
    })
    .await;

    {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".to_string());
        handlers::register_infra_handlers(ws.as_ref(), &cwd).await;
    }

    ws.connect(Duration::from_secs(10)).await?;

    let meta = build_machine_metadata(&config.home_dir);
    ws.update_metadata(|_| serde_json::to_value(&meta).unwrap_or(json!({})))
        .await
        .map_err(|e| warn!("failed to send machine metadata: {e}"))
        .ok();

    ws.update_runner_state(|mut s| {
        s.status = MachineRunnerStatus::Running;
        s.pid = std::process::id();
        s.http_port = http_port;
        s.started_at = start_time_ms;
        s
    })
    .await
    .map_err(|e| warn!("failed to send runner state: {e}"))
    .ok();

    info!(machine_id = %machine_id, "connected to hub via WebSocket");
    Ok(ws)
}

/// Current time as epoch milliseconds.
fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Format current time as a human-readable local string (MM/DD/YYYY, HH:MM:SS AM/PM).
fn format_locale_time() -> String {
    chrono::Local::now()
        .format("%m/%d/%Y, %I:%M:%S %p")
        .to_string()
}
