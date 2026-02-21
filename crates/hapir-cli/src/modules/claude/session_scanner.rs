use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tokio::fs;
use tracing::debug;

use super::types::{INTERNAL_CLAUDE_EVENT_TYPES, RawJsonLines};

/// A session scanner that watches `.jsonl` session files, parses lines,
/// and deduplicates by UUID.
pub struct SessionScanner {
    project_dir: PathBuf,
    current_session_id: Option<String>,
    processed_keys: HashSet<String>,
    cursors: HashMap<PathBuf, usize>,
    pending_sessions: HashSet<String>,
    finished_sessions: HashSet<String>,
    on_message: Box<dyn Fn(RawJsonLines) + Send + Sync>,
}

impl SessionScanner {
    pub fn new(
        session_id: Option<String>,
        working_directory: &str,
        on_message: Box<dyn Fn(RawJsonLines) + Send + Sync>,
    ) -> Self {
        // Claude stores session files in ~/.claude/projects/<hash>/
        // For now, use the working directory as the project dir base
        let project_dir = PathBuf::from(working_directory);

        Self {
            project_dir,
            current_session_id: session_id,
            processed_keys: HashSet::new(),
            cursors: HashMap::new(),
            pending_sessions: HashSet::new(),
            finished_sessions: HashSet::new(),
            on_message,
        }
    }

    /// Notify the scanner of a new session ID.
    pub fn on_new_session(&mut self, session_id: &str) {
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

    /// Scan session files for new messages.
    pub async fn scan(&mut self) {
        let mut files = Vec::new();
        for sid in &self.pending_sessions {
            files.push(self.session_file_path(sid));
        }
        if let Some(ref sid) = self.current_session_id
            && !self.pending_sessions.contains(sid)
        {
            files.push(self.session_file_path(sid));
        }

        let mut scanned = HashSet::new();
        for file_path in &files {
            let session_id = session_id_from_path(file_path);
            if let Some(ref sid) = session_id {
                scanned.insert(sid.clone());
            }

            let cursor = self.cursors.get(file_path).copied().unwrap_or(0);
            match read_session_log(file_path, cursor).await {
                Ok((events, total_lines)) => {
                    self.cursors.insert(file_path.clone(), total_lines);
                    for (key, message) in events {
                        if self.processed_keys.insert(key) {
                            (self.on_message)(message);
                        }
                    }
                }
                Err(_) => {
                    // File not found or read error -- skip
                }
            }
        }

        // Move scanned pending sessions to finished
        for sid in &scanned {
            if self.pending_sessions.remove(sid) {
                self.finished_sessions.insert(sid.clone());
            }
        }
    }

    /// Clean up resources.
    pub async fn cleanup(&mut self) {
        self.pending_sessions.clear();
        self.cursors.clear();
    }

    fn session_file_path(&self, session_id: &str) -> PathBuf {
        self.project_dir.join(format!("{}.jsonl", session_id))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    if name.ends_with(".jsonl") {
        Some(name[..name.len() - ".jsonl".len()].to_string())
    } else {
        None
    }
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

        // Skip internal event types
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
