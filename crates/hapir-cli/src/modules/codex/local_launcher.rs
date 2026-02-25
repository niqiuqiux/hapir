use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::select;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use hapir_shared::schemas::SessionStartedBy as SharedStartedBy;
use hapir_shared::session::FlatMessage;

use crate::agent::local_launch_policy::{
    LocalLaunchContext, LocalLaunchExitReason, get_local_launch_exit_reason,
};
use crate::agent::local_sync::LocalSyncDriver;
use crate::agent::loop_base::LoopResult;
use crate::agent::session_base::AgentSessionBase;

use super::CodexMode;
use super::session_scanner::CodexSessionScanner;

pub async fn codex_local_launcher(session: &Arc<AgentSessionBase<CodexMode>>) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[codexLocalLauncher] Starting in {}", working_directory);

    let session_ref = session.clone();
    let scanner = CodexSessionScanner::new(
        &working_directory,
        session.session_id.lock().await.clone(),
        Some(Box::new(move |sid: &str| {
            let s = session_ref.clone();
            let sid = sid.to_string();
            tokio::spawn(async move {
                s.on_session_found(&sid).await;
            });
        })),
    );
    let scanner = Arc::new(Mutex::new(scanner));

    let mut sync_driver = LocalSyncDriver::start(
        scanner.clone(),
        session.ws_client.clone(),
        Duration::from_secs(2),
        "codexLocalSync",
    );

    let mut cmd = Command::new("codex");
    if let Some(ref sid) = *session.session_id.lock().await {
        cmd.arg("resume").arg(sid);
    }
    cmd.current_dir(&working_directory);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            warn!("[codexLocalLauncher] Failed to spawn codex: {}", e);
            session
                .ws_client
                .send_typed_message(&FlatMessage::Error {
                    message: format!("Failed to launch codex CLI: {}", e),
                    exit_reason: None,
                })
                .await;
            sync_driver.stop();
            return LoopResult::Exit;
        }
    };

    let switched = select! {
        status = child.wait() => {
            match status {
                Ok(s) => debug!("[codexLocalLauncher] Codex process exited: {:?}", s),
                Err(e) => warn!("[codexLocalLauncher] Error waiting for codex: {}", e),
            }
            false
        }
        _ = session.switch_notify.notified() => {
            info!("[codexLocalLauncher] Switch requested, killing codex process");
            kill_child_gracefully(&mut child).await;
            true
        }
    };

    sync_driver.stop();
    LocalSyncDriver::final_flush(&scanner, &session.ws_client, "codexLocalSync").await;

    if switched {
        return LoopResult::Switch;
    }

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: Some(SharedStartedBy::Terminal),
        starting_mode: Some(*session.mode.lock().await),
    });

    debug!("[codexLocalLauncher] Exit reason: {:?}", exit_reason);

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}

async fn kill_child_gracefully(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = hapir_infra::utils::process::kill_process_tree(pid, false).await;
    } else {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
}
