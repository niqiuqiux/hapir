use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Duration, TimeZone, Utc};
use serde_json::Value;
use tokio::fs;
use tracing::debug;

use crate::agent::local_sync::LocalSessionScanner;

use hapir_shared::session::{AgentContent, RoleWrappedMessage};

pub type SessionFoundCallback = Box<dyn Fn(&str) + Send + Sync>;

pub struct CodexSessionScanner {
    sessions_root: PathBuf,
    target_cwd: Option<String>,
    active_session_id: Option<String>,
    reference_timestamp_ms: u64,
    session_start_window_ms: u64,
    file_cursors: HashMap<PathBuf, usize>,
    session_id_by_file: HashMap<PathBuf, String>,
    session_cwd_by_file: HashMap<PathBuf, String>,
    session_timestamp_by_file: HashMap<PathBuf, u64>,
    session_meta_parsed: HashSet<PathBuf>,
    pending_events: Vec<(PathBuf, Value)>,
    on_session_found: Option<SessionFoundCallback>,
    date_prefixes: HashSet<String>,
}

impl CodexSessionScanner {
    pub fn new(
        working_directory: &str,
        existing_session_id: Option<String>,
        on_session_found: Option<SessionFoundCallback>,
    ) -> Self {
        let home = dirs_next::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let sessions_root = home.join(".codex").join("sessions");

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let window_ms = 120_000;
        let date_prefixes = compute_date_prefixes(now_ms, window_ms);

        Self {
            sessions_root,
            target_cwd: Some(normalize_path(working_directory)),
            active_session_id: existing_session_id,
            reference_timestamp_ms: now_ms,
            session_start_window_ms: window_ms,
            file_cursors: HashMap::new(),
            session_id_by_file: HashMap::new(),
            session_cwd_by_file: HashMap::new(),
            session_timestamp_by_file: HashMap::new(),
            session_meta_parsed: HashSet::new(),
            pending_events: Vec::new(),
            on_session_found,
            date_prefixes,
        }
    }

    async fn discover_jsonl_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if !self.sessions_root.exists() {
            return files;
        }
        for prefix in &self.date_prefixes {
            let parts: Vec<&str> = prefix.split('/').collect();
            if parts.len() != 3 {
                continue;
            }
            let dir = self
                .sessions_root
                .join(parts[0])
                .join(parts[1])
                .join(parts[2]);
            let mut entries = match fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
        files
    }

    fn process_session_meta(&mut self, path: &PathBuf, payload: &Value) {
        if self.session_meta_parsed.contains(path) {
            return;
        }
        self.session_meta_parsed.insert(path.clone());

        if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
            self.session_id_by_file.insert(path.clone(), id.to_string());
        }
        if let Some(cwd) = payload.get("cwd").and_then(|v| v.as_str()) {
            self.session_cwd_by_file
                .insert(path.clone(), normalize_path(cwd));
        }
        if let Some(ts) = payload.get("timestamp").and_then(|v| v.as_str())
            && let Some(ms) = parse_iso_timestamp_ms(ts)
        {
            self.session_timestamp_by_file.insert(path.clone(), ms);
        }
    }

    fn file_matches_session(&self, path: &PathBuf) -> bool {
        let cwd_match = match (&self.target_cwd, self.session_cwd_by_file.get(path)) {
            (Some(target), Some(file_cwd)) => target == file_cwd,
            _ => false,
        };
        if !cwd_match {
            return false;
        }
        match self.session_timestamp_by_file.get(path) {
            Some(&ts) => {
                let diff = ts.abs_diff(self.reference_timestamp_ms);
                diff <= self.session_start_window_ms
            }
            None => false,
        }
    }

    fn activate_session(&mut self, sid: &str) {
        self.active_session_id = Some(sid.to_string());
        if let Some(cb) = &self.on_session_found {
            cb(sid);
        }
    }
}
// PLACEHOLDER_TRAIT

impl LocalSessionScanner for CodexSessionScanner {
    async fn scan(&mut self) -> Vec<Value> {
        let files = self.discover_jsonl_files().await;
        let mut messages = Vec::new();

        for path in files {
            let content = match fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue,
            };

            let lines: Vec<&str> = content.lines().collect();
            let cursor = self.file_cursors.get(&path).copied().unwrap_or(0);
            if cursor >= lines.len() {
                continue;
            }

            for line_str in &lines[cursor..] {
                let line: Value = match serde_json::from_str(line_str) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let event_type = line.get("type").and_then(|v| v.as_str()).unwrap_or("");

                if event_type == "session_meta"
                    && let Some(payload) = line.get("payload")
                {
                    self.process_session_meta(&path, payload);
                }

                if self.active_session_id.is_some() {
                    let file_sid = self.session_id_by_file.get(&path);
                    if file_sid == self.active_session_id.as_ref()
                        && let Some(msg) = convert_event(&line)
                    {
                        messages.push(msg);
                    }
                } else {
                    self.pending_events.push((path.clone(), line));
                }
            }

            self.file_cursors.insert(path, lines.len());
        }

        // No active session yet — find the best matching file by CWD + timestamp
        if self.active_session_id.is_none() {
            let mut best: Option<(PathBuf, u64)> = None;
            for (path, &ts) in &self.session_timestamp_by_file {
                if self.file_matches_session(path) {
                    let diff = ts.abs_diff(self.reference_timestamp_ms);
                    if best.as_ref().is_none_or(|(_, d)| diff < *d) {
                        best = Some((path.clone(), diff));
                    }
                }
            }

            if let Some((path, _)) = best
                && let Some(sid) = self.session_id_by_file.get(&path).cloned()
            {
                debug!(
                    "[codexSessionScanner] Matched session {} from {:?}",
                    sid, path
                );
                self.activate_session(&sid);

                let pending = std::mem::take(&mut self.pending_events);
                for (p, line) in pending {
                    if self.session_id_by_file.get(&p) == Some(&sid)
                        && let Some(msg) = convert_event(&line)
                    {
                        messages.push(msg);
                    }
                }
            }
        }

        messages
    }

    fn on_new_session(&mut self, session_id: &str) {
        debug!("[codexSessionScanner] New session ID: {}", session_id);
        self.active_session_id = Some(session_id.to_string());
        self.pending_events.clear();
    }

    async fn cleanup(&mut self) {
        self.file_cursors.clear();
        self.session_id_by_file.clear();
        self.session_cwd_by_file.clear();
        self.session_timestamp_by_file.clear();
        self.session_meta_parsed.clear();
        self.pending_events.clear();
        self.active_session_id = None;
    }
}
// PLACEHOLDER_HELPERS

fn codex_message_value(data: Value) -> Value {
    serde_json::to_value(&RoleWrappedMessage {
        role: "assistant".into(),
        content: AgentContent::Codex { data },
        meta: None,
    })
    .unwrap_or_default()
}

fn convert_event(line: &Value) -> Option<Value> {
    let event_type = line.get("type").and_then(|v| v.as_str())?;
    let payload = line.get("payload")?;

    match event_type {
        "event_msg" => {
            let msg_type = payload.get("type").and_then(|v| v.as_str())?;
            match msg_type {
                "user_message" => {
                    let message = payload.get("message").and_then(|v| v.as_str())?;
                    Some(serde_json::json!({
                        "role": "user",
                        "content": message,
                    }))
                }
                "agent_message" => {
                    let message = payload.get("message").and_then(|v| v.as_str())?;
                    Some(codex_message_value(serde_json::json!({
                        "type": "message",
                        "message": message,
                    })))
                }
                "agent_reasoning" => {
                    let text = payload
                        .get("text")
                        .or_else(|| payload.get("message"))
                        .and_then(|v| v.as_str())?;
                    Some(codex_message_value(serde_json::json!({
                        "type": "reasoning",
                        "message": text,
                    })))
                }
                _ => None,
            }
        }
        "response_item" => {
            let item_type = payload.get("type").and_then(|v| v.as_str())?;
            match item_type {
                "function_call" => {
                    let name = payload.get("name").and_then(|v| v.as_str())?;
                    let call_id = payload.get("call_id").and_then(|v| v.as_str())?;
                    let arguments = payload
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(Value::Null);
                    Some(codex_message_value(serde_json::json!({
                        "type": "tool-call",
                        "callId": call_id,
                        "name": name,
                        "input": arguments,
                    })))
                }
                "function_call_output" => {
                    let call_id = payload.get("call_id").and_then(|v| v.as_str())?;
                    let output = payload.get("output").cloned().unwrap_or(Value::Null);
                    Some(codex_message_value(serde_json::json!({
                        "type": "tool-call-result",
                        "callId": call_id,
                        "output": output,
                    })))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn normalize_path(p: &str) -> String {
    let path = PathBuf::from(p);
    let normalized = path.to_string_lossy().replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

fn parse_iso_timestamp_ms(ts: &str) -> Option<u64> {
    let dt = ts.parse::<chrono::DateTime<Utc>>().ok()?;
    Some(dt.timestamp_millis() as u64)
}

fn compute_date_prefixes(reference_ms: u64, window_ms: u64) -> HashSet<String> {
    let mut prefixes = HashSet::new();
    let center = Utc
        .timestamp_millis_opt(reference_ms as i64)
        .single()
        .unwrap_or_else(Utc::now);

    let window = Duration::milliseconds(window_ms as i64);
    let start = center - window;
    let end = center + window;

    let mut current = start.date_naive();
    let end_date = end.date_naive();
    while current <= end_date {
        prefixes.insert(current.format("%Y/%m/%d").to_string());
        current += Duration::days(1);
    }
    prefixes
}
