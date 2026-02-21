use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibilityState {
    Visible,
    Hidden,
}

struct VisibilityInner {
    /// namespace → set of visible subscription IDs
    visible_connections: HashMap<String, HashSet<String>>,
    /// subscription ID → namespace
    subscription_to_namespace: HashMap<String, String>,
}

/// Tracks which SSE connections are "visible" (browser tab in foreground).
/// Uses internal locking so all public methods take `&self`.
pub struct VisibilityTracker {
    inner: RwLock<VisibilityInner>,
}

impl VisibilityTracker {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(VisibilityInner {
                visible_connections: HashMap::new(),
                subscription_to_namespace: HashMap::new(),
            }),
        }
    }

    pub fn register_connection(
        &self,
        subscription_id: &str,
        namespace: &str,
        state: VisibilityState,
    ) {
        let mut inner = self.inner.write().unwrap();
        inner.remove_connection(subscription_id);
        inner
            .subscription_to_namespace
            .insert(subscription_id.to_string(), namespace.to_string());
        if state == VisibilityState::Visible {
            inner.add_visible(namespace, subscription_id);
        }
    }

    pub fn set_visibility(
        &self,
        subscription_id: &str,
        namespace: &str,
        state: VisibilityState,
    ) -> bool {
        let mut inner = self.inner.write().unwrap();
        let tracked = match inner.subscription_to_namespace.get(subscription_id) {
            Some(ns) if ns == namespace => ns.clone(),
            _ => return false,
        };

        if state == VisibilityState::Visible {
            inner.add_visible(&tracked, subscription_id);
        } else {
            inner.remove_visible(&tracked, subscription_id);
        }
        true
    }

    pub fn remove_connection(&self, subscription_id: &str) {
        self.inner
            .write()
            .unwrap()
            .remove_connection(subscription_id);
    }

    pub fn has_visible_connection(&self, namespace: &str) -> bool {
        let inner = self.inner.read().unwrap();
        inner
            .visible_connections
            .get(namespace)
            .is_some_and(|s| !s.is_empty())
    }

    pub fn is_visible_connection(&self, subscription_id: &str) -> bool {
        let inner = self.inner.read().unwrap();
        let namespace = match inner.subscription_to_namespace.get(subscription_id) {
            Some(ns) => ns,
            None => return false,
        };
        inner
            .visible_connections
            .get(namespace.as_str())
            .is_some_and(|s| s.contains(subscription_id))
    }
}

impl VisibilityInner {
    fn remove_connection(&mut self, subscription_id: &str) {
        if let Some(namespace) = self.subscription_to_namespace.remove(subscription_id) {
            self.remove_visible(&namespace, subscription_id);
        }
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
