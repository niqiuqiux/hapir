use super::session_base::AgentSessionBase;
use hapir_shared::modes::SessionMode;
use hapir_infra::utils::terminal::{clear_reclaim_prompt, prepare_for_local_agent, restore_after_local_agent, restore_terminal_state, save_terminal_state, set_logging_suppressed, show_reclaim_prompt, show_reclaiming_prompt};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::{debug, info};

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
                save_terminal_state();
                prepare_for_local_agent();
                let reason = (opts.run_local)(&session).await;
                restore_after_local_agent();
                if reason == LoopResult::Exit {
                    return Ok(());
                }
                mode = SessionMode::Remote;
                session.on_mode_change(mode).await;
                restore_terminal_state();
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
    show_reclaim_prompt();

    // Switch stdin to raw mode to capture individual keypresses
    #[cfg(unix)]
    {
        use tokio::io::AsyncReadExt;

        let _raw_guard = RawModeGuard::enter();
        let mut stdin = tokio::io::stdin();
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

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{
            ENABLE_EXTENDED_FLAGS, ENABLE_WINDOW_INPUT, GetConsoleMode, GetStdHandle, INPUT_RECORD,
            KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE, SetConsoleMode,
        };

        // Read console input in a blocking thread to avoid blocking the async runtime
        let pressed = tokio::task::spawn_blocking(|| unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return false;
            }

            // Save original console mode and switch to raw input
            let mut original_mode: u32 = 0;
            if GetConsoleMode(handle, &mut original_mode) == 0 {
                return false;
            }
            // Disable line input and echo; enable window input for raw key events
            let raw_mode = ENABLE_EXTENDED_FLAGS | ENABLE_WINDOW_INPUT;
            SetConsoleMode(handle, raw_mode);

            struct ConsoleGuard {
                handle: *mut core::ffi::c_void,
                original_mode: u32,
            }
            // SAFETY: the handle is valid for the duration of this blocking task
            unsafe impl Send for ConsoleGuard {}
            impl Drop for ConsoleGuard {
                fn drop(&mut self) {
                    unsafe {
                        SetConsoleMode(self.handle, self.original_mode);
                    }
                }
            }
            let _guard = ConsoleGuard {
                handle,
                original_mode,
            };

            let mut record: INPUT_RECORD = std::mem::zeroed();
            let mut events_read: u32 = 0;
            loop {
                if ReadConsoleInputW(handle, &mut record, 1, &mut events_read) == 0 {
                    return false;
                }
                if events_read == 1 && record.EventType as u32 == KEY_EVENT {
                    let key_event = &record.Event.KeyEvent;
                    // Only react on key-down of Space (0x20)
                    if key_event.bKeyDown != 0 && key_event.uChar.UnicodeChar == 0x20 {
                        return true;
                    }
                }
            }
        })
        .await
        .unwrap_or(false);

        if pressed {
            info!("[loop] Space pressed, requesting switch to local");
            show_reclaiming_prompt();
            notify.notify_one();
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        // On other platforms, just wait indefinitely (switch via web UI only)
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
