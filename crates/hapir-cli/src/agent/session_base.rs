use std::sync::Arc;

use arc_swap::ArcSwapOption;
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use tracing::debug;

use hapir_shared::modes::{ModelMode, PermissionMode, SessionMode};

use hapir_infra::utils::message_queue::MessageQueue2;
use hapir_infra::ws::session_client::WsSessionClient;
use hapir_shared::schemas::HapirSessionMetadata;

/// Callback invoked when the mode changes.
pub type ModeChangeCallback = Box<dyn Fn(SessionMode) + Send + Sync>;

/// Applies a session ID to metadata (flavor-specific).
pub type ApplySessionIdFn =
    Box<dyn Fn(HapirSessionMetadata, &str) -> HapirSessionMetadata + Send + Sync>;

/// Options for constructing an AgentSessionBase.
pub struct AgentSessionBaseOptions<Mode: Clone + Send + 'static> {
    pub ws_client: Arc<WsSessionClient>,
    pub path: String,
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
    pub session_id: Arc<Mutex<Option<String>>>,
    pub mode: Arc<Mutex<SessionMode>>,
    pub thinking: Arc<Mutex<bool>>,
    pub thinking_status: Arc<Mutex<Option<String>>>,
    pub ws_client: Arc<WsSessionClient>,
    pub queue: Arc<MessageQueue2<Mode>>,

    on_mode_change_cb: Arc<ModeChangeCallback>,
    apply_session_id_to_metadata: Arc<ApplySessionIdFn>,
    session_label: String,
    session_id_label: String,
    keep_alive_handle: ArcSwapOption<JoinHandle<()>>,
    permission_mode: Arc<Mutex<Option<PermissionMode>>>,
    model_mode: Arc<Mutex<Option<ModelMode>>>,
    /// Signalled to request a mode switch (local↔remote).
    pub switch_notify: Arc<Notify>,
}

impl<Mode: Clone + Send + 'static> AgentSessionBase<Mode> {
    /// Create a new session base and start the keep-alive interval.
    pub fn new(opts: AgentSessionBaseOptions<Mode>) -> Arc<Self> {
        let session = Arc::new(Self {
            path: opts.path,
            session_id: Arc::new(Mutex::new(opts.session_id)),
            mode: Arc::new(Mutex::new(opts.mode)),
            thinking: Arc::new(Mutex::new(false)),
            thinking_status: Arc::new(Mutex::new(None)),
            ws_client: opts.ws_client,
            queue: opts.queue,
            on_mode_change_cb: Arc::new(opts.on_mode_change_cb),
            apply_session_id_to_metadata: Arc::new(opts.apply_session_id_to_metadata),
            session_label: opts.session_label,
            session_id_label: opts.session_id_label,
            keep_alive_handle: ArcSwapOption::empty(),
            permission_mode: Arc::new(Mutex::new(opts.permission_mode)),
            model_mode: Arc::new(Mutex::new(opts.model_mode)),
            switch_notify: Arc::new(Notify::new()),
        });

        // Start keep-alive interval (first session-alive is sent by the
        // connect task after RPC re-registration to guarantee ordering)
        let s = session.clone();
        let handle = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(2));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                s.send_keep_alive().await;
            }
        });

        // Store the handle so we can cancel later
        session.keep_alive_handle.store(Some(Arc::new(handle)));

        session
    }

    async fn send_keep_alive(&self) {
        let thinking = *self.thinking.lock().await;
        let thinking_status = self.thinking_status.lock().await.clone();
        let mode = *self.mode.lock().await;
        let pm = *self.permission_mode.lock().await;
        let mm = *self.model_mode.lock().await;
        self.ws_client
            .keep_alive(thinking, thinking_status.as_deref(), mode, pm, mm)
            .await;
    }

    /// Called when the thinking state changes.
    pub async fn on_thinking_change(&self, thinking: bool) {
        debug!("[{}] thinking changed to {}", self.session_label, thinking);
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
    }

    /// Stop the keep-alive interval.
    pub fn stop_keep_alive(&self) {
        if let Some(h) = self.keep_alive_handle.swap(None) {
            h.abort();
        }
    }

    pub async fn get_permission_mode(&self) -> Option<PermissionMode> {
        *self.permission_mode.lock().await
    }

    pub async fn get_model_mode(&self) -> Option<ModelMode> {
        *self.model_mode.lock().await
    }

    /// Set the thinking status text shown in the web UI status bar.
    pub async fn set_thinking_status(&self, status: Option<String>) {
        *self.thinking_status.lock().await = status;
    }
}
