use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::debug;

use hapir_infra::ws::session_client::WsSessionClient;

/// Trait for scanning local agent session files and converting them to hub messages.
pub trait LocalSessionScanner: Send + 'static {
    /// Scan session files and return new messages as JSON values ready to send.
    fn scan(&mut self) -> impl std::future::Future<Output = Vec<Value>> + Send;

    /// Notify the scanner of a new session ID.
    fn on_new_session(&mut self, session_id: &str);

    /// Clean up resources.
    fn cleanup(&mut self) -> impl std::future::Future<Output = ()> + Send;
}

/// Drives periodic scanning of local session files and forwards messages to the hub.
pub struct LocalSyncDriver {
    handle: Option<JoinHandle<()>>,
}

impl LocalSyncDriver {
    /// Start the sync driver. Spawns a tokio task that periodically calls
    /// `scanner.scan()` and sends each returned value via `ws_client.send_message()`.
    pub fn start<S: LocalSessionScanner>(
        scanner: Arc<Mutex<S>>,
        ws_client: Arc<WsSessionClient>,
        interval: Duration,
        log_tag: &str,
    ) -> Self {
        let tag = log_tag.to_string();

        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                ticker.tick().await;

                let messages = scanner.lock().await.scan().await;
                if !messages.is_empty() {
                    debug!("[{}] Scanned {} new message(s)", tag, messages.len());
                }
                for msg in messages {
                    ws_client.send_message(msg).await;
                }
            }
        });

        Self {
            handle: Some(handle),
        }
    }

    /// Stop the sync driver.
    pub fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }

    /// Do a final scan to capture any remaining messages, then cleanup.
    pub async fn final_flush<S: LocalSessionScanner>(
        scanner: &Mutex<S>,
        ws_client: &WsSessionClient,
        log_tag: &str,
    ) {
        let mut guard = scanner.lock().await;
        let messages = guard.scan().await;
        if !messages.is_empty() {
            debug!(
                "[{}] Final flush: {} remaining message(s)",
                log_tag,
                messages.len()
            );
            for msg in messages {
                ws_client.send_message(msg).await;
            }
        }
        guard.cleanup().await;
    }
}

impl Drop for LocalSyncDriver {
    fn drop(&mut self) {
        self.stop();
    }
}
