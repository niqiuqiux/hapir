use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc};
use tracing::debug;
use hapir_infra::utils::process::kill_process_tree;
use super::types::{PermissionResult, QueryOptions, SdkMessage};

/// A running Claude query that yields SDK messages (--print mode).
pub struct Query {
    rx: mpsc::UnboundedReceiver<SdkMessage>,
    #[allow(dead_code)]
    child: Child,
    #[allow(dead_code)]
    reader_handle: tokio::task::JoinHandle<()>,
}

impl Query {
    pub async fn next_message(&mut self) -> Option<SdkMessage> {
        self.rx.recv().await
    }
}

/// An interactive Claude query using --input-format stream-json.
///
/// Keeps stdin open for bidirectional communication:
/// sending user messages and control responses (permissions).
pub struct InteractiveQuery {
    rx: mpsc::UnboundedReceiver<SdkMessage>,
    stdin: Mutex<ChildStdin>,
    #[allow(dead_code)]
    child: Child,
    #[allow(dead_code)]
    reader_handle: tokio::task::JoinHandle<()>,
}

impl InteractiveQuery {
    /// Return the OS PID of the child process, if still running.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    pub async fn next_message(&mut self) -> Option<SdkMessage> {
        self.rx.recv().await
    }

    /// Send a user message via stdin (JSON line).
    pub async fn send_user_message(&self, content: &str) -> anyhow::Result<()> {
        let msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": content,
            }
        });
        self.write_json_line(&msg).await
    }

    /// Send a control response (permission approved) via stdin.
    pub async fn send_control_response(
        &self,
        request_id: &str,
        result: PermissionResult,
    ) -> anyhow::Result<()> {
        let msg = serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": result,
            }
        });
        self.write_json_line(&msg).await
    }

    /// Send a control error response (permission denied) via stdin.
    pub async fn send_control_error(&self, request_id: &str, error: &str) -> anyhow::Result<()> {
        let msg = serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "error",
                "request_id": request_id,
                "error": error,
            }
        });
        self.write_json_line(&msg).await
    }

    /// Close stdin, signaling no more input.
    pub async fn close_stdin(&self) {
        let mut stdin = self.stdin.lock().await;
        let _ = stdin.shutdown().await;
    }

    /// Kill the child process tree (SIGTERM → wait → SIGKILL).
    pub async fn kill(&self) {
        let _ = self.stdin.lock().await.shutdown().await;
        if let Some(pid) = self.child.id() {
            let _ = kill_process_tree(pid, false).await;
        }
    }

    async fn write_json_line(&self, value: &serde_json::Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }
}

/// Build common CLI args from QueryOptions.
fn build_common_args(options: &QueryOptions) -> Vec<String> {
    let mut args = vec![
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];
    if let Some(ref sp) = options.custom_system_prompt {
        args.push("--system-prompt".to_string());
        args.push(sp.clone());
    }
    if let Some(ref ap) = options.append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(ap.clone());
    }
    if let Some(turns) = options.max_turns {
        args.push("--max-turns".to_string());
        args.push(turns.to_string());
    }
    if let Some(ref model) = options.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    if options.continue_conversation {
        args.push("--continue".to_string());
    }
    if let Some(ref resume) = options.resume {
        args.push("--resume".to_string());
        args.push(resume.clone());
    }
    if let Some(ref sp) = options.settings_path {
        args.push("--settings".to_string());
        args.push(sp.clone());
    }
    if !options.allowed_tools.is_empty() {
        args.push("--allowedTools".to_string());
        args.push(options.allowed_tools.join(","));
    }
    if !options.disallowed_tools.is_empty() {
        args.push("--disallowedTools".to_string());
        args.push(options.disallowed_tools.join(","));
    }
    for dir in &options.additional_directories {
        args.push("--add-dir".to_string());
        args.push(dir.clone());
    }
    if options.strict_mcp_config {
        args.push("--strict-mcp-config".to_string());
    }
    if let Some(ref pm) = options.permission_mode {
        args.push("--permission-mode".to_string());
        args.push(pm.clone());
    }
    if let Some(ref fm) = options.fallback_model {
        args.push("--fallback-model".to_string());
        args.push(fm.clone());
    }
    if let Some(ref servers) = options.mcp_servers {
        let config = serde_json::json!({"mcpServers": servers});
        args.push("--mcp-config".to_string());
        args.push(config.to_string());
    }
    args
}

fn spawn_stdout_reader(
    stdout: tokio::process::ChildStdout,
) -> (
    mpsc::UnboundedReceiver<SdkMessage>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            debug!("[claude-sdk-raw] {}", trimmed);
            match serde_json::from_str::<SdkMessage>(trimmed) {
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    debug!("Failed to parse SDK message: {} -- {}", e, trimmed);
                }
            }
        }
    });
    (rx, handle)
}

fn spawn_stderr_reader(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                tracing::warn!("[claude-sdk-stderr] {}", line);
            }
        }
    });
}

fn spawn_child(executable: &str, args: &[String], cwd: Option<&str>) -> anyhow::Result<Child> {
    let mut cmd = Command::new(executable);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    Ok(cmd.spawn()?)
}

/// Spawn a Claude process in --print mode.
/// Stdin is closed immediately (Claude needs EOF to start processing).
pub fn query(prompt: &str, options: QueryOptions) -> anyhow::Result<Query> {
    let executable = options
        .path_to_executable
        .clone()
        .unwrap_or_else(|| "claude".to_string());

    let mut args = build_common_args(&options);
    args.push("--print".to_string());
    args.push(prompt.to_string());

    debug!("Spawning Claude process: {} {}", executable, args.join(" "));

    let mut child = spawn_child(&executable, &args, options.cwd.as_deref())?;
    let stdout = child.stdout.take().expect("child stdout");

    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_reader(stderr);
    }

    // Close stdin for --print mode
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = stdin.shutdown().await;
        });
    }

    let (rx, reader_handle) = spawn_stdout_reader(stdout);

    Ok(Query {
        rx,
        child,
        reader_handle,
    })
}

/// Spawn a Claude process in interactive mode (--input-format stream-json).
///
/// Keeps stdin open for sending user messages and control responses.
/// The caller must send the initial user message via `send_user_message()`.
pub fn query_interactive(options: QueryOptions) -> anyhow::Result<InteractiveQuery> {
    let executable = options
        .path_to_executable
        .clone()
        .unwrap_or_else(|| "claude".to_string());

    let mut args = build_common_args(&options);
    args.push("--input-format".to_string());
    args.push("stream-json".to_string());
    args.push("--permission-prompt-tool".to_string());
    args.push("stdio".to_string());

    debug!(
        "Spawning interactive Claude process: {} {}",
        executable,
        args.join(" ")
    );

    let mut child = spawn_child(&executable, &args, options.cwd.as_deref())?;
    let stdout = child.stdout.take().expect("child stdout");
    let stdin = child.stdin.take().expect("child stdin");

    if let Some(stderr) = child.stderr.take() {
        spawn_stderr_reader(stderr);
    }

    let (rx, reader_handle) = spawn_stdout_reader(stdout);

    Ok(InteractiveQuery {
        rx,
        stdin: Mutex::new(stdin),
        child,
        reader_handle,
    })
}
