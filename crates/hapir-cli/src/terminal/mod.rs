use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use hapir_shared::socket::{
    TerminalErrorPayload, TerminalExitPayload, TerminalOutputPayload, TerminalReadyPayload,
};

const DEFAULT_IDLE_TIMEOUT_MS: u64 = 15 * 60_000;
const DEFAULT_MAX_TERMINALS: usize = 4;

const SENSITIVE_ENV_KEYS: &[&str] = &[
    "CLI_API_TOKEN",
    "HAPIR_API_URL",
    "HAPIR_HTTP_MCP_URL",
    "TELEGRAM_BOT_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// Events emitted by the terminal manager.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    Ready(TerminalReadyPayload),
    Output(TerminalOutputPayload),
    Exit(TerminalExitPayload),
    Error(TerminalErrorPayload),
}

/// Options for constructing a [`TerminalManager`].
#[derive(Debug)]
pub struct TerminalManagerOptions {
    pub session_id: String,
    pub session_path: Option<String>,
    pub idle_timeout_ms: Option<u64>,
    pub max_terminals: Option<usize>,
}

struct TerminalRuntime {
    #[allow(dead_code)]
    terminal_id: String,
    cols: u16,
    rows: u16,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    idle_cancel: Option<tokio::sync::watch::Sender<()>>,
}

fn resolve_env_number(name: &str, fallback: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(fallback)
}

fn resolve_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() {
            return shell;
        }
    }
    "/bin/bash".to_string()
}

fn build_filtered_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, val)| {
            !val.is_empty() && !SENSITIVE_ENV_KEYS.contains(&key.as_str())
        })
        .collect()
}

pub struct TerminalManager {
    session_id: String,
    session_path: Option<String>,
    idle_timeout_ms: u64,
    max_terminals: usize,
    terminals: Arc<Mutex<HashMap<String, TerminalRuntime>>>,
    event_tx: mpsc::UnboundedSender<TerminalEvent>,
    filtered_env: Vec<(String, String)>,
}

impl std::fmt::Debug for TerminalManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalManager")
            .field("session_id", &self.session_id)
            .field("max_terminals", &self.max_terminals)
            .finish()
    }
}

impl TerminalManager {
    /// Create a new terminal manager. Returns the manager and a receiver for terminal events.
    pub fn new(options: TerminalManagerOptions) -> (Self, mpsc::UnboundedReceiver<TerminalEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let idle_timeout_ms = options.idle_timeout_ms.unwrap_or_else(|| {
            resolve_env_number("HAPIR_TERMINAL_IDLE_TIMEOUT_MS", DEFAULT_IDLE_TIMEOUT_MS)
        });
        let max_terminals = options.max_terminals.unwrap_or_else(|| {
            resolve_env_number("HAPIR_TERMINAL_MAX_TERMINALS", DEFAULT_MAX_TERMINALS as u64) as usize
        });

        let mgr = Self {
            session_id: options.session_id,
            session_path: options.session_path,
            idle_timeout_ms,
            max_terminals,
            terminals: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
            filtered_env: build_filtered_env(),
        };
        (mgr, event_rx)
    }

    /// Spawn a new terminal or reuse an existing one with the same ID.
    pub async fn create(&self, terminal_id: &str, cols: u16, rows: u16) {
        let mut terminals = self.terminals.lock().await;

        // Reuse existing terminal
        if let Some(runtime) = terminals.get_mut(terminal_id) {
            runtime.cols = cols;
            runtime.rows = rows;
            let _ = runtime.master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
            if let Some(cancel) = runtime.idle_cancel.take() {
                let _ = cancel.send(());
            }
            let new_cancel = self.spawn_idle_timer(terminal_id);
            runtime.idle_cancel = Some(new_cancel);
            self.emit(TerminalEvent::Ready(TerminalReadyPayload {
                session_id: self.session_id.clone(),
                terminal_id: terminal_id.to_string(),
            }));
            return;
        }

        if terminals.len() >= self.max_terminals {
            self.emit_error(terminal_id, &format!("Too many terminals open (max {}).", self.max_terminals));
            return;
        }

        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        let pair = match pty_system.openpty(size) {
            Ok(p) => p,
            Err(e) => {
                self.emit_error(terminal_id, &format!("Failed to open PTY: {e}"));
                return;
            }
        };

        let shell = resolve_shell();
        let cwd = self
            .session_path
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().to_string_lossy().to_string());

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(&cwd);
        // Set filtered environment
        for (key, value) in &self.filtered_env {
            cmd.env(key, value);
        }

        let child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => {
                self.emit_error(terminal_id, &format!("Failed to spawn terminal: {e}"));
                return;
            }
        };
        // Drop the slave side - we only need the master
        drop(pair.slave);

        let writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => {
                self.emit_error(terminal_id, &format!("Failed to get PTY writer: {e}"));
                return;
            }
        };

        let reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => {
                self.emit_error(terminal_id, &format!("Failed to get PTY reader: {e}"));
                return;
            }
        };

        let idle_cancel = self.spawn_idle_timer(terminal_id);

        let runtime = TerminalRuntime {
            terminal_id: terminal_id.to_string(),
            cols,
            rows,
            writer,
            master: pair.master,
            child,
            idle_cancel: Some(idle_cancel),
        };

        terminals.insert(terminal_id.to_string(), runtime);

        // Spawn reader task
        self.spawn_reader(terminal_id.to_string(), reader);

        // Spawn exit watcher
        self.spawn_exit_watcher(terminal_id.to_string());

        self.emit(TerminalEvent::Ready(TerminalReadyPayload {
            session_id: self.session_id.clone(),
            terminal_id: terminal_id.to_string(),
        }));
    }
    /// Write data to a terminal.
    pub async fn write(&self, terminal_id: &str, data: &str) {
        let mut terminals = self.terminals.lock().await;
        let Some(runtime) = terminals.get_mut(terminal_id) else {
            self.emit_error(terminal_id, "Terminal not found.");
            return;
        };
        if let Err(e) = runtime.writer.write_all(data.as_bytes()) {
            warn!(terminal_id, error = %e, "Failed to write to terminal");
        }
        // Cancel old idle timer, spawn new one
        if let Some(cancel) = runtime.idle_cancel.take() {
            let _ = cancel.send(());
        }
        let new_cancel = self.spawn_idle_timer(terminal_id);
        runtime.idle_cancel = Some(new_cancel);
    }

    /// Resize a terminal.
    pub async fn resize(&self, terminal_id: &str, cols: u16, rows: u16) {
        let mut terminals = self.terminals.lock().await;
        let Some(runtime) = terminals.get_mut(terminal_id) else {
            return;
        };
        runtime.cols = cols;
        runtime.rows = rows;
        let _ = runtime.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Some(cancel) = runtime.idle_cancel.take() {
            let _ = cancel.send(());
        }
        let new_cancel = self.spawn_idle_timer(terminal_id);
        runtime.idle_cancel = Some(new_cancel);
    }

    /// Close a specific terminal.
    pub async fn close(&self, terminal_id: &str) {
        self.cleanup(terminal_id).await;
    }

    /// Close all terminals.
    pub async fn close_all(&self) {
        let ids: Vec<String> = self.terminals.lock().await.keys().cloned().collect();
        for id in ids {
            self.cleanup(&id).await;
        }
    }

    async fn cleanup(&self, terminal_id: &str) {
        let mut terminals = self.terminals.lock().await;
        let Some(mut runtime) = terminals.remove(terminal_id) else {
            return;
        };

        // Cancel idle timer
        if let Some(cancel) = runtime.idle_cancel.take() {
            let _ = cancel.send(());
        }

        // Kill the child process
        if let Err(e) = runtime.child.kill() {
            debug!(terminal_id, error = %e, "Failed to kill terminal process");
        }
    }

    fn emit(&self, event: TerminalEvent) {
        let _ = self.event_tx.send(event);
    }

    fn emit_error(&self, terminal_id: &str, message: &str) {
        self.emit(TerminalEvent::Error(TerminalErrorPayload {
            session_id: self.session_id.clone(),
            terminal_id: terminal_id.to_string(),
            message: message.to_string(),
        }));
    }

    fn spawn_idle_timer(&self, terminal_id: &str) -> tokio::sync::watch::Sender<()> {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(());
        let timeout = self.idle_timeout_ms;
        let terminals = self.terminals.clone();
        let event_tx = self.event_tx.clone();
        let session_id = self.session_id.clone();
        let tid = terminal_id.to_string();

        if timeout > 0 {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(timeout)) => {
                            let _ = event_tx.send(TerminalEvent::Error(TerminalErrorPayload {
                                session_id: session_id.clone(),
                                terminal_id: tid.clone(),
                                message: "Terminal closed due to inactivity.".to_string(),
                            }));
                            // Cleanup
                            let mut terms = terminals.lock().await;
                            if let Some(mut rt) = terms.remove(&tid) {
                                if let Some(c) = rt.idle_cancel.take() {
                                    let _ = c.send(());
                                }
                                let _ = rt.child.kill();
                            }
                            break;
                        }
                        _ = cancel_rx.changed() => {
                            // Timer was reset or cancelled
                            break;
                        }
                    }
                }
            });
        }

        cancel_tx
    }

    fn spawn_reader(&self, terminal_id: String, mut reader: Box<dyn Read + Send>) {
        let event_tx = self.event_tx.clone();
        let session_id = self.session_id.clone();

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        if !text.is_empty() {
                            let _ = event_tx.send(TerminalEvent::Output(TerminalOutputPayload {
                                session_id: session_id.clone(),
                                terminal_id: terminal_id.clone(),
                                data: text,
                            }));
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    fn spawn_exit_watcher(&self, terminal_id: String) {
        let terminals = self.terminals.clone();
        let event_tx = self.event_tx.clone();
        let session_id = self.session_id.clone();

        tokio::spawn(async move {
            // Poll for child exit
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let mut terms = terminals.lock().await;
                let Some(runtime) = terms.get_mut(&terminal_id) else {
                    break;
                };
                match runtime.child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.exit_code() as i32;
                        let _ = event_tx.send(TerminalEvent::Exit(TerminalExitPayload {
                            session_id: session_id.clone(),
                            terminal_id: terminal_id.clone(),
                            code: Some(code),
                            signal: None,
                        }));
                        // Cleanup
                        if let Some(mut rt) = terms.remove(&terminal_id) {
                            if let Some(c) = rt.idle_cancel.take() {
                                let _ = c.send(());
                            }
                        }
                        break;
                    }
                    Ok(None) => {
                        // Still running
                    }
                    Err(_) => break,
                }
            }
        });
    }
}
