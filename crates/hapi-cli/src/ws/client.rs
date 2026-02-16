use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex, Notify, RwLock};
use tokio::time;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use super::outbox::SocketOutbox;
use super::protocol::{WsMessage, WsRequest};

// --- Heartbeat / reconnection constants ---
const PING_INTERVAL: Duration = Duration::from_secs(25);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration for the WebSocket client
#[derive(Debug, Clone)]
pub struct WsClientConfig {
    pub url: String,
    pub auth_token: String,
    pub client_type: String, // "session-scoped" or "machine-scoped"
    pub scope_id: String,    // session ID or machine ID
    /// Max reconnection attempts (None = unlimited). Resets after each successful connection.
    pub max_reconnect_attempts: Option<usize>,
}

/// Monotonic epoch millis for lock-free last-activity tracking.
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

/// Event handler callback type
type EventHandler = Box<dyn Fn(Value) + Send + Sync>;

/// RPC request handler type
type RpcHandler = Arc<
    dyn Fn(Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>>
        + Send
        + Sync,
>;

pub struct WsClient {
    config: WsClientConfig,
    state: Arc<RwLock<ConnectionState>>,

    /// Channel to send messages to the write task
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>,

    /// Pending ack callbacks: request_id -> oneshot sender
    pending_acks: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,

    /// Event handlers: event_name -> handler
    event_handlers: Arc<RwLock<HashMap<String, EventHandler>>>,

    /// RPC handlers: method_name -> handler
    rpc_handlers: Arc<RwLock<HashMap<String, RpcHandler>>>,

    /// Offline message buffer
    outbox: Arc<Mutex<SocketOutbox>>,

    /// Notify when connected (for reconnection logic)
    connected_notify: Arc<Notify>,

    /// Shutdown signal
    shutdown: Arc<Notify>,
    shutdown_flag: Arc<AtomicBool>,

    /// Has connected at least once
    has_connected_once: Arc<AtomicBool>,

    /// Last time we received any data (epoch ms, lock-free)
    last_activity: Arc<AtomicU64>,

    /// Connection callbacks
    on_connect: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    on_disconnect: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
}

impl WsClient {
    pub fn new(config: WsClientConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(ConnectionState::Disconnected)),
            tx: Arc::new(Mutex::new(None)),
            pending_acks: Arc::new(Mutex::new(HashMap::new())),
            event_handlers: Arc::new(RwLock::new(HashMap::new())),
            rpc_handlers: Arc::new(RwLock::new(HashMap::new())),
            outbox: Arc::new(Mutex::new(SocketOutbox::new())),
            connected_notify: Arc::new(Notify::new()),
            shutdown: Arc::new(Notify::new()),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            has_connected_once: Arc::new(AtomicBool::new(false)),
            last_activity: Arc::new(AtomicU64::new(0)),
            on_connect: Arc::new(Mutex::new(None)),
            on_disconnect: Arc::new(Mutex::new(None)),
        }
    }

    /// Register an event handler.
    pub async fn on(&self, event: impl Into<String>, handler: impl Fn(Value) + Send + Sync + 'static) {
        self.event_handlers.write().await.insert(event.into(), Box::new(handler));
    }

    /// Register an RPC handler.
    pub async fn register_rpc(
        &self,
        method: impl Into<String>,
        handler: impl Fn(Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send>> + Send + Sync + 'static,
    ) {
        self.rpc_handlers.write().await.insert(method.into(), Arc::new(handler));
    }

    /// Set connection callback.
    #[allow(dead_code)]
    pub async fn on_connect(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_connect.lock().await = Some(Box::new(f));
    }

    /// Set disconnection callback.
    #[allow(dead_code)]
    pub async fn on_disconnect(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_disconnect.lock().await = Some(Box::new(f));
    }

    /// Send a fire-and-forget event.
    pub async fn emit(&self, event: impl Into<String>, data: Value) {
        let req = WsRequest::fire(event, data);
        let json = match serde_json::to_string(&req) {
            Ok(j) => j,
            Err(_) => return,
        };

        if let Some(tx) = self.tx.lock().await.as_ref() {
            let _ = tx.send(Message::Text(json.into()));
        } else {
            self.outbox.lock().await.enqueue(&req.event, &json);
        }
    }

    /// Send an event and wait for ack response.
    pub async fn emit_with_ack(&self, event: impl Into<String>, data: Value) -> anyhow::Result<Value> {
        let (req, id) = WsRequest::with_ack(event, data);
        let json = serde_json::to_string(&req)?;

        let (sender, receiver) = oneshot::channel();
        self.pending_acks.lock().await.insert(id.clone(), sender);

        if let Some(tx) = self.tx.lock().await.as_ref() {
            tx.send(Message::Text(json.into())).map_err(|_| anyhow::anyhow!("send failed"))?;
        } else {
            self.pending_acks.lock().await.remove(&id);
            anyhow::bail!("not connected");
        }

        match time::timeout(Duration::from_secs(30), receiver).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => anyhow::bail!("ack sender dropped"),
            Err(_) => {
                self.pending_acks.lock().await.remove(&id);
                anyhow::bail!("ack timeout")
            }
        }
    }

    /// Start the WebSocket client with auto-reconnection, heartbeat, and connect timeout.
    pub async fn connect(&self) {
        let config = self.config.clone();
        let state = self.state.clone();
        let tx_holder = self.tx.clone();
        let pending_acks = self.pending_acks.clone();
        let event_handlers = self.event_handlers.clone();
        let rpc_handlers = self.rpc_handlers.clone();
        let outbox = self.outbox.clone();
        let connected_notify = self.connected_notify.clone();
        let shutdown = self.shutdown.clone();
        let shutdown_flag = self.shutdown_flag.clone();
        let has_connected_once = self.has_connected_once.clone();
        let last_activity = self.last_activity.clone();
        let on_connect = self.on_connect.clone();
        let on_disconnect = self.on_disconnect.clone();

        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            let max_backoff = Duration::from_secs(5);
            let mut attempts: usize = 0;

            loop {
                if shutdown_flag.load(Ordering::Relaxed) {
                    break;
                }

                // Check reconnect limit
                if let Some(max) = config.max_reconnect_attempts {
                    if attempts >= max {
                        warn!(attempts, "max reconnection attempts reached, giving up");
                        break;
                    }
                }
                attempts += 1;

                *state.write().await = ConnectionState::Connecting;

                let ws_url = format!(
                    "{}/ws/cli?token={}&clientType={}&scopeId={}",
                    config.url.replace("http://", "ws://").replace("https://", "wss://"),
                    urlencoding::encode(&config.auth_token),
                    urlencoding::encode(&config.client_type),
                    urlencoding::encode(&config.scope_id),
                );

                debug!(url = %ws_url, attempt = attempts, "connecting to WebSocket");

                // Connect with timeout
                let connect_result =
                    time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&ws_url)).await;

                let ws_stream = match connect_result {
                    Ok(Ok((stream, _))) => stream,
                    Ok(Err(e)) => {
                        warn!(error = %e, "WebSocket connection failed");
                        Self::wait_backoff(&shutdown_flag, &shutdown, &mut backoff, max_backoff).await;
                        continue;
                    }
                    Err(_) => {
                        warn!("WebSocket connect timed out ({}s)", CONNECT_TIMEOUT.as_secs());
                        Self::wait_backoff(&shutdown_flag, &shutdown, &mut backoff, max_backoff).await;
                        continue;
                    }
                };

                info!("WebSocket connected");
                *state.write().await = ConnectionState::Connected;
                has_connected_once.store(true, Ordering::Relaxed);
                backoff = Duration::from_secs(1);
                attempts = 0; // reset on success

                last_activity.store(epoch_ms(), Ordering::Relaxed);

                let (mut write, mut read) = ws_stream.split();
                let (send_tx, mut send_rx) = mpsc::unbounded_channel::<Message>();
                *tx_holder.lock().await = Some(send_tx.clone());

                // Flush outbox
                {
                    let mut ob = outbox.lock().await;
                    for msg in ob.drain() {
                        let _ = send_tx.send(Message::Text(msg.into()));
                    }
                }

                if let Some(ref cb) = *on_connect.lock().await {
                    cb();
                }
                connected_notify.notify_waiters();

                // --- Write task ---
                let write_shutdown = shutdown_flag.clone();
                let write_task = async {
                    while let Some(msg) = send_rx.recv().await {
                        if write_shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        if write.send(msg).await.is_err() {
                            break;
                        }
                    }
                };

                // --- Ping task (heartbeat) ---
                let ping_tx = send_tx.clone();
                let ping_shutdown = shutdown_flag.clone();
                let ping_task = async {
                    let mut interval = time::interval(PING_INTERVAL);
                    interval.tick().await; // skip first immediate tick
                    loop {
                        interval.tick().await;
                        if ping_shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        if ping_tx.send(Message::Ping(vec![].into())).is_err() {
                            break;
                        }
                    }
                };

                // --- Watchdog task (detect dead connection) ---
                let wd_activity = last_activity.clone();
                let wd_shutdown = shutdown_flag.clone();
                let dead_timeout = PING_INTERVAL + PONG_TIMEOUT; // 35s
                let watchdog_task = async {
                    let mut interval = time::interval(Duration::from_secs(5));
                    loop {
                        interval.tick().await;
                        if wd_shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        let last = wd_activity.load(Ordering::Relaxed);
                        let now = epoch_ms();
                        if now.saturating_sub(last) > dead_timeout.as_millis() as u64 {
                            warn!("no activity for {}s, connection presumed dead", dead_timeout.as_secs());
                            break;
                        }
                    }
                };

                // --- Read task ---
                let read_pending = pending_acks.clone();
                let read_handlers = event_handlers.clone();
                let read_rpcs = rpc_handlers.clone();
                let read_tx = send_tx.clone();
                let read_shutdown = shutdown_flag.clone();
                let read_activity = last_activity.clone();
                let read_task = async {
                    while let Some(msg) = read.next().await {
                        if read_shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        // Any received frame counts as activity
                        read_activity.store(epoch_ms(), Ordering::Relaxed);

                        match msg {
                            Ok(Message::Text(text)) => {
                                let text_str: &str = &text;
                                if let Ok(ws_msg) = serde_json::from_str::<WsMessage>(text_str) {
                                    // Ack response
                                    if let Some(ref id) = ws_msg.id {
                                        if ws_msg.event.ends_with(":ack") {
                                            if let Some(sender) = read_pending.lock().await.remove(id) {
                                                let _ = sender.send(ws_msg.data);
                                                continue;
                                            }
                                        }
                                    }

                                    // RPC request
                                    if ws_msg.event == "rpc-request" {
                                        if let Some(ref id) = ws_msg.id {
                                            let method = ws_msg.data.get("method")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let params = ws_msg.data.get("params")
                                                .cloned()
                                                .unwrap_or(Value::Null);

                                            if let Some(handler) = read_rpcs.read().await.get(method).cloned() {
                                                let id = id.clone();
                                                let tx = read_tx.clone();
                                                tokio::spawn(async move {
                                                    let result = handler(params).await;
                                                    let ack = WsRequest {
                                                        id: Some(id),
                                                        event: "rpc-request:ack".into(),
                                                        data: result,
                                                    };
                                                    if let Ok(json) = serde_json::to_string(&ack) {
                                                        let _ = tx.send(Message::Text(json.into()));
                                                    }
                                                });
                                            }
                                            continue;
                                        }
                                    }

                                    // Event handler
                                    if let Some(handler) = read_handlers.read().await.get(&ws_msg.event) {
                                        handler(ws_msg.data);
                                    }
                                }
                            }
                            Ok(Message::Pong(_)) => {
                                // Activity already recorded above
                            }
                            Ok(Message::Close(_)) => break,
                            Err(e) => {
                                warn!(error = %e, "WebSocket read error");
                                break;
                            }
                            _ => {}
                        }
                    }
                };

                tokio::select! {
                    _ = write_task => {},
                    _ = read_task => {},
                    _ = ping_task => {},
                    _ = watchdog_task => {},
                    _ = shutdown.notified() => {
                        *state.write().await = ConnectionState::Disconnected;
                        *tx_holder.lock().await = None;
                        return;
                    }
                }

                *state.write().await = ConnectionState::Disconnected;
                *tx_holder.lock().await = None;
                pending_acks.lock().await.clear();

                if let Some(ref cb) = *on_disconnect.lock().await {
                    cb();
                }

                Self::wait_backoff(&shutdown_flag, &shutdown, &mut backoff, max_backoff).await;
            }
        });
    }

    /// Wait for backoff duration, respecting shutdown.
    async fn wait_backoff(
        shutdown_flag: &AtomicBool,
        shutdown: &Notify,
        backoff: &mut Duration,
        max_backoff: Duration,
    ) {
        if shutdown_flag.load(Ordering::Relaxed) {
            return;
        }
        debug!(backoff_ms = backoff.as_millis(), "reconnecting after delay");
        tokio::select! {
            _ = time::sleep(*backoff) => {},
            _ = shutdown.notified() => {},
        }
        *backoff = (*backoff * 2).min(max_backoff);
    }

    /// Disconnect and stop reconnection.
    pub async fn close(&self) {
        self.shutdown_flag.store(true, Ordering::Relaxed);
        self.shutdown.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// Check if currently connected.
    #[allow(dead_code)]
    pub async fn is_connected(&self) -> bool {
        *self.state.read().await == ConnectionState::Connected
    }

    /// Wait until connected (or timeout).
    #[allow(dead_code)]
    pub async fn wait_connected(&self, timeout: Duration) -> bool {
        if self.is_connected().await {
            return true;
        }
        tokio::time::timeout(timeout, self.connected_notify.notified())
            .await
            .is_ok()
    }

    /// Connect and wait for the first successful connection.
    /// Returns Ok(()) if connected within timeout, Err if not.
    #[allow(dead_code)]
    pub async fn connect_and_wait(&self, timeout: Duration) -> anyhow::Result<()> {
        self.connect().await;
        if self.wait_connected(timeout).await {
            Ok(())
        } else {
            anyhow::bail!("failed to connect within {}s", timeout.as_secs())
        }
    }
}
