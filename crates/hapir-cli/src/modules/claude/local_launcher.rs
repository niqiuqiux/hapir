use std::sync::Arc;

use tracing::{debug, warn};

use crate::agent::local_launch_policy::{
    LocalLaunchContext, LocalLaunchExitReason, get_local_launch_exit_reason,
};
use crate::agent::loop_base::LoopResult;
use crate::modules::claude::session::{ClaudeSession, StartedBy};

use super::run::EnhancedMode;

/// Local launcher for Claude.
///
/// Spawns the `claude` CLI process in local/interactive mode and manages
/// the local launch lifecycle. Message sync is handled by the hook server.
pub async fn claude_local_launcher(session: &Arc<ClaudeSession<EnhancedMode>>) -> LoopResult {
    let working_directory = session.base.path.clone();
    debug!("[claudeLocalLauncher] Starting in {}", working_directory);

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

    match cmd.status().await {
        Ok(status) => {
            debug!("[claudeLocalLauncher] Claude process exited: {:?}", status);
            session.consume_one_time_flags().await;
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
        }
    };

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
