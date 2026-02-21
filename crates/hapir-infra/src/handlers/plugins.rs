use std::path::PathBuf;

use serde_json::Value;
use tracing::debug;

/// Parsed plugin installation info.
pub struct PluginInstall {
    pub name: String,
    pub install_path: PathBuf,
}

/// Resolve a plugin's install path. If the exact path from installed_plugins.json
/// doesn't exist (stale entry), fall back to the newest subdirectory under the
/// parent (e.g. `.../systems-programming/unknown` -> `.../systems-programming/1.2.0`).
pub async fn resolve_plugin_install_path(install_path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(install_path);
    if tokio::fs::metadata(&path).await.is_ok() {
        return Some(path);
    }

    let parent = path.parent()?;
    let mut reader = tokio::fs::read_dir(parent).await.ok()?;
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    while let Ok(Some(entry)) = reader.next_entry().await {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let mtime = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(_, t)| mtime > *t) {
            newest = Some((p, mtime));
        }
    }
    let (resolved, _) = newest?;
    debug!(
        "Plugin installPath fallback: {} -> {}",
        install_path,
        resolved.display()
    );
    Some(resolved)
}

/// Read installed_plugins.json and return resolved (name, install_path) pairs.
pub async fn get_installed_plugins() -> Vec<PluginInstall> {
    let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .or_else(|| dirs_next::home_dir().map(|h| h.join(".claude").to_string_lossy().to_string()))
        .unwrap_or_default();
    let installed_path = PathBuf::from(&config_dir)
        .join("plugins")
        .join("installed_plugins.json");

    let content = match tokio::fs::read_to_string(&installed_path).await {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let parsed: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let plugins = match parsed.get("plugins").and_then(|v| v.as_object()) {
        Some(p) => p,
        None => return vec![],
    };

    let mut result = vec![];
    for (plugin_key, installations) in plugins {
        let last_at = plugin_key.rfind('@').unwrap_or(plugin_key.len());
        let plugin_name = if last_at > 0 {
            &plugin_key[..last_at]
        } else {
            plugin_key.as_str()
        };
        let installs = match installations.as_array() {
            Some(a) => a,
            None => continue,
        };
        let best = installs
            .iter()
            .max_by_key(|i| i.get("lastUpdated").and_then(|v| v.as_str()).unwrap_or(""));
        if let Some(install) = best
            && let Some(install_path) = install.get("installPath").and_then(|v| v.as_str())
            && let Some(resolved) = resolve_plugin_install_path(install_path).await
        {
            result.push(PluginInstall {
                name: plugin_name.to_string(),
                install_path: resolved,
            });
        }
    }
    result
}
