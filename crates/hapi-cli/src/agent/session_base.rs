use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tracing::debug;

use hapi_shared::modes::{ModelMode, PermissionMode};

use crate::api::ApiClient;
use crate::utils::message_queue::MessageQueue2;
use crate::ws::session_client::WsSessionClient;

/// The local/remote mode of the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Local,
    Remote,
}

impl SessionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

/// Callback invoked when a session ID is discovered.
pub type SessionFoundCallback = Box<dyn Fn(&str) + Send + Sync>;

/// Callback invoked when the mode changes.
pub type ModeChangeCallback = Box<dyn Fn(SessionMode) + Send + Sync>;

/// Applies a session ID to metadata (flavor-specific).
pub type ApplySessionIdFn =
    Box<dyn Fn(hapi_shared::schemas::Metadata, &str) -> hapi_shared::schemas::Metadata + Send + Sync>;

/// Options for constructing an AgentSessionBase.
pub struct AgentSessionBaseOptions<Mode: Clone + Send + 'static> {
    pub api: Arc<ApiClient>,
    pub ws_client: Arc<WsSessionClient>,
    pub path: String,
    pub log_path: String,
    pub session_id: Option<String>,
    pub queue: Arc<MessageQueue2<Mode>>,
    pub on_mode_change_cb: ModeChangeCallback,
    pub mode: SessionMode,
    pub session_label: String,
    pub session_id_label: String,
    pub apply_session_id_to_metadata: ApplySessionIdFn,
    pub permission_mode: Option<PermissionMode>,
    pub model_mode: Option<ModelMode>,
}

/// Generic session base that manages keep-alive, mode, thinking state,
/// and session-found callbacks. Parameterized over the queue's Mode type.
pub struct AgentSessionBase<Mode: Clone + Send + 'static> {
    pub path: String,
    pub log_path: String,
    pub session_id: Arc<Mutex<Option<String>>>,
    pub mode: Arc<Mutex<SessionMode>>,
    pub thinking: Arc<Mutex<bool>>,
    pub api: Arc<ApiClient>,
    pub ws_client: Arc<WsSessionClient>,
    pub queue: Arc<MessageQueue2<Mode>>,

    session_found_callbacks: Arc<Mutex<Vec<SessionFoundCallback>>>,
    on_mode_change_cb: Arc<ModeChangeCallback>,
    apply_session_id_to_metadata: Arc<ApplySessionIdFn>,
    session_label: String,
    session_id_label: String,
    keep_alive_handle: std::sync::Mutex<Option<JoinHandle<()>>>,
    permission_mode: Arc<Mutex<Option<PermissionMode>>>,
    model_mode: Arc<Mutex<Option<ModelMode>>>,
}

impl<Mode: Clone + Send + 'static> AgentSessionBase<Mode> {
    /// Create a new session base and start the keep-alive interval.
    pub fn new(opts: AgentSessionBaseOptions<Mode>) -> Arc<Self> {
        let session = Arc::new(Self {
            path: opts.path,
            log_path: opts.log_path,
            session_id: Arc::new(Mutex::new(opts.session_id)),
            mode: Arc::new(Mutex::new(opts.mode)),
            thinking: Arc::new(Mutex::new(false)),
            api: opts.api,
            ws_client: opts.ws_client,
            queue: opts.queue,
            session_found_callbacks: Arc::new(Mutex::new(Vec::new())),
            on_mode_change_cb: Arc::new(opts.on_mode_change_cb),
            apply_session_id_to_metadata: Arc::new(opts.apply_session_id_to_metadata),
            session_label: opts.session_label,
            session_id_label: opts.session_id_label,
            keep_alive_handle: std::sync::Mutex::new(None),
            permission_mode: Arc::new(Mutex::new(opts.permission_mode)),
            model_mode: Arc::new(Mutex::new(opts.model_mode)),
        });

        // Send initial keep-alive and start interval
        let s = session.clone();
        let handle = tokio::spawn(async move {
            // Initial keep-alive
            s.send_keep_alive().await;

            let mut interval = time::interval(Duration::from_secs(2));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                s.send_keep_alive().await;
            }
        });

        // Store the handle so we can cancel later
        *session.keep_alive_handle.lock().unwrap() = Some(handle);

        session
    }

    async fn send_keep_alive(&self) {
        let thinking = *self.thinking.lock().await;
        let mode = self.mode.lock().await.as_str().to_string();
        let runtime = self.get_keep_alive_runtime().await;
        self.ws_client.keep_alive(thinking, &mode, &runtime).await;
    }

    async fn get_keep_alive_runtime(&self) -> String {
        let pm = *self.permission_mode.lock().await;
        let mm = *self.model_mode.lock().await;
        if pm.is_none() && mm.is_none() {
            return String::new();
        }
        let mut obj = serde_json::Map::new();
        if let Some(pm) = pm {
            if let Ok(v) = serde_json::to_value(pm) {
                obj.insert("permissionMode".to_string(), v);
            }
        }
        if let Some(mm) = mm {
            if let Ok(v) = serde_json::to_value(mm) {
                obj.insert("modelMode".to_string(), v);
            }
        }
        serde_json::Value::Object(obj).to_string()
    }

    /// Called when the thinking state changes.
    pub async fn on_thinking_change(&self, thinking: bool) {
        *self.thinking.lock().await = thinking;
        self.send_keep_alive().await;
    }

    /// Called when the mode changes between local and remote.
    pub async fn on_mode_change(&self, mode: SessionMode) {
        *self.mode.lock().await = mode;
        self.send_keep_alive().await;
        let pm_label = match *self.permission_mode.lock().await {
            Some(pm) => pm.label().to_string(),
            None => "unset".to_string(),
        };
        let mm_label = match *self.model_mode.lock().await {
            Some(mm) => mm.label().to_string(),
            None => "unset".to_string(),
        };
        debug!(
            "[{}] Mode switched to {} (permissionMode={}, modelMode={})",
            self.session_label,
            mode.as_str(),
            pm_label,
            mm_label,
        );
        (self.on_mode_change_cb)(mode);
    }

    /// Called when a session ID is discovered from the agent backend.
    pub async fn on_session_found(&self, session_id: &str) {
        *self.session_id.lock().await = Some(session_id.to_string());

        let sid = session_id.to_string();
        let label = self.session_id_label.clone();
        let session_label = self.session_label.clone();
        let apply_fn = self.apply_session_id_to_metadata.clone();

        let _ = self
            .ws_client
            .update_metadata(move |metadata| apply_fn(metadata, &sid))
            .await;

        debug!(
            "[{}] {} session ID {} added to metadata",
            session_label, label, session_id
        );

        let callbacks = self.session_found_callbacks.lock().await;
        for cb in callbacks.iter() {
            cb(session_id);
        }
    }

    /// Register a callback for when a session ID is found.
    pub async fn add_session_found_callback(&self, callback: SessionFoundCallback) {
        self.session_found_callbacks.lock().await.push(callback);
    }

    /// Stop the keep-alive interval.
    pub fn stop_keep_alive(&self) {
        let mut handle = self.keep_alive_handle.lock().unwrap();
        if let Some(h) = handle.take() {
            h.abort();
        }
    }

    pub async fn get_permission_mode(&self) -> Option<PermissionMode> {
        *self.permission_mode.lock().await
    }

    pub async fn get_model_mode(&self) -> Option<ModelMode> {
        *self.model_mode.lock().await
    }
}
