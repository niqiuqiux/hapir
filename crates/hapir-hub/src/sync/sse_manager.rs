use std::collections::HashMap;

use hapir_shared::schemas::SyncEvent;
use tokio::sync::mpsc;

use super::visibility_tracker::{VisibilityState, VisibilityTracker};

/// An SSE subscription descriptor.
#[derive(Debug, Clone)]
pub struct SseSubscription {
    pub id: String,
    pub namespace: String,
    pub all: bool,
    pub session_id: Option<String>,
    pub machine_id: Option<String>,
}

/// Internal connection state.
struct SseConnection {
    sub: SseSubscription,
    tx: mpsc::UnboundedSender<SseMessage>,
}

/// Messages sent to an SSE connection.
#[derive(Debug, Clone)]
pub enum SseMessage {
    Event(SyncEvent),
    Heartbeat,
}

/// Manages Server-Sent Events connections.
pub struct SseManager {
    connections: HashMap<String, SseConnection>,
    visibility: VisibilityTracker,
    heartbeat_ms: u64,
}

impl SseManager {
    pub fn new(heartbeat_ms: u64) -> Self {
        Self {
            connections: HashMap::new(),
            visibility: VisibilityTracker::new(),
            heartbeat_ms,
        }
    }

    // PLACEHOLDER_SSE_CONTINUE

    pub fn heartbeat_ms(&self) -> u64 {
        self.heartbeat_ms
    }

    pub fn visibility(&self) -> &VisibilityTracker {
        &self.visibility
    }

    pub fn visibility_mut(&mut self) -> &mut VisibilityTracker {
        &mut self.visibility
    }

    /// Subscribe a new SSE connection. Returns a receiver for events and the subscription info.
    pub fn subscribe(
        &mut self,
        id: String,
        namespace: String,
        all: bool,
        session_id: Option<String>,
        machine_id: Option<String>,
        visibility: VisibilityState,
    ) -> (mpsc::UnboundedReceiver<SseMessage>, SseSubscription) {
        let (tx, rx) = mpsc::unbounded_channel();

        let sub = SseSubscription {
            id: id.clone(),
            namespace: namespace.clone(),
            all,
            session_id,
            machine_id,
        };

        self.connections.insert(
            id.clone(),
            SseConnection {
                sub: sub.clone(),
                tx,
            },
        );
        self.visibility.register_connection(&id, &namespace, visibility);

        (rx, sub)
    }

    pub fn unsubscribe(&mut self, id: &str) {
        self.connections.remove(id);
        self.visibility.remove_connection(id);
    }

    /// Broadcast an event to all matching connections.
    /// Returns IDs of connections that failed delivery (for deferred cleanup).
    pub fn broadcast(&self, event: &SyncEvent) -> Vec<String> {
        let mut failed = Vec::new();
        for conn in self.connections.values() {
            if !self.should_send(&conn.sub, event) {
                continue;
            }
            if conn.tx.send(SseMessage::Event(event.clone())).is_err() {
                failed.push(conn.sub.id.clone());
            }
        }
        failed
    }

    /// Send a toast to visible connections in a namespace. Returns delivery count.
    /// Auto-unsubscribes connections that fail delivery.
    pub fn send_toast(&mut self, namespace: &str, event: &SyncEvent) -> usize {
        let mut count = 0;
        let mut failed = Vec::new();
        for conn in self.connections.values() {
            if conn.sub.namespace != namespace {
                continue;
            }
            if !self.visibility.is_visible_connection(&conn.sub.id) {
                continue;
            }
            if conn.tx.send(SseMessage::Event(event.clone())).is_ok() {
                count += 1;
            } else {
                failed.push(conn.sub.id.clone());
            }
        }
        for id in failed {
            self.unsubscribe(&id);
        }
        count
    }

    /// Send heartbeat to all connections. Auto-unsubscribes failed connections.
    pub fn send_heartbeats(&mut self) {
        let mut failed = Vec::new();
        for conn in self.connections.values() {
            if conn.tx.send(SseMessage::Heartbeat).is_err() {
                failed.push(conn.sub.id.clone());
            }
        }
        for id in failed {
            self.unsubscribe(&id);
        }
    }

    /// Clean up connections whose senders have been dropped.
    pub fn cleanup_dead(&mut self) {
        let dead: Vec<String> = self
            .connections
            .iter()
            .filter(|(_, conn)| conn.tx.is_closed())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dead {
            self.unsubscribe(&id);
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    fn should_send(&self, sub: &SseSubscription, event: &SyncEvent) -> bool {
        // connection-changed goes to everyone
        if matches!(event, SyncEvent::ConnectionChanged { .. }) {
            return true;
        }

        // Check namespace match
        let event_ns = event_namespace(event);
        if let Some(ns) = event_ns {
            if ns != sub.namespace {
                return false;
            }
        } else {
            return false;
        }

        // message-received only goes to the specific session subscriber
        if let SyncEvent::MessageReceived { session_id, .. } = event {
            return sub.session_id.as_deref() == Some(session_id.as_str());
        }

        // "all" subscribers get everything in their namespace
        if sub.all {
            return true;
        }

        // Check session/machine match
        if let Some(sid) = event_session_id(event) {
            if sub.session_id.as_deref() == Some(sid) {
                return true;
            }
        }
        if let Some(mid) = event_machine_id(event) {
            if sub.machine_id.as_deref() == Some(mid) {
                return true;
            }
        }

        false
    }
}

fn event_namespace(event: &SyncEvent) -> Option<&str> {
    match event {
        SyncEvent::SessionAdded { namespace, .. }
        | SyncEvent::SessionUpdated { namespace, .. }
        | SyncEvent::SessionRemoved { namespace, .. }
        | SyncEvent::MessageReceived { namespace, .. }
        | SyncEvent::MessageDelta { namespace, .. }
        | SyncEvent::MachineUpdated { namespace, .. }
        | SyncEvent::Toast { namespace, .. }
        | SyncEvent::ConnectionChanged { namespace, .. } => namespace.as_deref(),
    }
}

fn event_session_id(event: &SyncEvent) -> Option<&str> {
    match event {
        SyncEvent::SessionAdded { session_id, .. }
        | SyncEvent::SessionUpdated { session_id, .. }
        | SyncEvent::SessionRemoved { session_id, .. }
        | SyncEvent::MessageReceived { session_id, .. }
        | SyncEvent::MessageDelta { session_id, .. } => Some(session_id.as_str()),
        _ => None,
    }
}

fn event_machine_id(event: &SyncEvent) -> Option<&str> {
    match event {
        SyncEvent::MachineUpdated { machine_id, .. } => Some(machine_id.as_str()),
        _ => None,
    }
}
