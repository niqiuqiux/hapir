use std::io::{Write, stdout};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static LOGGING_SUPPRESSED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
static ORIGINAL_TERMIOS: OnceLock<libc::termios> = OnceLock::new();

/// Save the current terminal settings so they can be restored on exit.
/// Should be called once at program startup before any raw mode usage.
#[cfg(unix)]
pub fn save_terminal_state() {
    ORIGINAL_TERMIOS.get_or_init(|| unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        libc::tcgetattr(libc::STDIN_FILENO, &mut termios);
        termios
    });
}

#[cfg(not(unix))]
pub fn save_terminal_state() {}

/// Restore the terminal to the state saved at startup.
/// Safe to call multiple times or even if `save_terminal_state` was never called.
#[cfg(unix)]
pub fn restore_terminal_state() {
    if let Some(original) = ORIGINAL_TERMIOS.get() {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original);
        }
    }
}

#[cfg(not(unix))]
pub fn restore_terminal_state() {}

/// Suppress tracing output (used when a local agent takes over the terminal).
pub fn set_logging_suppressed(suppressed: bool) {
    LOGGING_SUPPRESSED.store(suppressed, Ordering::Relaxed);
}

/// Check whether tracing output is currently suppressed.
pub fn is_logging_suppressed() -> bool {
    LOGGING_SUPPRESSED.load(Ordering::Relaxed)
}

/// Clear the terminal screen and move cursor to top-left.
pub fn clear_screen() {
    print!("\x1b[2J\x1b[H");
    let _ = Write::flush(&mut stdout());
}

/// Prepare the terminal for a local agent process:
/// clears the screen and suppresses hapir's own log output.
pub fn prepare_for_local_agent() {
    set_logging_suppressed(true);
    clear_screen();
}

/// Restore normal terminal state after a local agent exits.
pub fn restore_after_local_agent() {
    set_logging_suppressed(false);
}

/// Show a prompt telling the terminal user the session is controlled remotely.
pub fn show_reclaim_prompt() {
    println!("\x1b[2J\x1b[H");
    println!("  Session is now controlled from the web.");
    println!("  Press \x1b[1mSpace\x1b[0m to take back control.\n");
    let _ = Write::flush(&mut stdout());
}

/// Show a transitional message while the local agent is starting up.
pub fn show_reclaiming_prompt() {
    print!("\x1b[2J\x1b[H");
    println!("  Starting local session...");
    let _ = Write::flush(&mut stdout());
}

/// Clear the reclaim prompt when leaving remote mode.
pub fn clear_reclaim_prompt() {
    clear_screen();
}
