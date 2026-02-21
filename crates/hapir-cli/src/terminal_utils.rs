use std::sync::atomic::{AtomicBool, Ordering};

static LOGGING_SUPPRESSED: AtomicBool = AtomicBool::new(false);

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
    let _ = std::io::Write::flush(&mut std::io::stdout());
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
