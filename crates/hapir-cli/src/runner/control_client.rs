use std::path::Path;

use anyhow::{bail, Result};
use tracing::{debug, warn};

use crate::persistence;

use super::types::*;

/// HTTP timeout for runner control requests (configurable via HAPI_RUNNER_HTTP_TIMEOUT env var, in milliseconds).
fn http_timeout() -> std::time::Duration {
    let ms: u64 = std::env::var("HAPI_RUNNER_HTTP_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    std::time::Duration::from_millis(ms)
}

/// Check if the runner is alive by reading state and pinging.
/// Returns the control server port if alive.
pub async fn check_runner_alive(
    state_path: &Path,
    lock_path: &Path,
) -> Option<u16> {
    let state = persistence::read_runner_state(state_path)?;
    let port = state.http_port;
    let pid = state.pid;

    // Check if process is alive
    if !persistence::is_process_alive(pid) {
        // Stale state, clean up
        persistence::clear_runner_state(state_path, lock_path);
        return None;
    }

    // Try to ping the control server
    let client = reqwest::Client::builder()
        .timeout(http_timeout())
        .build()
        .ok()?;

    match client
        .post(format!("http://127.0.0.1:{port}/list"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => Some(port),
        _ => None,
    }
}

/// List sessions from the runner
pub async fn list_sessions(port: u16) -> Result<Vec<ListSessionEntry>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/list"))
        .send()
        .await?;

    if !resp.status().is_success() {
        bail!("list sessions failed: {}", resp.status());
    }

    let body: ListSessionsResponse = resp.json().await?;
    Ok(body.children)
}

/// Stop a specific session
pub async fn stop_session(port: u16, session_id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/stop-session"))
        .json(&StopSessionRequest {
            session_id: session_id.to_string(),
        })
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("stop session failed: {text}");
    }
    Ok(())
}

/// Spawn a new session via the runner
pub async fn spawn_session(
    port: u16,
    req: &SpawnSessionRequest,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/spawn-session"))
        .json(req)
        .send()
        .await?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await?;

    if !status.is_success() && status != reqwest::StatusCode::CONFLICT {
        let error = body.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
        bail!("spawn session failed: {error}");
    }

    Ok(body)
}

/// Request runner shutdown (fire-and-forget HTTP request)
pub async fn stop_runner(port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let _ = client
        .post(format!("http://127.0.0.1:{port}/stop"))
        .send()
        .await;
    Ok(())
}

/// Gracefully stop the runner: try HTTP stop, wait for process death, then force kill.
/// Matches TS `stopRunner()` in controlClient.ts.
pub async fn stop_runner_gracefully(state_path: &Path, _lock_path: &Path) {
    let state = match persistence::read_runner_state(state_path) {
        Some(s) => s,
        None => {
            debug!("no runner state found");
            return;
        }
    };

    let pid = state.pid;
    debug!(pid = pid, "stopping runner");

    // Try HTTP graceful stop
    if let Ok(()) = stop_runner(state.http_port).await {
        if wait_for_process_death(pid, 2000).await {
            debug!("runner stopped gracefully via HTTP");
            return;
        }
        debug!("HTTP stop sent but process still alive, will force kill");
    }

    // Force kill
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, libc::SIGKILL); }
        debug!(pid = pid, "force killed runner");
    }
}

/// Wait for a process to die within a timeout. Returns true if process died.
async fn wait_for_process_death(pid: u32, timeout_ms: u64) -> bool {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms);
    while start.elapsed() < timeout {
        if !persistence::is_process_alive(pid) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

/// Check if the running runner version matches the currently installed CLI version.
/// Matches TS `isRunnerRunningCurrentlyInstalledHappyVersion()`.
pub async fn is_runner_running_current_version(
    state_path: &Path,
    lock_path: &Path,
) -> Option<bool> {
    // Check if runner is alive first
    let _port = check_runner_alive(state_path, lock_path).await?;
    let state = persistence::read_runner_state(state_path)?;

    // Compare mtime first (preferred)
    let current_mtime = crate::commands::runner::get_cli_mtime_ms();
    if let (Some(current), Some(started)) = (current_mtime, state.started_with_cli_mtime_ms) {
        debug!(current = current, started = started as i64, "comparing CLI mtime");
        return Some(current == started as i64);
    }

    // Fallback: compare version string
    let current_version = env!("CARGO_PKG_VERSION");
    debug!(current = current_version, started = %state.started_with_cli_version, "comparing CLI version");
    Some(current_version == state.started_with_cli_version)
}

/// Notify runner that a session has started (webhook)
pub async fn notify_session_started(
    port: u16,
    session_id: &str,
    metadata: Option<serde_json::Value>,
) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/session-started"))
        .json(&SessionStartedPayload {
            session_id: session_id.to_string(),
            metadata,
        })
        .send()
        .await?;

    if !resp.status().is_success() {
        warn!(status = %resp.status(), "session-started webhook failed");
    }
    Ok(())
}
