use std::path::Path;

use anyhow::{bail, Result};
use tracing::warn;

use crate::persistence;

use super::types::*;

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
        .timeout(std::time::Duration::from_secs(2))
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
pub async fn list_sessions(port: u16) -> Result<Vec<TrackedSession>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/list"))
        .send()
        .await?;

    if !resp.status().is_success() {
        bail!("list sessions failed: {}", resp.status());
    }

    let body: ListSessionsResponse = resp.json().await?;
    Ok(body.sessions)
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
) -> Result<SpawnSessionResponse> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/spawn-session"))
        .json(req)
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("spawn session failed: {text}");
    }

    Ok(resp.json().await?)
}

/// Request runner shutdown
pub async fn stop_runner(port: u16) -> Result<()> {
    let client = reqwest::Client::new();
    let _ = client
        .post(format!("http://127.0.0.1:{port}/stop"))
        .send()
        .await;
    Ok(())
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
