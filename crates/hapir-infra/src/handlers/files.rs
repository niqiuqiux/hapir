use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::rpc::RpcRegistry;

use super::path_security::validate_path;

pub async fn register_file_handlers(rpc: &(impl RpcRegistry + Sync), working_directory: &str) {
    let wd = Arc::new(working_directory.to_string());

    // readFile handler
    {
        let wd = wd.clone();
        rpc.register("readFile", move |params: Value| {
            let wd = wd.clone();
            async move {
                let path_str = match params.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => return json!({"success": false, "error": "Missing 'path' field"}),
                };

                if let Err(e) = validate_path(&path_str, &wd) {
                    return json!({"success": false, "error": e});
                }

                let resolved = Path::new(wd.as_str()).join(&path_str);
                debug!(path = %resolved.display(), "readFile handler");

                match tokio::fs::read(&resolved).await {
                    Ok(bytes) => {
                        let content = BASE64.encode(&bytes);
                        json!({"success": true, "content": content})
                    }
                    Err(e) => {
                        debug!(error = %e, "Failed to read file");
                        json!({"success": false, "error": format!("Failed to read file: {e}")})
                    }
                }
            }
        })
        .await;
    }

    // writeFile handler
    {
        let wd = wd.clone();
        rpc.register("writeFile", move |params: Value| {
            let wd = wd.clone();
            async move {
                let path_str = match params.get("path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => return json!({"success": false, "error": "Missing 'path' field"}),
                };
                let content_b64 = match params.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c.to_string(),
                    None => return json!({"success": false, "error": "Missing 'content' field"}),
                };

                if let Err(e) = validate_path(&path_str, &wd) {
                    return json!({"success": false, "error": e});
                }

                let resolved = Path::new(wd.as_str()).join(&path_str);
                debug!(path = %resolved.display(), "writeFile handler");

                let expected_hash = params
                    .get("expectedHash")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                // Decode base64 content
                let bytes = match BASE64.decode(&content_b64) {
                    Ok(b) => b,
                    Err(e) => {
                        return json!({"success": false, "error": format!("Invalid base64 content: {e}")})
                    }
                };

                // Hash-based conflict detection
                if let Some(ref expected) = expected_hash {
                    match tokio::fs::read(&resolved).await {
                        Ok(existing) => {
                            let mut hasher = Sha256::new();
                            hasher.update(&existing);
                            let actual_hash = hex::encode(hasher.finalize());
                            if &actual_hash != expected {
                                return json!({
                                    "success": false,
                                    "error": format!(
                                        "File hash mismatch. Expected: {expected}, Actual: {actual_hash}"
                                    )
                                });
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            return json!({"success": false, "error": "File does not exist but hash was provided"});
                        }
                        Err(e) => {
                            return json!({"success": false, "error": format!("Failed to read existing file: {e}")});
                        }
                    }
                } else {
                    // No hash means file should be new
                    match tokio::fs::metadata(&resolved).await {
                        Ok(_) => {
                            return json!({"success": false, "error": "File already exists but was expected to be new"});
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // Good - file doesn't exist
                        }
                        Err(e) => {
                            return json!({"success": false, "error": format!("Failed to check file: {e}")});
                        }
                    }
                }

                // Write the file
                if let Some(parent) = resolved.parent()
                    && let Err(e) = tokio::fs::create_dir_all(parent).await
                {
                    return json!({"success": false, "error": format!("Failed to create directories: {e}")});
                }

                match tokio::fs::write(&resolved, &bytes).await {
                    Ok(()) => {
                        let mut hasher = Sha256::new();
                        hasher.update(&bytes);
                        let hash = hex::encode(hasher.finalize());
                        json!({"success": true, "hash": hash})
                    }
                    Err(e) => {
                        debug!(error = %e, "Failed to write file");
                        json!({"success": false, "error": format!("Failed to write file: {e}")})
                    }
                }
            }
        })
        .await;
    }
}
