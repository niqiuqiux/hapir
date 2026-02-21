use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use tracing::debug;

use crate::rpc::RpcRegistry;

const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

/// Upload directory: `tmpdir()/hapir-blobs/{sessionId}/`
fn upload_dir(session_id: &str) -> PathBuf {
    std::env::temp_dir().join("hapir-blobs").join(session_id)
}

/// Clean up the upload directory for a session.
pub async fn cleanup_upload_dir(session_id: &str) {
    let dir = upload_dir(session_id);
    if dir.exists()
        && let Err(e) = tokio::fs::remove_dir_all(&dir).await
    {
        debug!("Failed to cleanup upload dir {}: {}", dir.display(), e);
    }
}

fn sanitize_filename(filename: &str) -> String {
    let sanitized: String = filename
        .replace(['/', '\\'], "_")
        .replace("..", "_")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    let truncated = if sanitized.len() > 255 {
        sanitized[..255].to_string()
    } else {
        sanitized
    };
    if truncated.is_empty() {
        "upload".to_string()
    } else {
        truncated
    }
}

pub async fn register_upload_handlers(rpc: &(impl RpcRegistry + Sync), _working_directory: &str) {
    // uploadFile handler
    rpc.register("uploadFile", move |params: Value| async move {
        let filename = match params.get("filename").and_then(|v| v.as_str()) {
            Some(f) if !f.is_empty() => f.to_string(),
            _ => return json!({"success": false, "error": "Filename is required"}),
        };
        let content_b64 = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => return json!({"success": false, "error": "Content is required"}),
        };
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Estimate decoded size from base64 length
        let estimated = (content_b64.len() * 3) / 4;
        if estimated > MAX_UPLOAD_BYTES {
            return json!({"success": false, "error": "File too large (max 50MB)"});
        }

        let bytes = match BASE64.decode(&content_b64) {
            Ok(b) => b,
            Err(e) => return json!({"success": false, "error": format!("Invalid base64: {e}")}),
        };
        if bytes.len() > MAX_UPLOAD_BYTES {
            return json!({"success": false, "error": "File too large (max 50MB)"});
        }

        let dir = upload_dir(&session_id);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return json!({"success": false, "error": format!("Failed to create upload dir: {e}")});
        }

        let sanitized = sanitize_filename(&filename);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let file_path = dir.join(format!("{timestamp}-{sanitized}"));

        match tokio::fs::write(&file_path, &bytes).await {
            Ok(()) => {
                debug!(path = %file_path.display(), "File uploaded");
                json!({"success": true, "path": file_path.to_string_lossy()})
            }
            Err(e) => {
                json!({"success": false, "error": format!("Failed to write file: {e}")})
            }
        }
    })
    .await;

    // deleteUpload handler
    rpc.register("deleteUpload", move |params: Value| async move {
        let path_str = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => return json!({"success": false, "error": "Path is required"}),
        };
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Validate path is within the session's upload directory
        let path = Path::new(&path_str);
        let dir = upload_dir(&session_id);
        let ok = path.canonicalize().ok().is_some_and(|resolved| {
            dir.canonicalize()
                .ok()
                .is_some_and(|resolved_dir| resolved.starts_with(&resolved_dir))
        });
        if !ok {
            return json!({"success": false, "error": "Invalid upload path"});
        }

        match tokio::fs::remove_file(path).await {
            Ok(()) => json!({"success": true}),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                json!({"success": true})
            }
            Err(e) => {
                json!({"success": false, "error": format!("Failed to delete: {e}")})
            }
        }
    })
    .await;
}
