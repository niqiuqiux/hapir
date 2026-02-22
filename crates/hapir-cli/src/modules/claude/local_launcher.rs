use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::agent::local_launch_policy::{
    LocalLaunchContext, LocalLaunchExitReason, get_local_launch_exit_reason,
};
use crate::agent::local_sync::{LocalSessionScanner, LocalSyncDriver};
use crate::agent::loop_base::LoopResult;
use crate::modules::claude::session::{ClaudeSession, StartedBy};
use crate::modules::claude::session_scanner::SessionScanner;

use super::run::EnhancedMode;

/// Local launcher for Claude.
///
/// Spawns the `claude` CLI process in local/interactive mode,
/// sets up a session scanner to watch for session IDs, and manages
/// the local launch lifecycle with periodic message sync to the hub.
pub async fn claude_local_launcher(session: &Arc<ClaudeSession<EnhancedMode>>) -> LoopResult {
    let working_directory = session.base.path.clone();
    debug!("[claudeLocalLauncher] Starting in {}", working_directory);

    let scanner = Arc::new(Mutex::new(SessionScanner::new(None, &working_directory)));

    // Start periodic sync driver (3s interval)
    let mut sync_driver = LocalSyncDriver::start(
        Mutex::new(ScannerRef(scanner.clone())),
        session.base.ws_client.clone(),
        Duration::from_secs(3),
        "claudeLocalSync",
    );

    // Register session-found callback on the base session
    let scanner_for_cb = scanner.clone();
    session
        .base
        .add_session_found_callback(Box::new(move |sid| {
            let scanner = scanner_for_cb.clone();
            let sid = sid.to_string();
            tokio::spawn(async move {
                scanner.lock().await.on_new_session(&sid);
            });
        }))
        .await;

    // Build CLI arguments for local (interactive) mode
    let mut args: Vec<String> = Vec::new();

    if !session.hook_settings_path.is_empty() {
        args.push("--settings".to_string());
        args.push(session.hook_settings_path.clone());
    }

    if let Some(ref extra_args) = *session.claude_args.lock().await {
        args.extend(extra_args.iter().cloned());
    }

    debug!(
        "[claudeLocalLauncher] Spawning claude process with args: {:?}",
        args
    );

    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(&args).current_dir(&working_directory);

    if let Some(ref env_vars) = session.claude_env_vars {
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
    }

    let _exit_status = match cmd.status().await {
        Ok(status) => {
            debug!("[claudeLocalLauncher] Claude process exited: {:?}", status);
            session.consume_one_time_flags().await;
            Some(status)
        }
        Err(e) => {
            warn!("[claudeLocalLauncher] Failed to spawn claude: {}", e);
            session.record_local_launch_failure(
                format!("Failed to launch claude: {}", e),
                "spawn_error".to_string(),
            );
            session
                .base
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to launch claude CLI: {}", e),
                }))
                .await;
            None
        }
    };

    // Stop periodic sync, do a final flush, then cleanup
    sync_driver.stop();
    {
        let mut guard = scanner.lock().await;
        let messages = guard.scan().await;
        if !messages.is_empty() {
            debug!(
                "[claudeLocalSync] Final flush: {} remaining message(s)",
                messages.len()
            );
            for msg in messages {
                session.base.ws_client.send_message(msg).await;
            }
        }
        guard.cleanup().await;
    }

    let shared_started_by = match session.started_by {
        StartedBy::Runner => hapir_shared::schemas::StartedBy::Runner,
        StartedBy::Terminal => hapir_shared::schemas::StartedBy::Terminal,
    };

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: Some(shared_started_by),
        starting_mode: Some(session.starting_mode),
    });

    debug!("[claudeLocalLauncher] Exit reason: {:?}", exit_reason);

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}
/// Wrapper that delegates `LocalSessionScanner` through an `Arc<Mutex<SessionScanner>>`,
/// allowing the scanner to be shared between the sync driver and the session-found callback.
struct ScannerRef(Arc<Mutex<SessionScanner>>);

impl LocalSessionScanner for ScannerRef {
    async fn scan(&mut self) -> Vec<serde_json::Value> {
        self.0.lock().await.scan().await
    }

    fn on_new_session(&mut self, session_id: &str) {
        // Not used via the driver path; the callback calls scanner directly
        let _ = session_id;
    }

    async fn cleanup(&mut self) {
        self.0.lock().await.cleanup().await;
    }
}
