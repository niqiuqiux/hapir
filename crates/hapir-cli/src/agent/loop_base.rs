use super::session_base::{AgentSessionBase, SessionMode};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::stdin;
use tokio::sync::Notify;
use tracing::{debug, info};
use hapir_infra::utils::terminal::{clear_reclaim_prompt, prepare_for_local_agent, restore_after_local_agent, set_logging_suppressed, show_reclaim_prompt, show_reclaiming_prompt};

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
    pub session: Arc<AgentSessionBase<Mode>>,
    pub starting_mode: Option<SessionMode>,
    pub log_tag: String,
    pub run_local: LoopLauncher<Mode>,
    pub run_remote: LoopLauncher<Mode>,
    pub on_session_ready: Option<SessionReadyCallback<Mode>>,
    /// When true, show a "press Space to reclaim" prompt in the terminal
    /// while the remote launcher is running.
    pub terminal_reclaim: bool,
}

/// Run the session with an optional on_session_ready callback, then enter the loop.
pub async fn run_local_remote_session<Mode: Clone + Send + 'static>(
    mut opts: LoopOptions<Mode>,
) -> anyhow::Result<()> {
    if let Some(on_ready) = opts.on_session_ready.take() {
        on_ready(&opts.session);
    }

    run_local_remote_loop(opts).await
}

/// Alternating loop between local and remote launchers.
async fn run_local_remote_loop<Mode: Clone + Send + 'static>(
    opts: LoopOptions<Mode>,
) -> anyhow::Result<()> {
    let session = opts.session;
    let mut mode = opts.starting_mode.unwrap_or(SessionMode::Local);
    let log_tag = opts.log_tag;

    loop {
        debug!("[{log_tag}] Iteration with mode: {}", mode.as_str());

        match mode {
            SessionMode::Local => {
                prepare_for_local_agent();
                let reason = (opts.run_local)(&session).await;
                restore_after_local_agent();
                if reason == LoopResult::Exit {
                    return Ok(());
                }
                mode = SessionMode::Remote;
                session.on_mode_change(mode).await;
            }
            SessionMode::Remote => {
                // When running from a terminal, show a reclaim prompt and
                // listen for Space key to switch back to local mode.
                // Keep logging suppressed so remote launcher output doesn't
                // pollute the reclaim screen.
                let keypress_handle = if opts.terminal_reclaim {
                    set_logging_suppressed(true);
                    let notify = session.switch_notify.clone();
                    Some(tokio::spawn(async move {
                        wait_for_reclaim_keypress(&notify).await;
                    }))
                } else {
                    None
                };

                let reason = (opts.run_remote)(&session).await;

                if let Some(handle) = keypress_handle {
                    handle.abort();
                    set_logging_suppressed(false);
                    clear_reclaim_prompt();
                }

                if reason == LoopResult::Exit {
                    return Ok(());
                }
                mode = SessionMode::Local;
                session.on_mode_change(mode).await;
            }
        }
    }
}

/// Display a reclaim prompt and wait for the user to press Space.
async fn wait_for_reclaim_keypress(notify: &Notify) {
    use tokio::io::AsyncReadExt;

    show_reclaim_prompt();

    // Switch stdin to raw mode to capture individual keypresses
    #[cfg(unix)]
    {
        let _raw_guard = RawModeGuard::enter();
        let mut stdin = stdin();
        let mut buf = [0u8; 1];
        loop {
            match stdin.read(&mut buf).await {
                Ok(1) if buf[0] == b' ' => {
                    info!("[loop] Space pressed, requesting switch to local");
                    show_reclaiming_prompt();
                    notify.notify_one();
                    return;
                }
                Ok(0) | Err(_) => return,
                _ => {}
            }
        }
    }

    #[cfg(not(unix))]
    {
        // On non-unix, just wait indefinitely (switch via web UI only)
        std::future::pending::<()>().await;
    }
}

/// RAII guard for terminal raw mode.
#[cfg(unix)]
struct RawModeGuard {
    original: libc::termios,
}

#[cfg(unix)]
impl RawModeGuard {
    fn enter() -> Self {
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            libc::tcgetattr(libc::STDIN_FILENO, &mut original);
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
            Self { original }
        }
    }
}

#[cfg(unix)]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
    }
}
