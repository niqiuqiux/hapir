use std::collections::{HashMap, HashSet};

use tracing::debug;

/// Stub permission handler for Claude sessions.
///
/// Tracks tool calls and manages allowed tools. The full implementation
/// will integrate with the SDK's canCallTool callback.
pub struct PermissionHandler {
    tool_calls: Vec<TrackedToolCall>,
    responses: HashMap<String, PermissionResponseEntry>,
    allowed_tools: HashSet<String>,
    allowed_bash_literals: HashSet<String>,
    allowed_bash_prefixes: HashSet<String>,
    permission_mode: String,
}

struct TrackedToolCall {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub input: serde_json::Value,
    #[allow(dead_code)]
    pub used: bool,
}

#[derive(Debug, Clone)]
pub struct PermissionResponseEntry {
    pub id: String,
    pub approved: bool,
    pub reason: Option<String>,
    pub mode: Option<String>,
    pub allow_tools: Option<Vec<String>>,
    pub received_at: Option<u64>,
}

impl PermissionHandler {
    pub fn new() -> Self {
        Self {
            tool_calls: Vec::new(),
            responses: HashMap::new(),
            allowed_tools: HashSet::new(),
            allowed_bash_literals: HashSet::new(),
            allowed_bash_prefixes: HashSet::new(),
            permission_mode: "default".to_string(),
        }
    }

    pub fn handle_mode_change(&mut self, mode: &str) {
        self.permission_mode = mode.to_string();
    }

    /// Reset all state for new sessions.
    pub fn reset(&mut self) {
        self.tool_calls.clear();
        self.responses.clear();
        self.allowed_tools.clear();
        self.allowed_bash_literals.clear();
        self.allowed_bash_prefixes.clear();
        debug!("[PermissionHandler] State reset");
    }

    /// Check if a tool call was rejected.
    pub fn is_aborted(&self, tool_call_id: &str) -> bool {
        if let Some(resp) = self.responses.get(tool_call_id)
            && !resp.approved
        {
            return true;
        }
        // Always abort exit_plan_mode
        self.tool_calls.iter().any(|tc| {
            tc.id == tool_call_id && (tc.name == "exit_plan_mode" || tc.name == "ExitPlanMode")
        })
    }

    pub fn get_responses(&self) -> &HashMap<String, PermissionResponseEntry> {
        &self.responses
    }
}

impl Default for PermissionHandler {
    fn default() -> Self {
        Self::new()
    }
}
