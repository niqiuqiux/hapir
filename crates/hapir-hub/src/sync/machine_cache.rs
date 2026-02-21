use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use hapir_shared::schemas::SyncEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::alive_time::clamp_alive_time;
use super::event_publisher::EventPublisher;
use crate::store::Store;

const MACHINE_TIMEOUT_MS: i64 = 45_000;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineMetadata {
    pub host: String,
    pub platform: String,
    pub happy_cli_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_home_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub happy_lib_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: String,
    pub namespace: String,
    pub seq: f64,
    pub created_at: f64,
    pub updated_at: f64,
    pub active: bool,
    pub active_at: f64,
    pub metadata: Option<MachineMetadata>,
    pub metadata_version: f64,
    pub runner_state: Option<Value>,
    pub runner_state_version: f64,
}

pub struct MachineCache {
    machines: HashMap<String, Machine>,
    last_broadcast_at: HashMap<String, i64>,
}

impl MachineCache {
    pub fn new() -> Self {
        Self {
            machines: HashMap::new(),
            last_broadcast_at: HashMap::new(),
        }
    }

    pub fn get_machines(&self) -> Vec<Machine> {
        self.machines.values().cloned().collect()
    }

    pub fn get_machines_by_namespace(&self, namespace: &str) -> Vec<Machine> {
        self.machines
            .values()
            .filter(|m| m.namespace == namespace)
            .cloned()
            .collect()
    }

    pub fn get_machine(&self, machine_id: &str) -> Option<&Machine> {
        self.machines.get(machine_id)
    }

    pub fn get_machine_by_namespace(&self, machine_id: &str, namespace: &str) -> Option<&Machine> {
        self.machines
            .get(machine_id)
            .filter(|m| m.namespace == namespace)
    }

    pub fn get_online_machines(&self) -> Vec<Machine> {
        self.machines
            .values()
            .filter(|m| m.active)
            .cloned()
            .collect()
    }

    pub fn get_online_machines_by_namespace(&self, namespace: &str) -> Vec<Machine> {
        self.machines
            .values()
            .filter(|m| m.active && m.namespace == namespace)
            .cloned()
            .collect()
    }

    pub fn get_or_create_machine(
        &mut self,
        id: &str,
        metadata: &Value,
        runner_state: Option<&Value>,
        namespace: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> anyhow::Result<Machine> {
        use crate::store::machines;
        let stored =
            machines::get_or_create_machine(&store.conn(), id, metadata, runner_state, namespace)?;
        self.refresh_machine(&stored.id, store, publisher)
            .ok_or_else(|| anyhow::anyhow!("failed to load machine"))
    }

    pub fn refresh_machine(
        &mut self,
        machine_id: &str,
        store: &Store,
        publisher: &EventPublisher,
    ) -> Option<Machine> {
        use crate::store::machines as store_machines;

        let stored = match store_machines::get_machine(&store.conn(), machine_id) {
            Some(m) => m,
            None => {
                if let Some(removed) = self.machines.remove(machine_id) {
                    publisher.emit(SyncEvent::MachineUpdated {
                        machine_id: machine_id.to_string(),
                        namespace: Some(removed.namespace),
                        data: None,
                    });
                }
                return None;
            }
        };

        let existing = self.machines.get(machine_id);

        // Parse metadata with defaults
        let metadata: Option<MachineMetadata> = stored.metadata.as_ref().and_then(|v| {
            let obj = v.as_object()?;
            Some(MachineMetadata {
                host: obj
                    .get("host")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                platform: obj
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                happy_cli_version: obj
                    .get("happyCliVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                display_name: obj
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                home_dir: obj
                    .get("homeDir")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                happy_home_dir: obj
                    .get("happyHomeDir")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                happy_lib_dir: obj
                    .get("happyLibDir")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            })
        });

        let stored_active_at = stored.active_at.unwrap_or(stored.created_at);
        let existing_active_at = existing.map(|e| e.active_at as i64).unwrap_or(0);
        let use_stored = stored_active_at > existing_active_at;

        let machine = Machine {
            id: stored.id.clone(),
            namespace: stored.namespace.clone(),
            seq: stored.seq as f64,
            created_at: stored.created_at as f64,
            updated_at: stored.updated_at as f64,
            active: if use_stored {
                stored.active
            } else {
                existing.map(|e| e.active).unwrap_or(stored.active)
            },
            active_at: if use_stored {
                stored_active_at as f64
            } else if existing_active_at > 0 {
                existing_active_at as f64
            } else {
                stored_active_at as f64
            },
            metadata,
            metadata_version: stored.metadata_version as f64,
            runner_state: stored.runner_state,
            runner_state_version: stored.runner_state_version as f64,
        };

        self.machines
            .insert(machine_id.to_string(), machine.clone());
        let ns = machine.namespace.clone();
        let data = serde_json::to_value(&machine).ok();
        publisher.emit(SyncEvent::MachineUpdated {
            machine_id: machine_id.to_string(),
            namespace: Some(ns),
            data,
        });

        Some(machine)
    }

    pub fn reload_all(&mut self, store: &Store, publisher: &EventPublisher) {
        use crate::store::machines;
        let all = machines::get_machines(&store.conn());
        for m in all {
            self.refresh_machine(&m.id, store, publisher);
        }
    }

    pub fn handle_machine_alive(
        &mut self,
        machine_id: &str,
        time: i64,
        store: &Store,
        publisher: &EventPublisher,
    ) {
        let t = match clamp_alive_time(time) {
            Some(t) => t,
            None => return,
        };

        if !self.machines.contains_key(machine_id) {
            self.refresh_machine(machine_id, store, publisher);
        }
        let machine = match self.machines.get_mut(machine_id) {
            Some(m) => m,
            None => return,
        };

        let was_active = machine.active;
        machine.active = true;
        machine.active_at = machine.active_at.max(t as f64);

        let now = now_millis();
        let last = self.last_broadcast_at.get(machine_id).copied().unwrap_or(0);
        let should_broadcast = (!was_active && machine.active) || (now - last > 10_000);

        if should_broadcast {
            self.last_broadcast_at.insert(machine_id.to_string(), now);
            publisher.emit(SyncEvent::MachineUpdated {
                machine_id: machine_id.to_string(),
                namespace: Some(machine.namespace.clone()),
                data: Some(serde_json::json!({"active": true, "activeAt": machine.active_at})),
            });
        }
    }

    pub fn expire_inactive(&mut self, publisher: &EventPublisher) {
        let now = now_millis();
        let expired: Vec<String> = self
            .machines
            .iter()
            .filter(|(_, m)| m.active && (now - m.active_at as i64) > MACHINE_TIMEOUT_MS)
            .map(|(id, _)| id.clone())
            .collect();

        if !expired.is_empty() {
            tracing::info!(count = expired.len(), "expiring inactive machines");
        }
        for id in expired {
            tracing::debug!(machine_id = %id, "machine expired due to inactivity");
            self.mark_machine_offline(&id, publisher);
        }
    }

    /// Mark a specific machine as offline (e.g. when its WebSocket disconnects).
    pub fn mark_machine_offline(&mut self, machine_id: &str, publisher: &EventPublisher) {
        if let Some(machine) = self.machines.get_mut(machine_id)
            && machine.active
        {
            machine.active = false;
            publisher.emit(SyncEvent::MachineUpdated {
                machine_id: machine_id.to_string(),
                namespace: Some(machine.namespace.clone()),
                data: Some(serde_json::json!({"active": false})),
            });
        }
    }
}
