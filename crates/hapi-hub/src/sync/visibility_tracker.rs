use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibilityState {
    Visible,
    Hidden,
}

/// Tracks which SSE connections are "visible" (browser tab in foreground).
pub struct VisibilityTracker {
    /// namespace → set of visible subscription IDs
    visible_connections: HashMap<String, HashSet<String>>,
    /// subscription ID → namespace
    subscription_to_namespace: HashMap<String, String>,
}

impl VisibilityTracker {
    pub fn new() -> Self {
        Self {
            visible_connections: HashMap::new(),
            subscription_to_namespace: HashMap::new(),
        }
    }

    pub fn register_connection(&mut self, subscription_id: &str, namespace: &str, state: VisibilityState) {
        self.remove_connection(subscription_id);
        self.subscription_to_namespace.insert(subscription_id.to_string(), namespace.to_string());
        if state == VisibilityState::Visible {
            self.add_visible(namespace, subscription_id);
        }
    }

    pub fn set_visibility(&mut self, subscription_id: &str, namespace: &str, state: VisibilityState) -> bool {
        let tracked = match self.subscription_to_namespace.get(subscription_id) {
            Some(ns) if ns == namespace => ns.clone(),
            _ => return false,
        };

        if state == VisibilityState::Visible {
            self.add_visible(&tracked, subscription_id);
        } else {
            self.remove_visible(&tracked, subscription_id);
        }
        true
    }

    pub fn remove_connection(&mut self, subscription_id: &str) {
        if let Some(namespace) = self.subscription_to_namespace.remove(subscription_id) {
            self.remove_visible(&namespace, subscription_id);
        }
    }

    pub fn has_visible_connection(&self, namespace: &str) -> bool {
        self.visible_connections
            .get(namespace)
            .is_some_and(|s| !s.is_empty())
    }

    pub fn is_visible_connection(&self, subscription_id: &str) -> bool {
        let namespace = match self.subscription_to_namespace.get(subscription_id) {
            Some(ns) => ns,
            None => return false,
        };
        self.visible_connections
            .get(namespace.as_str())
            .is_some_and(|s| s.contains(subscription_id))
    }

    fn add_visible(&mut self, namespace: &str, subscription_id: &str) {
        self.visible_connections
            .entry(namespace.to_string())
            .or_default()
            .insert(subscription_id.to_string());
    }

    fn remove_visible(&mut self, namespace: &str, subscription_id: &str) {
        if let Some(set) = self.visible_connections.get_mut(namespace) {
            set.remove(subscription_id);
            if set.is_empty() {
                self.visible_connections.remove(namespace);
            }
        }
    }
}
