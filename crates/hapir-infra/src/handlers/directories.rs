use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracing::debug;

use crate::rpc::RpcRegistry;

use super::path_security::validate_path;

pub async fn register_directory_handlers(rpc: &(impl RpcRegistry + Sync), working_directory: &str) {
    let wd = Arc::new(working_directory.to_string());

    // listDirectory handler
    {
        let wd = wd.clone();
        rpc.register("listDirectory", move |params: Value| {
            let wd = wd.clone();
            async move {
                let target = params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");

                if let Err(e) = validate_path(target, &wd) {
                    return json!({"success": false, "error": e});
                }

                let resolved = Path::new(wd.as_str()).join(target);
                debug!(path = %resolved.display(), "listDirectory handler");

                let mut read_dir = match tokio::fs::read_dir(&resolved).await {
                    Ok(rd) => rd,
                    Err(e) => {
                        debug!(error = %e, "Failed to list directory");
                        return json!({"success": false, "error": format!("Failed to list directory: {e}")});
                    }
                };

                let mut entries = Vec::new();
                while let Ok(Some(entry)) = read_dir.next_entry().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let file_type = entry.file_type().await.ok();

                    let entry_type = match &file_type {
                        Some(ft) if ft.is_dir() => "directory",
                        Some(ft) if ft.is_file() => "file",
                        _ => "other",
                    };

                    let mut entry_json = json!({
                        "name": name,
                        "type": entry_type,
                    });

                    // Get size/modified for non-symlinks
                    if file_type.as_ref().is_some_and(|ft| !ft.is_symlink())
                        && let Ok(meta) = entry.metadata().await
                    {
                        entry_json["size"] = json!(meta.len());
                        if let Ok(modified) = meta.modified()
                            && let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH)
                        {
                            entry_json["modified"] = json!(dur.as_millis() as u64);
                        }
                    }

                    entries.push(entry_json);
                }

                // Sort: directories first, then alphabetical
                entries.sort_by(|a, b| {
                    let a_type = a["type"].as_str().unwrap_or("");
                    let b_type = b["type"].as_str().unwrap_or("");
                    let a_is_dir = a_type == "directory";
                    let b_is_dir = b_type == "directory";
                    match (a_is_dir, b_is_dir) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => {
                            let a_name = a["name"].as_str().unwrap_or("");
                            let b_name = b["name"].as_str().unwrap_or("");
                            a_name.cmp(b_name)
                        }
                    }
                });

                json!({"success": true, "entries": entries})
            }
        })
        .await;
    }

    // getDirectoryTree handler
    {
        let wd = wd.clone();
        rpc.register("getDirectoryTree", move |params: Value| {
            let wd = wd.clone();
            async move {
                let target = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                let max_depth = params.get("maxDepth").and_then(|v| v.as_i64()).unwrap_or(3);

                if max_depth < 0 {
                    return json!({"success": false, "error": "maxDepth must be non-negative"});
                }

                if let Err(e) = validate_path(target, &wd) {
                    return json!({"success": false, "error": e});
                }

                let resolved = Path::new(wd.as_str()).join(target);
                debug!(path = %resolved.display(), max_depth, "getDirectoryTree handler");

                let base_name = resolved
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| resolved.to_string_lossy().to_string());

                match build_tree(&resolved, &base_name, 0, max_depth as usize).await {
                    Some(tree) => json!({"success": true, "tree": tree}),
                    None => {
                        json!({"success": false, "error": "Failed to access the specified path"})
                    }
                }
            }
        })
        .await;
    }
}

async fn build_tree(path: &Path, name: &str, depth: usize, max_depth: usize) -> Option<Value> {
    let meta = tokio::fs::symlink_metadata(path).await.ok()?;

    let node_type = if meta.is_dir() { "directory" } else { "file" };
    let mut node = json!({
        "name": name,
        "path": path.to_string_lossy(),
        "type": node_type,
        "size": meta.len(),
    });

    if let Ok(modified) = meta.modified()
        && let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        node["modified"] = json!(dur.as_millis() as u64);
    }

    if meta.is_dir() && depth < max_depth {
        let mut read_dir = tokio::fs::read_dir(path).await.ok()?;
        let mut children = Vec::new();

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let ft = match entry.file_type().await {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            let child_name = entry.file_name().to_string_lossy().to_string();
            let child_path = entry.path();
            if let Some(child_node) =
                Box::pin(build_tree(&child_path, &child_name, depth + 1, max_depth)).await
            {
                children.push(child_node);
            }
        }

        children.sort_by(|a, b| {
            let a_is_dir = a["type"].as_str() == Some("directory");
            let b_is_dir = b["type"].as_str() == Some("directory");
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let a_name = a["name"].as_str().unwrap_or("");
                    let b_name = b["name"].as_str().unwrap_or("");
                    a_name.cmp(b_name)
                }
            }
        });

        node["children"] = json!(children);
    }

    Some(node)
}
