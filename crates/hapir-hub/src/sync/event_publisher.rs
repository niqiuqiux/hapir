use std::sync::RwLock;

use hapir_shared::schemas::SyncEvent;
use tokio::sync::{broadcast, mpsc};

use super::sse_manager::{SseManager, SseMessage, SseSubscription};
use super::visibility_tracker::VisibilityState;

pub type SyncEventListener = Box<dyn Fn(&SyncEvent) + Send + Sync>;

/// Publishes sync events to listeners and SSE connections.
/// Uses a tokio broadcast channel for fan-out.
/// SSE manager is wrapped in an RwLock so that `emit()` only requires `&self`.
pub struct EventPublisher {
    tx: broadcast::Sender<SyncEvent>,
    sse_manager: RwLock<SseManager>,
    heartbeat_ms: u64,
}

impl EventPublisher {
    pub fn new(sse_manager: SseManager) -> Self {
        let (tx, _) = broadcast::channel(256);
        let heartbeat_ms = sse_manager.heartbeat_ms();
        Self {
            tx,
            sse_manager: RwLock::new(sse_manager),
            heartbeat_ms,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.tx.subscribe()
    }

    // --- SSE delegate methods ---

    pub fn subscribe_sse(
        &self,
        id: String,
        namespace: String,
        all: bool,
        session_id: Option<String>,
        machine_id: Option<String>,
        visibility: VisibilityState,
    ) -> (mpsc::UnboundedReceiver<SseMessage>, SseSubscription) {
        let mut mgr = self.sse_manager.write().unwrap();
        mgr.subscribe(id, namespace, all, session_id, machine_id, visibility)
    }

    pub fn unsubscribe_sse(&self, id: &str) {
        let mut mgr = self.sse_manager.write().unwrap();
        mgr.unsubscribe(id);
    }

    pub fn sse_connection_count(&self) -> usize {
        let mgr = self.sse_manager.read().unwrap();
        mgr.connection_count()
    }

    pub fn has_visible_sse_connection(&self, namespace: &str) -> bool {
        let mgr = self.sse_manager.read().unwrap();
        mgr.visibility().has_visible_connection(namespace)
    }

    pub fn set_sse_visibility(
        &self,
        subscription_id: &str,
        namespace: &str,
        state: VisibilityState,
    ) -> bool {
        let mut mgr = self.sse_manager.write().unwrap();
        mgr.visibility_mut().set_visibility(subscription_id, namespace, state)
    }

    pub fn send_heartbeats(&self) {
        let mut mgr = self.sse_manager.write().unwrap();
        mgr.send_heartbeats();
    }

    pub fn send_toast(&self, namespace: &str, event: &SyncEvent) -> usize {
        let mut mgr = self.sse_manager.write().unwrap();
        mgr.send_toast(namespace, event)
    }

    pub fn heartbeat_ms(&self) -> u64 {
        self.heartbeat_ms
    }

    // --- Event emission ---

    pub fn emit(&self, event: SyncEvent) {
        // Broadcast to SSE connections and clean up failed ones
        let failed = {
            let mgr = self.sse_manager.read().unwrap();
            mgr.broadcast(&event)
        };
        if !failed.is_empty() {
            let mut mgr = self.sse_manager.write().unwrap();
            for id in failed {
                mgr.unsubscribe(&id);
            }
        }

        // Broadcast to channel subscribers
        let _ = self.tx.send(event);
    }

    pub fn emit_with_namespace(&self, mut event: SyncEvent, namespace: Option<String>) {
        if let Some(ns) = namespace {
            set_event_namespace(&mut event, Some(ns));
        }
        self.emit(event);
    }
}

/// Helper to set the namespace on a SyncEvent.
fn set_event_namespace(event: &mut SyncEvent, namespace: Option<String>) {
    match event {
        SyncEvent::SessionAdded { namespace: ns, .. }
        | SyncEvent::SessionUpdated { namespace: ns, .. }
        | SyncEvent::SessionRemoved { namespace: ns, .. }
        | SyncEvent::MessageReceived { namespace: ns, .. }
        | SyncEvent::MessageDelta { namespace: ns, .. }
        | SyncEvent::MachineUpdated { namespace: ns, .. }
        | SyncEvent::Toast { namespace: ns, .. }
        | SyncEvent::ConnectionChanged { namespace: ns, .. } => {
            *ns = namespace;
        }
    }
}
