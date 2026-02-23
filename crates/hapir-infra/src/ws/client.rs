use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::outbox::SocketOutbox;
use super::protocol::{WsMessage, WsRequest};
use crate::utils::time::epoch_ms;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, Mutex, Notify, RwLock};
use tokio::time;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

const PING_INTERVAL: Duration = Duration::from_secs(25);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct WsClientConfig {
    pub url: String,
    pub auth_token: String,
    pub client_type: String,
    pub scope_id: String,
    pub max_reconnect_attempts: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

type EventHandler = Box<dyn Fn(Value) + Send + Sync>;
type RpcHandler = Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync>;

type ConnectionCallback = Box<dyn Fn() + Send + Sync>;

pub struct WsClient {
    config: WsClientConfig,
    state: Arc<RwLock<ConnectionState>>,
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>,
    pending_acks: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    event_handlers: Arc<RwLock<HashMap<String, EventHandler>>>,
    rpc_handlers: Arc<RwLock<HashMap<String, RpcHandler>>>,
    outbox: Arc<Mutex<SocketOutbox>>,
    connected_notify: Arc<Notify>,
    shutdown: Arc<Notify>,
    shutdown_flag: Arc<AtomicBool>,
    last_activity: Arc<AtomicU64>,
    on_connect: Arc<Mutex<Option<ConnectionCallback>>>,
    on_disconnect: Arc<Mutex<Option<ConnectionCallback>>>,
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
            last_activity: Arc::new(AtomicU64::new(0)),
            on_connect: Arc::new(Mutex::new(None)),
            on_disconnect: Arc::new(Mutex::new(None)),
        }
    }

    async fn set_state(state: &RwLock<ConnectionState>, new_state: ConnectionState) {
        *state.write().await = new_state;
    }

    fn build_ws_url(config: &WsClientConfig) -> String {
        format!(
            "{}/ws/cli?token={}&clientType={}&scopeId={}",
            config
                .url
                .replace("http://", "ws://")
                .replace("https://", "wss://"),
            urlencoding::encode(&config.auth_token),
            urlencoding::encode(&config.client_type),
            urlencoding::encode(&config.scope_id),
        )
    }

    async fn flush_outbox(
        outbox: &Mutex<SocketOutbox>,
        tx: &mpsc::UnboundedSender<Message>,
    ) {
        let mut ob = outbox.lock().await;
        for msg in ob.drain() {
            let _ = tx.send(Message::Text(msg.into()));
        }
    }

    async fn reregister_rpc(
        handlers: &RwLock<HashMap<String, RpcHandler>>,
        tx: &mpsc::UnboundedSender<Message>,
    ) {
        let handlers = handlers.read().await;
        for method in handlers.keys() {
            let req = WsRequest::fire("rpc-register", serde_json::json!({"method": method}));
            if let Ok(json) = serde_json::to_string(&req) {
                let _ = tx.send(Message::Text(json.into()));
            }
        }
        if !handlers.is_empty() {
            info!(count = handlers.len(), "re-registered RPC handlers on connect");
        }
    }

    async fn cleanup(
        state: &RwLock<ConnectionState>,
        tx: &Mutex<Option<mpsc::UnboundedSender<Message>>>,
        acks: &Mutex<HashMap<String, oneshot::Sender<Value>>>,
    ) {
        Self::set_state(state, ConnectionState::Disconnected).await;
        *tx.lock().await = None;
        acks.lock().await.clear();
    }

    async fn write_loop(
        mut rx: mpsc::UnboundedReceiver<Message>,
        mut sink: futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
            Message,
        >,
        shutdown: &AtomicBool,
    ) {
        while let Some(msg) = rx.recv().await {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    }

    async fn ping_loop(tx: &mpsc::UnboundedSender<Message>, shutdown: &AtomicBool) {
        let mut interval = time::interval(PING_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            if tx.send(Message::Ping(vec![].into())).is_err() {
                break;
            }
        }
    }

    async fn watchdog_loop(activity: &AtomicU64, shutdown: &AtomicBool) {
        let dead_timeout = PING_INTERVAL + PONG_TIMEOUT;
        let mut interval = time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            let last = activity.load(Ordering::Relaxed);
            let now = epoch_ms();
            if now.saturating_sub(last) > dead_timeout.as_millis() as u64 {
                warn!(
                    "no activity for {}s, connection presumed dead",
                    dead_timeout.as_secs()
                );
                break;
            }
        }
    }

    async fn read_loop(
        mut stream: futures::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        >,
        pending: &Mutex<HashMap<String, oneshot::Sender<Value>>>,
        events: &RwLock<HashMap<String, EventHandler>>,
        rpcs: &RwLock<HashMap<String, RpcHandler>>,
        tx: &mpsc::UnboundedSender<Message>,
        shutdown: &AtomicBool,
        activity: &AtomicU64,
    ) {
        while let Some(msg) = stream.next().await {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            activity.store(epoch_ms(), Ordering::Relaxed);

            match msg {
                Ok(Message::Text(text)) => {
                    let text_str: &str = &text;
                    let Ok(ws_msg) = serde_json::from_str::<WsMessage>(text_str) else {
                        continue;
                    };

                    if let Some(ref id) = ws_msg.id
                        && ws_msg.event.ends_with(":ack")
                        && let Some(sender) = pending.lock().await.remove(id)
                    {
                        let _ = sender.send(ws_msg.data);
                        continue;
                    }

                    if ws_msg.event == "rpc-request"
                        && let Some(ref id) = ws_msg.id
                    {
                        Self::dispatch_rpc(ws_msg.data, id, rpcs, tx).await;
                        continue;
                    }

                    if let Some(handler) = events.read().await.get(&ws_msg.event) {
                        handler(ws_msg.data);
                    }
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    warn!(error = %e, "WebSocket read error");
                    break;
                }
                _ => {}
            }
        }
    }

    async fn dispatch_rpc(
        data: Value,
        id: &str,
        rpcs: &RwLock<HashMap<String, RpcHandler>>,
        tx: &mpsc::UnboundedSender<Message>,
    ) {
        let method = data.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params_raw = data.get("params").cloned().unwrap_or(Value::Null);
        // Hub sends params as JSON-encoded string
        let params = match params_raw {
            Value::String(ref s) => serde_json::from_str(s).unwrap_or(params_raw),
            other => other,
        };

        if let Some(handler) = rpcs.read().await.get(method).cloned() {
            debug!(method, "RPC request received, dispatching to handler");
            let id = id.to_string();
            let tx = tx.clone();
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
    }

    async fn connect_internal(&self) {
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

                if let Some(max) = config.max_reconnect_attempts
                    && attempts >= max
                {
                    warn!(attempts, "max reconnection attempts reached, giving up");
                    break;
                }
                attempts += 1;

                Self::set_state(&state, ConnectionState::Connecting).await;

                let ws_url = Self::build_ws_url(&config);
                debug!(url = %ws_url, attempt = attempts, "connecting to WebSocket");

                let connect_result =
                    time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&ws_url)).await;

                let ws_stream = match connect_result {
                    Ok(Ok((stream, _))) => stream,
                    Ok(Err(e)) => {
                        warn!(attempt = attempts, error = %e, "WebSocket connection failed, will retry");
                        Self::wait_backoff(&shutdown_flag, &shutdown, &mut backoff, max_backoff)
                            .await;
                        continue;
                    }
                    Err(_) => {
                        warn!(
                            attempt = attempts,
                            "WebSocket connect timed out ({}s), will retry",
                            CONNECT_TIMEOUT.as_secs()
                        );
                        Self::wait_backoff(&shutdown_flag, &shutdown, &mut backoff, max_backoff)
                            .await;
                        continue;
                    }
                };

                info!(scope_id = %config.scope_id, "WebSocket connected");
                Self::set_state(&state, ConnectionState::Connected).await;
                backoff = Duration::from_secs(1);
                attempts = 0;

                last_activity.store(epoch_ms(), Ordering::Relaxed);

                let (write, read) = ws_stream.split();
                let (send_tx, send_rx) = mpsc::unbounded_channel::<Message>();

                // Hold tx lock while flushing outbox and re-registering,
                // prevents race with emit() enqueueing to already-flushed outbox.
                {
                    let mut tx_guard = tx_holder.lock().await;
                    *tx_guard = Some(send_tx.clone());
                    Self::flush_outbox(&outbox, &send_tx).await;
                    Self::reregister_rpc(&rpc_handlers, &send_tx).await;
                }

                if let Some(ref cb) = *on_connect.lock().await {
                    cb();
                }
                connected_notify.notify_waiters();

                tokio::select! {
                    _ = Self::write_loop(send_rx, write, &shutdown_flag) => {},
                    _ = Self::read_loop(read, &pending_acks, &event_handlers, &rpc_handlers, &send_tx, &shutdown_flag, &last_activity) => {},
                    _ = Self::ping_loop(&send_tx, &shutdown_flag) => {},
                    _ = Self::watchdog_loop(&last_activity, &shutdown_flag) => {},
                    _ = shutdown.notified() => {
                        Self::cleanup(&state, &tx_holder, &pending_acks).await;
                        return;
                    }
                }

                Self::cleanup(&state, &tx_holder, &pending_acks).await;
                info!(scope_id = %config.scope_id, "WebSocket disconnected, scheduling reconnect");

                if let Some(ref cb) = *on_disconnect.lock().await {
                    cb();
                }

                Self::wait_backoff(&shutdown_flag, &shutdown, &mut backoff, max_backoff).await;
            }
        });
    }

    pub async fn is_connected(&self) -> bool {
        *self.state.read().await == ConnectionState::Connected
    }

    async fn wait_connected(&self, timeout: Duration) -> bool {
        if self.is_connected().await {
            return true;
        }
        tokio::time::timeout(timeout, self.connected_notify.notified())
            .await
            .is_ok()
    }

    pub async fn connect(&self, timeout: Duration) -> anyhow::Result<()> {
        self.connect_internal().await;
        if self.wait_connected(timeout).await {
            Ok(())
        } else {
            anyhow::bail!("failed to connect within {}s", timeout.as_secs())
        }
    }

    pub async fn on(
        &self,
        event: impl Into<String>,
        handler: impl Fn(Value) + Send + Sync + 'static,
    ) {
        self.event_handlers
            .write()
            .await
            .insert(event.into(), Box::new(handler));
    }

    pub async fn register_rpc(
        &self,
        method: impl Into<String>,
        handler: impl Fn(Value) -> Pin<Box<dyn Future<Output = Value> + Send>> + Send + Sync + 'static,
    ) {
        self.rpc_handlers
            .write()
            .await
            .insert(method.into(), Arc::new(handler));
    }

    #[allow(dead_code)]
    pub async fn on_connect(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_connect.lock().await = Some(Box::new(f));
    }

    #[allow(dead_code)]
    pub async fn on_disconnect(&self, f: impl Fn() + Send + Sync + 'static) {
        *self.on_disconnect.lock().await = Some(Box::new(f));
    }

    pub async fn emit(&self, event: impl Into<String>, data: Value) {
        let req = WsRequest::fire(event, data);
        let json = match serde_json::to_string(&req) {
            Ok(j) => j,
            Err(_) => return,
        };

        // Hold tx lock to prevent race with outbox flush
        let tx_guard = self.tx.lock().await;
        if let Some(tx) = tx_guard.as_ref() {
            let _ = tx.send(Message::Text(json.into()));
        } else {
            self.outbox.lock().await.enqueue(&req.event, &json);
        }
    }

    pub async fn emit_with_ack(
        &self,
        event: impl Into<String>,
        data: Value,
    ) -> anyhow::Result<Value> {
        let (req, id) = WsRequest::with_ack(event, data);
        let json = serde_json::to_string(&req)?;

        let (sender, receiver) = oneshot::channel();
        self.pending_acks.lock().await.insert(id.clone(), sender);

        if let Some(tx) = self.tx.lock().await.as_ref() {
            tx.send(Message::Text(json.into()))
                .map_err(|_| anyhow::anyhow!("send failed"))?;
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

    async fn wait_backoff(
        shutdown_flag: &AtomicBool,
        shutdown: &Notify,
        backoff: &mut Duration,
        max_backoff: Duration,
    ) {
        if shutdown_flag.load(Ordering::Relaxed) {
            return;
        }
        debug!(
            backoff_ms = backoff.as_millis() as u64,
            "waiting before reconnect"
        );
        tokio::select! {
            _ = time::sleep(*backoff) => {},
            _ = shutdown.notified() => {},
        }
        *backoff = (*backoff * 2).min(max_backoff);
    }

    pub async fn close(&self) {
        self.shutdown_flag.store(true, Ordering::Relaxed);
        self.shutdown.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
