use std::sync::Arc;

use serde_json::{Value, json};
use tracing::debug;

use crate::rpc::RpcRegistry;

use super::path_security::validate_path;

pub async fn register_ripgrep_handlers(rpc: &(impl RpcRegistry + Sync), working_directory: &str) {
    let wd = Arc::new(working_directory.to_string());

    rpc.register("ripgrep", move |params: Value| {
        let wd = wd.clone();
        async move {
            let args: Vec<String> = match params.get("args").and_then(|v| v.as_array()) {
                Some(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                None => return json!({"success": false, "error": "Missing 'args' field"}),
            };

            let cwd_override = params.get("cwd").and_then(|v| v.as_str()).map(String::from);

            if let Some(ref cwd) = cwd_override
                && let Err(e) = validate_path(cwd, &wd)
            {
                return json!({"success": false, "error": e});
            }

            let cwd = cwd_override.unwrap_or_else(|| wd.to_string());
            debug!(args = ?args, cwd = %cwd, "ripgrep handler");

            let result = tokio::process::Command::new("rg")
                .args(&args)
                .current_dir(&cwd)
                .output()
                .await;

            match result {
                Ok(output) => {
                    let code = output.status.code().unwrap_or(1);
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    json!({
                        "success": true,
                        "exitCode": code,
                        "stdout": stdout,
                        "stderr": stderr
                    })
                }
                Err(e) => {
                    debug!(error = %e, "Failed to run ripgrep");
                    json!({"success": false, "error": format!("Failed to run ripgrep: {e}")})
                }
            }
        }
    })
    .await;
}
