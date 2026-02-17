use std::collections::{HashMap, HashSet};
use tokio::time::{Duration, Instant};

/// A registered terminal connection.
#[derive(Debug, Clone)]
pub struct TerminalEntry {
    pub terminal_id: String,
    pub session_id: String,
    /// The webapp/terminal WebSocket connection ID
    pub socket_id: String,
    /// The CLI WebSocket connection ID that owns the PTY
    pub cli_socket_id: String,
    pub last_activity: Instant,
}

pub struct TerminalRegistry {
    terminals: HashMap<String, TerminalEntry>,
    by_socket: HashMap<String, HashSet<String>>,
    by_session: HashMap<String, HashSet<String>>,
    by_cli_socket: HashMap<String, HashSet<String>>,
    idle_timeout: Duration,
}

impl TerminalRegistry {
    pub fn new(idle_timeout_ms: u64) -> Self {
        Self {
            terminals: HashMap::new(),
            by_socket: HashMap::new(),
            by_session: HashMap::new(),
            by_cli_socket: HashMap::new(),
            idle_timeout: Duration::from_millis(idle_timeout_ms),
        }
    }

    pub fn register(
        &mut self,
        terminal_id: &str,
        session_id: &str,
        socket_id: &str,
        cli_socket_id: &str,
    ) -> Option<TerminalEntry> {
        if self.terminals.contains_key(terminal_id) {
            return None;
        }

        let entry = TerminalEntry {
            terminal_id: terminal_id.to_string(),
            session_id: session_id.to_string(),
            socket_id: socket_id.to_string(),
            cli_socket_id: cli_socket_id.to_string(),
            last_activity: Instant::now(),
        };

        self.terminals.insert(terminal_id.to_string(), entry.clone());
        self.by_socket
            .entry(socket_id.to_string())
            .or_default()
            .insert(terminal_id.to_string());
        self.by_session
            .entry(session_id.to_string())
            .or_default()
            .insert(terminal_id.to_string());
        self.by_cli_socket
            .entry(cli_socket_id.to_string())
            .or_default()
            .insert(terminal_id.to_string());

        Some(entry)
    }

    pub fn mark_activity(&mut self, terminal_id: &str) {
        if let Some(entry) = self.terminals.get_mut(terminal_id) {
            entry.last_activity = Instant::now();
        }
    }

    pub fn get(&self, terminal_id: &str) -> Option<&TerminalEntry> {
        self.terminals.get(terminal_id)
    }

    pub fn remove(&mut self, terminal_id: &str) -> Option<TerminalEntry> {
        let entry = self.terminals.remove(terminal_id)?;
        Self::remove_from_index(&mut self.by_socket, &entry.socket_id, terminal_id);
        Self::remove_from_index(&mut self.by_session, &entry.session_id, terminal_id);
        Self::remove_from_index(&mut self.by_cli_socket, &entry.cli_socket_id, terminal_id);
        Some(entry)
    }

    pub fn remove_by_socket(&mut self, socket_id: &str) -> Vec<TerminalEntry> {
        let ids: Vec<String> = self
            .by_socket
            .get(socket_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        ids.iter().filter_map(|id| self.remove(id)).collect()
    }

    pub fn remove_by_cli_socket(&mut self, cli_socket_id: &str) -> Vec<TerminalEntry> {
        let ids: Vec<String> = self
            .by_cli_socket
            .get(cli_socket_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        ids.iter().filter_map(|id| self.remove(id)).collect()
    }

    pub fn count_for_socket(&self, socket_id: &str) -> usize {
        self.by_socket.get(socket_id).map(|s| s.len()).unwrap_or(0)
    }

    pub fn count_for_session(&self, session_id: &str) -> usize {
        self.by_session.get(session_id).map(|s| s.len()).unwrap_or(0)
    }

    /// Returns terminal IDs that have been idle beyond the timeout.
    pub fn collect_idle(&self) -> Vec<String> {
        let now = Instant::now();
        self.terminals
            .iter()
            .filter(|(_, e)| now.duration_since(e.last_activity) > self.idle_timeout)
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn remove_from_index(index: &mut HashMap<String, HashSet<String>>, key: &str, terminal_id: &str) {
        if let Some(set) = index.get_mut(key) {
            set.remove(terminal_id);
            if set.is_empty() {
                index.remove(key);
            }
        }
    }
}
