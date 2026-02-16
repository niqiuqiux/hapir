use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tracing::debug;

use crate::rpc::RpcHandlerManager;

use super::path_security::validate_path;

pub async fn register_bash_handlers(rpc: &RpcHandlerManager, working_directory: &str) {
    let wd = Arc::new(working_directory.to_string());

    rpc.register("bash", move |params: Value| {
        let wd = wd.clone();
        async move {
            let command = match params.get("command").and_then(|v| v.as_str()) {
                Some(c) => c.to_string(),
                None => return json!({"success": false, "error": "Missing 'command' field"}),
            };

            let cwd_override = params.get("cwd").and_then(|v| v.as_str()).map(String::from);
            let timeout_ms = params
                .get("timeout")
                .and_then(|v| v.as_u64())
                .unwrap_or(30_000);

            if let Some(ref cwd) = cwd_override {
                if let Err(e) = validate_path(cwd, &wd) {
                    return json!({"success": false, "error": e});
                }
            }

            let cwd = cwd_override.unwrap_or_else(|| wd.to_string());

            debug!(command = %command, cwd = %cwd, "bash handler");

            let result = tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                run_command(&command, &cwd),
            )
            .await;

            match result {
                Ok(Ok(output)) => {
                    let code = output.status.code().unwrap_or(1);
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    json!({
                        "success": output.status.success(),
                        "stdout": stdout,
                        "stderr": stderr,
                        "exitCode": code
                    })
                }
                Ok(Err(e)) => {
                    json!({"success": false, "error": e.to_string(), "stdout": "", "stderr": e.to_string(), "exitCode": 1})
                }
                Err(_) => {
                    json!({"success": false, "error": "Command timed out", "stdout": "", "stderr": "", "exitCode": -1})
                }
            }
        }
    })
    .await;
}

async fn run_command(command: &str, cwd: &str) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output()
        .await
}
