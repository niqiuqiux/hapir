use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use crate::agent::session_base::AgentSessionBase;
use hapir_shared::modes::SessionMode;
use hapir_shared::schemas::SessionStartedBy;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;

type PermissionResponseSender = tokio::sync::oneshot::Sender<(bool, Option<serde_json::Value>)>;

/// Claude-specific session extending AgentSessionBase.
///
/// Holds Claude-specific fields like env vars, CLI args, MCP server
/// configuration, and hook settings path.
pub struct ClaudeSession<Mode: Clone + Send + 'static> {
    pub base: Arc<AgentSessionBase<Mode>>,
    pub claude_env_vars: Option<HashMap<String, String>>,
    pub claude_args: Mutex<Option<Vec<String>>>,
    pub mcp_servers: HashMap<String, Value>,
    pub hook_settings_path: String,
    pub started_by: SessionStartedBy,
    pub starting_mode: SessionMode,
    pub local_launch_failure: Mutex<Option<LocalLaunchFailure>>,
    pub pending_permissions: Arc<Mutex<HashMap<String, PermissionResponseSender>>>,
    pub active_pid: Arc<AtomicU32>,
}

#[derive(Debug, Clone)]
pub struct LocalLaunchFailure {
    pub message: String,
    pub exit_reason: String,
}

impl<Mode: Clone + Send + 'static> ClaudeSession<Mode> {
    pub fn record_local_launch_failure(&self, message: String, exit_reason: String) {
        *self.local_launch_failure.blocking_lock() = Some(LocalLaunchFailure {
            message,
            exit_reason,
        });
    }

    /// Clear the current session ID (used by /clear command).
    pub async fn clear_session_id(&self) {
        *self.base.session_id.lock().await = None;
        debug!("[Session] Session ID cleared");
    }

    /// Consume one-time Claude flags from claude_args after Claude spawn.
    /// Currently handles: --resume (with or without session ID).
    pub async fn consume_one_time_flags(&self) {
        let mut args_guard = self.claude_args.lock().await;
        let args = match args_guard.as_mut() {
            Some(a) => a,
            None => return,
        };

        let mut filtered = Vec::new();
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--resume" {
                // Check if next arg looks like a UUID
                if i + 1 < args.len() {
                    let next = &args[i + 1];
                    if !next.starts_with('-') && next.contains('-') {
                        debug!("[Session] Consumed --resume flag with session ID: {}", next);
                        i += 2;
                        continue;
                    }
                }
                debug!("[Session] Consumed --resume flag (no session ID)");
                i += 1;
                continue;
            }
            filtered.push(args[i].clone());
            i += 1;
        }

        if filtered.is_empty() {
            *args_guard = None;
        } else {
            *args_guard = Some(filtered);
        }
        debug!("[Session] Consumed one-time flags");
    }
}
