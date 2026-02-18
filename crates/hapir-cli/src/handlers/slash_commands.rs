use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tracing::debug;

use crate::rpc::RpcHandlerManager;

fn builtin_commands(agent: &str) -> Vec<Value> {
    match agent {
        "claude" => vec![
            json!({"name": "clear", "description": "Clear conversation history", "source": "builtin"}),
            json!({"name": "compact", "description": "Compact conversation context", "source": "builtin"}),
            json!({"name": "context", "description": "Show context information", "source": "builtin"}),
            json!({"name": "cost", "description": "Show session cost", "source": "builtin"}),
            json!({"name": "plan", "description": "Toggle plan mode", "source": "builtin"}),
        ],
        "gemini" => vec![
            json!({"name": "about", "description": "About Gemini", "source": "builtin"}),
            json!({"name": "clear", "description": "Clear conversation", "source": "builtin"}),
            json!({"name": "compress", "description": "Compress context", "source": "builtin"}),
        ],
        _ => vec![],
    }
}

fn parse_frontmatter(content: &str) -> (Option<String>, String) {
    if !content.starts_with("---") {
        return (None, content.trim().to_string());
    }
    let after_first = &content[3..];
    let newline_pos = after_first.find('\n').unwrap_or(0);
    let rest = &after_first[newline_pos + 1..];
    if let Some(end) = rest.find("\n---") {
        let yaml_block = &rest[..end];
        let body = rest[end + 4..].trim_start_matches('\n').trim().to_string();
        let description = yaml_block.lines().find_map(|line| {
            line.trim()
                .strip_prefix("description:")
                .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        });
        (description, body)
    } else {
        (None, content.trim().to_string())
    }
}

fn user_commands_dir(agent: &str) -> Option<PathBuf> {
    match agent {
        "claude" => {
            let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
                .ok()
                .or_else(|| {
                    dirs_next::home_dir()
                        .map(|h| h.join(".claude").to_string_lossy().to_string())
                })
                .unwrap_or_default();
            Some(PathBuf::from(config_dir).join("commands"))
        }
        "codex" => {
            let codex_home = std::env::var("CODEX_HOME")
                .ok()
                .or_else(|| {
                    dirs_next::home_dir()
                        .map(|h| h.join(".codex").to_string_lossy().to_string())
                })
                .unwrap_or_default();
            Some(PathBuf::from(codex_home).join("prompts"))
        }
        _ => None,
    }
}

async fn scan_commands_dir(dir: &Path, source: &str, plugin_name: Option<&str>) -> Vec<Value> {
    let mut reader = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let mut commands = vec![];
    while let Ok(Some(entry)) = reader.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let name = match plugin_name {
            Some(pn) => format!("{pn}:{stem}"),
            None => stem,
        };
        let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        let (description, body) = parse_frontmatter(&content);
        let desc = description.unwrap_or_else(|| {
            if source == "plugin" {
                format!("{} command", plugin_name.unwrap_or("plugin"))
            } else {
                "Custom command".to_string()
            }
        });
        let mut cmd = json!({
            "name": name,
            "description": desc,
            "source": source,
            "content": body,
        });
        if let Some(pn) = plugin_name {
            cmd["pluginName"] = json!(pn);
        }
        commands.push(cmd);
    }
    commands.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        an.cmp(bn)
    });
    commands
}

async fn scan_plugin_commands(agent: &str) -> Vec<Value> {
    if agent != "claude" {
        return vec![];
    }
    let config_dir = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .or_else(|| {
            dirs_next::home_dir().map(|h| h.join(".claude").to_string_lossy().to_string())
        })
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

    let mut all = vec![];
    for (plugin_key, installations) in plugins {
        let last_at = plugin_key.rfind('@').unwrap_or(plugin_key.len());
        let plugin_name = if last_at > 0 {
            &plugin_key[..last_at]
        } else {
            plugin_key
        };
        let installs = match installations.as_array() {
            Some(a) => a,
            None => continue,
        };
        let best = installs.iter().max_by_key(|i| {
            i.get("lastUpdated")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        });
        if let Some(install) = best
            && let Some(install_path) = install.get("installPath").and_then(|v| v.as_str())
        {
            let commands_dir = PathBuf::from(install_path).join("commands");
            let cmds =
                scan_commands_dir(&commands_dir, "plugin", Some(plugin_name)).await;
            all.extend(cmds);
        }
    }
    all.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        an.cmp(bn)
    });
    all
}

pub async fn register_slash_command_handlers(rpc: &RpcHandlerManager, _working_directory: &str) {
    rpc.register("listSlashCommands", move |params: Value| async move {
        let agent = params
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("claude")
            .to_string();
        debug!("listSlashCommands for agent={}", agent);

        let builtin = builtin_commands(&agent);
        let user_dir = user_commands_dir(&agent);
        let user_cmds = match user_dir {
            Some(dir) => scan_commands_dir(&dir, "user", None).await,
            None => vec![],
        };
        let plugin_cmds = scan_plugin_commands(&agent).await;

        let mut commands = builtin;
        commands.extend(user_cmds);
        commands.extend(plugin_cmds);

        json!({"success": true, "commands": commands})
    })
    .await;
}
