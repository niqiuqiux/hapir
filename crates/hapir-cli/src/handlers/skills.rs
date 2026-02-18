use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tracing::debug;

use crate::rpc::RpcHandlerManager;

fn skills_root() -> PathBuf {
    let codex_home = std::env::var("CODEX_HOME")
        .ok()
        .or_else(|| {
            dirs_next::home_dir().map(|h| h.join(".codex").to_string_lossy().to_string())
        })
        .unwrap_or_default();
    PathBuf::from(codex_home).join("skills")
}

fn parse_frontmatter(content: &str) -> (Option<String>, Option<String>) {
    if !content.starts_with("---") {
        return (None, None);
    }
    let after_first = &content[3..];
    let newline_pos = after_first.find('\n').unwrap_or(0);
    let rest = &after_first[newline_pos + 1..];
    let Some(end) = rest.find("\n---") else {
        return (None, None);
    };
    let yaml_block = &rest[..end];

    let mut name = None;
    let mut description = None;
    for line in yaml_block.lines() {
        let trimmed = line.trim();
        if let Some(v) = trimmed.strip_prefix("name:") {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                name = Some(v);
            }
        }
        if let Some(v) = trimmed.strip_prefix("description:") {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !v.is_empty() {
                description = Some(v);
            }
        }
    }
    (name, description)
}

fn extract_skill_summary(skill_dir: &Path, content: &str) -> Option<Value> {
    let (name_from_fm, description) = parse_frontmatter(content);
    let name_from_dir = skill_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();

    let name = name_from_fm.unwrap_or(name_from_dir);
    if name.is_empty() {
        return None;
    }
    Some(json!({"name": name, "description": description}))
}

async fn list_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut reader = match tokio::fs::read_dir(root).await {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let mut result = vec![];
    while let Ok(Some(entry)) = reader.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".system" {
            if let Ok(mut sub) = tokio::fs::read_dir(&path).await {
                while let Ok(Some(sub_entry)) = sub.next_entry().await {
                    if sub_entry.path().is_dir() {
                        result.push(sub_entry.path());
                    }
                }
            }
        } else {
            result.push(path);
        }
    }
    result
}

async fn list_skills_impl() -> Vec<Value> {
    let root = skills_root();
    let dirs = list_skill_dirs(&root).await;
    let mut skills = vec![];
    for dir in dirs {
        let skill_file = dir.join("SKILL.md");
        if let Ok(content) = tokio::fs::read_to_string(&skill_file).await
            && let Some(summary) = extract_skill_summary(&dir, &content)
        {
            skills.push(summary);
        }
    }
    skills.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        an.cmp(bn)
    });
    skills
}

pub async fn register_skills_handlers(rpc: &RpcHandlerManager, _working_directory: &str) {
    rpc.register("listSkills", move |_params: Value| async move {
        debug!("listSkills");
        let skills = list_skills_impl().await;
        json!({"success": true, "skills": skills})
    })
    .await;
}
