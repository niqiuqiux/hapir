use std::collections::HashMap;
use std::sync::Mutex;

use crate::agent::types::{AgentBackend, AgentBackendFactory};

/// Global registry of agent backend factories.
pub struct AgentRegistry {
    factories: Mutex<HashMap<String, AgentBackendFactory>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            factories: Mutex::new(HashMap::new()),
        }
    }

    /// Register a factory for the given agent type.
    pub fn register(&self, agent_type: &str, factory: AgentBackendFactory) {
        assert!(
            !agent_type.is_empty(),
            "Agent type must be a non-empty string"
        );
        self.factories
            .lock()
            .unwrap()
            .insert(agent_type.to_string(), factory);
    }

    /// Create an agent backend instance for the given type.
    pub fn create(&self, agent_type: &str) -> anyhow::Result<Box<dyn AgentBackend>> {
        let factories = self.factories.lock().unwrap();
        let factory = factories
            .get(agent_type)
            .ok_or_else(|| anyhow::anyhow!("Unknown agent type: {agent_type}"))?;
        Ok(factory())
    }

    /// List all registered agent types, sorted alphabetically.
    pub fn list(&self) -> Vec<String> {
        let factories = self.factories.lock().unwrap();
        let mut keys: Vec<String> = factories.keys().cloned().collect();
        keys.sort();
        keys
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}
