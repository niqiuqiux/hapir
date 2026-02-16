use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

type CommandFn = Box<dyn Fn() -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

/// Shared mutable state for InvalidateSync.
struct Inner {
    invalidated: bool,
    invalidated_double: bool,
    stopped: bool,
    running: bool,
}

/// A coalescing sync primitive. Calling `invalidate()` triggers a command.
/// If called again while the command is running, the command re-runs once
/// after the current execution completes.
pub struct InvalidateSync {
    inner: Arc<Mutex<Inner>>,
    command: Arc<CommandFn>,
    /// Notified when a sync cycle completes (for `invalidate_and_await`).
    done_notify: Arc<Notify>,
}

impl InvalidateSync {
    pub fn new<F, Fut>(command: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let command: CommandFn = Box::new(move || Box::pin(command()));
        Self {
            inner: Arc::new(Mutex::new(Inner {
                invalidated: false,
                invalidated_double: false,
                stopped: false,
                running: false,
            })),
            command: Arc::new(command),
            done_notify: Arc::new(Notify::new()),
        }
    }

    /// Trigger the command (non-blocking). If already running, schedules
    /// one more execution after the current one finishes.
    pub fn invalidate(&self) {
        let inner = self.inner.clone();
        let command = self.command.clone();
        let done_notify = self.done_notify.clone();

        tokio::spawn(async move {
            let should_run = {
                let mut state = inner.lock().await;
                if state.stopped {
                    return;
                }
                if !state.invalidated {
                    state.invalidated = true;
                    state.invalidated_double = false;
                    !state.running
                } else {
                    state.invalidated_double = true;
                    false
                }
            };

            if should_run {
                Self::do_sync(inner, command, done_notify).await;
            }
        });
    }

    /// Trigger the command and wait until the current sync cycle completes.
    pub async fn invalidate_and_await(&self) {
        {
            let state = self.inner.lock().await;
            if state.stopped {
                return;
            }
        }
        let notified = self.done_notify.notified();
        self.invalidate();
        notified.await;
    }

    /// Stop the sync loop. Any pending waiters are released.
    pub async fn stop(&self) {
        let mut state = self.inner.lock().await;
        if state.stopped {
            return;
        }
        state.stopped = true;
        drop(state);
        self.done_notify.notify_waiters();
    }

    fn do_sync(
        inner: Arc<Mutex<Inner>>,
        command: Arc<CommandFn>,
        done_notify: Arc<Notify>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            {
                let mut state = inner.lock().await;
                state.running = true;
            }

            // Run the command.
            let _ = command().await;

            let rerun = {
                let mut state = inner.lock().await;
                state.running = false;
                if state.stopped {
                    done_notify.notify_waiters();
                    return;
                }
                if state.invalidated_double {
                    state.invalidated_double = false;
                    true
                } else {
                    state.invalidated = false;
                    done_notify.notify_waiters();
                    false
                }
            };

            if rerun {
                Self::do_sync(inner, command, done_notify).await;
            }
        })
    }
}