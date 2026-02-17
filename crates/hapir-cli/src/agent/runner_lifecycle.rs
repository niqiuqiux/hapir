use std::process;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::debug;

use crate::ws::session_client::WsSessionClient;

/// Options for creating a RunnerLifecycle.
pub struct RunnerLifecycleOptions {
    pub ws_client: Arc<WsSessionClient>,
    pub log_tag: String,
    pub stop_keep_alive: Option<Box<dyn Fn() + Send + Sync>>,
    pub on_before_close: Option<Box<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>>,
    pub on_after_close: Option<Box<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>>,
}

/// Manages the lifecycle of a runner process: exit codes, archiving, cleanup, signal handling.
pub struct RunnerLifecycle {
    exit_code: Arc<Mutex<i32>>,
    archive_reason: Arc<Mutex<String>>,
    cleanup_started: Arc<Mutex<bool>>,
    ws_client: Arc<WsSessionClient>,
    log_tag: String,
    stop_keep_alive: Option<Arc<dyn Fn() + Send + Sync>>,
    on_before_close: Option<Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>>,
    on_after_close: Option<Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>>,
}

impl RunnerLifecycle {
    pub fn new(opts: RunnerLifecycleOptions) -> Arc<Self> {
        Arc::new(Self {
            exit_code: Arc::new(Mutex::new(0)),
            archive_reason: Arc::new(Mutex::new("User terminated".to_string())),
            cleanup_started: Arc::new(Mutex::new(false)),
            ws_client: opts.ws_client,
            log_tag: opts.log_tag,
            stop_keep_alive: opts.stop_keep_alive.map(Arc::from),
            on_before_close: opts.on_before_close.map(Arc::from),
            on_after_close: opts.on_after_close.map(Arc::from),
        })
    }

    pub async fn set_exit_code(&self, code: i32) {
        *self.exit_code.lock().await = code;
    }

    pub async fn set_archive_reason(&self, reason: &str) {
        *self.archive_reason.lock().await = reason.to_string();
    }
    pub async fn mark_crash(&self, error: &str) {
        debug!("[{}] Unhandled error: {}", self.log_tag, error);
        *self.exit_code.lock().await = 1;
        *self.archive_reason.lock().await = "Session crashed".to_string();
    }

    async fn archive_and_close(&self) {
        let reason = self.archive_reason.lock().await.clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as f64;

        let _ = self
            .ws_client
            .update_metadata(move |mut metadata| {
                metadata.lifecycle_state = Some("archived".to_string());
                metadata.lifecycle_state_since = Some(now);
                metadata.archived_by = Some("cli".to_string());
                metadata.archive_reason = Some(reason);
                metadata
            })
            .await;

        self.ws_client.send_session_end().await;
        self.ws_client.close().await;
    }

    pub async fn cleanup(&self) {
        {
            let mut started = self.cleanup_started.lock().await;
            if *started {
                return;
            }
            *started = true;
        }

        debug!("[{}] Cleanup start", self.log_tag);

        if let Some(ref stop) = self.stop_keep_alive {
            stop();
        }

        if let Some(ref before) = self.on_before_close {
            before().await;
        }

        self.archive_and_close().await;

        debug!("[{}] Cleanup complete", self.log_tag);

        if let Some(ref after) = self.on_after_close {
            after().await;
        }
    }

    pub async fn cleanup_and_exit(&self, code_override: Option<i32>) {
        if let Some(code) = code_override {
            *self.exit_code.lock().await = code;
        }

        self.cleanup().await;
        let code = *self.exit_code.lock().await;
        process::exit(code);
    }

    /// Register SIGTERM and SIGINT handlers that trigger cleanup.
    pub fn register_process_handlers(self: &Arc<Self>) {
        let lifecycle = self.clone();
        tokio::spawn(async move {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            let mut sigint =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                    .expect("failed to register SIGINT handler");

            tokio::select! {
                _ = sigterm.recv() => {
                    lifecycle.cleanup_and_exit(None).await;
                }
                _ = sigint.recv() => {
                    lifecycle.cleanup_and_exit(None).await;
                }
            }
        });
    }
}

/// Set the `controlledByUser` flag on the agent state based on mode.
pub async fn set_controlled_by_user(ws_client: &WsSessionClient, mode: super::session_base::SessionMode) {
    let controlled = mode == super::session_base::SessionMode::Local;
    let _ = ws_client
        .update_agent_state(move |mut state| {
            state["controlledByUser"] = serde_json::json!(controlled);
            state
        })
        .await;
}

/// Create a mode-change handler that sends a switch event and updates controlledByUser.
pub fn create_mode_change_handler(
    ws_client: Arc<WsSessionClient>,
) -> Box<dyn Fn(super::session_base::SessionMode) + Send + Sync> {
    Box::new(move |mode| {
        let client = ws_client.clone();
        tokio::spawn(async move {
            let mode_str = mode.as_str();
            client
                .send_message(serde_json::json!({
                    "type": "switch",
                    "mode": mode_str,
                }))
                .await;
            set_controlled_by_user(&client, mode).await;
        });
    })
}
