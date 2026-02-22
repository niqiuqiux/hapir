use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::fs;
use tracing::debug;

use crate::agent::local_sync::LocalSessionScanner;

use super::types::{INTERNAL_CLAUDE_EVENT_TYPES, RawJsonLines};

/// A session scanner that watches `.jsonl` session files, parses lines,
/// deduplicates by UUID, and returns hub-ready messages.
pub struct SessionScanner {
    project_dir: PathBuf,
    current_session_id: Option<String>,
    processed_keys: HashSet<String>,
    cursors: HashMap<PathBuf, usize>,
    pending_sessions: HashSet<String>,
    finished_sessions: HashSet<String>,
}

impl SessionScanner {
    pub fn new(session_id: Option<String>, working_directory: &str) -> Self {
        let project_dir = get_claude_project_path(working_directory);

        Self {
            project_dir,
            current_session_id: session_id,
            processed_keys: HashSet::new(),
            cursors: HashMap::new(),
            pending_sessions: HashSet::new(),
            finished_sessions: HashSet::new(),
        }
    }

    fn session_file_path(&self, session_id: &str) -> PathBuf {
        self.project_dir.join(format!("{}.jsonl", session_id))
    }

    /// Core scan logic: read files, deduplicate, convert to hub messages.
    async fn scan_inner(&mut self) -> Vec<Value> {
        let mut files = Vec::new();
        for sid in &self.pending_sessions {
            files.push(self.session_file_path(sid));
        }
        if let Some(ref sid) = self.current_session_id
            && !self.pending_sessions.contains(sid)
        {
            files.push(self.session_file_path(sid));
        }
        let mut result = Vec::new();
        let mut scanned = HashSet::new();

        for file_path in &files {
            let session_id = session_id_from_path(file_path);
            if let Some(ref sid) = session_id {
                scanned.insert(sid.clone());
            }

            let cursor = self.cursors.get(file_path).copied().unwrap_or(0);
            if let Ok((events, total_lines)) = read_session_log(file_path, cursor).await {
                    self.cursors.insert(file_path.clone(), total_lines);
                    for (key, message) in events {
                        if self.processed_keys.insert(key)
                            && let Some(hub_msg) = convert_to_hub_message(&message)
                        {
                            result.push(hub_msg);
                        }
                    }
            }
        }

        for sid in &scanned {
            if self.pending_sessions.remove(sid) {
                self.finished_sessions.insert(sid.clone());
            }
        }

        result
    }
}

impl LocalSessionScanner for SessionScanner {
    async fn scan(&mut self) -> Vec<Value> {
        self.scan_inner().await
    }

    fn on_new_session(&mut self, session_id: &str) {
        if self.current_session_id.as_deref() == Some(session_id) {
            debug!(
                "[SESSION_SCANNER] New session: {} is the same as current, skipping",
                session_id
            );
            return;
        }
        if self.finished_sessions.contains(session_id) {
            debug!(
                "[SESSION_SCANNER] New session: {} is already finished, skipping",
                session_id
            );
            return;
        }
        if self.pending_sessions.contains(session_id) {
            debug!(
                "[SESSION_SCANNER] New session: {} is already pending, skipping",
                session_id
            );
            return;
        }
        if let Some(ref current) = self.current_session_id {
            self.pending_sessions.insert(current.clone());
        }
        debug!("[SESSION_SCANNER] New session: {}", session_id);
        self.current_session_id = Some(session_id.to_string());
    }

    async fn cleanup(&mut self) {
        self.pending_sessions.clear();
        self.cursors.clear();
    }
}
/// Convert a raw JSONL message to a hub-compatible message JSON value.
///
/// - `User` with plain text content → `{ role: "user", content: { type: "text", text }, meta: { sentFrom: "cli" } }`
/// - `User` with complex content / `Assistant` / `System` → `{ role: "agent", content: { type: "output", data }, meta: { sentFrom: "cli" } }`
/// - `Summary` → None (skipped, matching TS behavior)
fn convert_to_hub_message(raw: &RawJsonLines) -> Option<Value> {
    match raw {
        RawJsonLines::Summary { .. } => None,
        RawJsonLines::User { message, .. } => {
            if let Some(content) = &message.content
                && let Some(text) = content.as_str()
            {
                return Some(serde_json::json!({
                    "role": "user",
                    "content": { "type": "text", "text": text },
                    "meta": { "sentFrom": "cli" }
                }));
            }
            let data = serde_json::to_value(raw).ok()?;
            Some(serde_json::json!({
                "role": "agent",
                "content": { "type": "output", "data": data },
                "meta": { "sentFrom": "cli" }
            }))
        }
        _ => {
            let data = serde_json::to_value(raw).ok()?;
            Some(serde_json::json!({
                "role": "agent",
                "content": { "type": "output", "data": data },
                "meta": { "sentFrom": "cli" }
            }))
        }
    }
}

/// Get the Claude project path for a given working directory.
///
/// Mirrors the TS `getProjectPath`: resolves the absolute path, replaces
/// all non-alphanumeric characters with `-`, and joins under the Claude
/// config directory's `projects/` folder.
fn get_claude_project_path(working_directory: &str) -> PathBuf {
    let config_dir = std::env::var("CLAUDE_CONFIG_DIR").ok().unwrap_or_else(|| {
        dirs_next::home_dir()
            .map(|h| h.join(".claude").to_string_lossy().to_string())
            .unwrap_or_else(|| ".claude".to_string())
    });

    let resolved = std::path::Path::new(working_directory)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(working_directory));

    let sanitized: String = resolved
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    PathBuf::from(config_dir).join("projects").join(sanitized)
}

fn message_key(message: &RawJsonLines) -> String {
    match message {
        RawJsonLines::User { uuid, .. } => uuid.clone(),
        RawJsonLines::Assistant { uuid, .. } => uuid.clone(),
        RawJsonLines::Summary { leaf_uuid, summary } => {
            format!("summary: {}: {}", leaf_uuid, summary)
        }
        RawJsonLines::System { uuid, .. } => uuid.clone(),
    }
}

fn session_id_from_path(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_suffix(".jsonl").map(|s| s.to_string())
}

async fn read_session_log(
    file_path: &Path,
    start_line: usize,
) -> anyhow::Result<(Vec<(String, RawJsonLines)>, usize)> {
    let content = fs::read_to_string(file_path).await?;
    let lines: Vec<&str> = content.split('\n').collect();

    let has_trailing_empty = !lines.is_empty() && lines.last() == Some(&"");
    let total_lines = if has_trailing_empty {
        lines.len() - 1
    } else {
        lines.len()
    };

    let effective_start = if start_line > total_lines {
        0
    } else {
        start_line
    };

    let mut events = Vec::new();
    for line in lines.iter().skip(effective_start) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(msg_type) = parsed.get("type").and_then(|v| v.as_str())
            && INTERNAL_CLAUDE_EVENT_TYPES.contains(&msg_type)
        {
            continue;
        }

        match serde_json::from_value::<RawJsonLines>(parsed) {
            Ok(message) => {
                let key = message_key(&message);
                events.push((key, message));
            }
            Err(_) => continue,
        }
    }

    Ok((events, total_lines))
}
