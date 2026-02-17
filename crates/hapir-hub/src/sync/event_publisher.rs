use hapir_shared::schemas::SyncEvent;
use tokio::sync::broadcast;

use super::sse_manager::SseManager;

pub type SyncEventListener = Box<dyn Fn(&SyncEvent) + Send + Sync>;

/// Publishes sync events to listeners and SSE connections.
/// Uses a tokio broadcast channel for fan-out.
pub struct EventPublisher {
    tx: broadcast::Sender<SyncEvent>,
    sse_manager: SseManager,
}

impl EventPublisher {
    pub fn new(sse_manager: SseManager) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self { tx, sse_manager }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.tx.subscribe()
    }

    pub fn sse_manager(&self) -> &SseManager {
        &self.sse_manager
    }

    pub fn sse_manager_mut(&mut self) -> &mut SseManager {
        &mut self.sse_manager
    }

    pub fn emit(&self, event: SyncEvent) {
        // Broadcast to SSE connections
        self.sse_manager.broadcast(&event);

        // Broadcast to channel subscribers
        if let Err(e) = self.tx.send(event) {
            // No receivers — that's fine, just means nobody is listening
            let _ = e;
        }
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
        | SyncEvent::MachineUpdated { namespace: ns, .. }
        | SyncEvent::Toast { namespace: ns, .. }
        | SyncEvent::ConnectionChanged { namespace: ns, .. } => {
            *ns = namespace;
        }
    }
}
