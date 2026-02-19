use std::future::Future;
use std::pin::Pin;

use tracing::debug;

use super::session_base::{AgentSessionBase, SessionMode};

/// Result of a loop iteration: switch to the other mode, or exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopResult {
    Switch,
    Exit,
}

/// An async launcher function that runs one side (local or remote) of the loop.
pub type LoopLauncher<Mode> = Box<
    dyn for<'a> Fn(
            &'a AgentSessionBase<Mode>,
        ) -> Pin<Box<dyn Future<Output = LoopResult> + Send + 'a>>
        + Send
        + Sync,
>;

type SessionReadyCallback<Mode> = Box<dyn FnOnce(&AgentSessionBase<Mode>) + Send>;

/// Options for the local/remote loop.
pub struct LoopOptions<Mode: Clone + Send + 'static> {
    pub session: std::sync::Arc<AgentSessionBase<Mode>>,
    pub starting_mode: Option<SessionMode>,
    pub log_tag: String,
    pub run_local: LoopLauncher<Mode>,
    pub run_remote: LoopLauncher<Mode>,
    pub on_session_ready: Option<SessionReadyCallback<Mode>>,
}

/// Run the session with an optional on_session_ready callback, then enter the loop.
pub async fn run_local_remote_session<Mode: Clone + Send + 'static>(
    opts: LoopOptions<Mode>,
) -> anyhow::Result<()> {
    if let Some(on_ready) = opts.on_session_ready {
        on_ready(&opts.session);
    }

    run_local_remote_loop(
        opts.session,
        opts.starting_mode,
        &opts.log_tag,
        &opts.run_local,
        &opts.run_remote,
    )
    .await
}

/// Alternating loop between local and remote launchers.
pub async fn run_local_remote_loop<Mode: Clone + Send + 'static>(
    session: std::sync::Arc<AgentSessionBase<Mode>>,
    starting_mode: Option<SessionMode>,
    log_tag: &str,
    run_local: &LoopLauncher<Mode>,
    run_remote: &LoopLauncher<Mode>,
) -> anyhow::Result<()> {
    let mut mode = starting_mode.unwrap_or(SessionMode::Local);

    loop {
        debug!("[{log_tag}] Iteration with mode: {}", mode.as_str());

        match mode {
            SessionMode::Local => {
                let reason = run_local(&session).await;
                if reason == LoopResult::Exit {
                    return Ok(());
                }
                mode = SessionMode::Remote;
                session.on_mode_change(mode).await;
            }
            SessionMode::Remote => {
                let reason = run_remote(&session).await;
                if reason == LoopResult::Exit {
                    return Ok(());
                }
                mode = SessionMode::Local;
                session.on_mode_change(mode).await;
            }
        }
    }
}
