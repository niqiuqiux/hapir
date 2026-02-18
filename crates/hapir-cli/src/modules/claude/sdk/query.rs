use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::debug;

use super::types::{QueryOptions, SdkMessage};

/// A running Claude query that yields SDK messages.
pub struct Query {
    rx: mpsc::UnboundedReceiver<SdkMessage>,
    #[allow(dead_code)]
    child: Child,
    #[allow(dead_code)]
    reader_handle: tokio::task::JoinHandle<()>,
}

impl Query {
    /// Iterate over messages from the Claude process.
    pub async fn next_message(&mut self) -> Option<SdkMessage> {
        self.rx.recv().await
    }
}

/// Spawn a Claude process and return a `Query` that yields SDK messages.
///
/// The process is spawned with `--output-format stream-json --verbose`
/// and reads JSONL from stdout.
pub fn query(prompt: &str, options: QueryOptions) -> anyhow::Result<Query> {
    let executable = options
        .path_to_executable
        .clone()
        .unwrap_or_else(|| "claude".to_string());

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

    // Add MCP config if present
    if let Some(ref servers) = options.mcp_servers {
        let config = serde_json::json!({"mcpServers": servers});
        args.push("--mcp-config".to_string());
        args.push(config.to_string());
    }

    // Add prompt
    args.push("--print".to_string());
    args.push(prompt.to_string());

    debug!("Spawning Claude process: {} {}", executable, args.join(" "));

    let mut cmd = Command::new(&executable);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(ref cwd) = options.cwd {
        cmd.current_dir(cwd);
    }

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("child stdout");

    // Capture stderr for error reporting
    if let Some(stderr) = child.stderr.take() {
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

    // Close stdin for --print mode
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            let _ = stdin.shutdown().await;
        });
    }

    let (tx, rx) = mpsc::unbounded_channel();

    let reader_handle = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<SdkMessage>(trimmed) {
                Ok(msg) => {
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    debug!("Failed to parse SDK message: {}", trimmed);
                }
            }
        }
    });

    Ok(Query {
        rx,
        child,
        reader_handle,
    })
}
