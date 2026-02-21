use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracing::debug;

use crate::rpc::RpcRegistry;

use super::path_security::validate_path;

async fn run_git_command(args: &[&str], cwd: &str, timeout_ms: u64) -> Value {
    let result = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        tokio::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let code = output.status.code().unwrap_or(1);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if output.status.success() {
                json!({"success": true, "stdout": stdout, "stderr": stderr, "exitCode": code})
            } else {
                json!({"success": false, "error": stderr.clone(), "stdout": stdout, "stderr": stderr, "exitCode": code})
            }
        }
        Ok(Err(e)) => {
            json!({"success": false, "error": e.to_string(), "stdout": "", "stderr": e.to_string(), "exitCode": 1})
        }
        Err(_) => {
            json!({"success": false, "error": "Command timed out", "stdout": "", "stderr": "", "exitCode": -1})
        }
    }
}

fn resolve_cwd(params: &Value, wd: &str) -> Result<String, Value> {
    let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or(wd);

    if let Err(e) = validate_path(cwd, wd) {
        return Err(json!({"success": false, "error": e}));
    }
    Ok(cwd.to_string())
}

pub async fn register_git_handlers(rpc: &(impl RpcRegistry + Sync), working_directory: &str) {
    let wd = Arc::new(working_directory.to_string());

    // git-status
    {
        let wd = wd.clone();
        rpc.register("git-status", move |params: Value| {
            let wd = wd.clone();
            async move {
                let cwd = match resolve_cwd(&params, &wd) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let timeout = params
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10_000);
                debug!(cwd = %cwd, "git-status handler");
                run_git_command(
                    &[
                        "status",
                        "--porcelain=v2",
                        "--branch",
                        "--untracked-files=all",
                    ],
                    &cwd,
                    timeout,
                )
                .await
            }
        })
        .await;
    }

    // git-diff-numstat
    {
        let wd = wd.clone();
        rpc.register("git-diff-numstat", move |params: Value| {
            let wd = wd.clone();
            async move {
                let cwd = match resolve_cwd(&params, &wd) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let timeout = params
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10_000);
                let staged = params
                    .get("staged")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                debug!(cwd = %cwd, staged, "git-diff-numstat handler");

                let args: Vec<&str> = if staged {
                    vec!["diff", "--cached", "--numstat"]
                } else {
                    vec!["diff", "--numstat"]
                };
                run_git_command(&args, &cwd, timeout).await
            }
        })
        .await;
    }

    // git-diff-file
    {
        let wd = wd.clone();
        rpc.register("git-diff-file", move |params: Value| {
            let wd = wd.clone();
            async move {
                let cwd = match resolve_cwd(&params, &wd) {
                    Ok(c) => c,
                    Err(e) => return e,
                };
                let file_path = match params.get("filePath").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => return json!({"success": false, "error": "Missing 'filePath' field"}),
                };
                if let Err(e) = validate_path(&file_path, &wd) {
                    return json!({"success": false, "error": e});
                }
                let timeout = params
                    .get("timeout")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10_000);
                let staged = params
                    .get("staged")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                debug!(cwd = %cwd, file_path = %file_path, staged, "git-diff-file handler");

                let args: Vec<&str> = if staged {
                    vec!["diff", "--cached", "--no-ext-diff", "--", &file_path]
                } else {
                    vec!["diff", "--no-ext-diff", "--", &file_path]
                };
                run_git_command(&args, &cwd, timeout).await
            }
        })
        .await;
    }
}
