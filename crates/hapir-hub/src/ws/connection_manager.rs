use std::collections::HashMap;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::sync::rpc_gateway::RpcTransport;
use super::rpc_registry::RpcRegistry;

/// Distinguishes why an RPC call could not be dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcCallError {
    /// No handler has been registered for this method (yet).
    NotRegistered,
    /// Handler exists but the underlying connection is gone.
    SendFailed,
}

/// A message to be sent to a WebSocket connection.
#[derive(Debug, Clone)]
pub enum WsOutMessage {
    Text(String),
    Close,
}

/// Per-connection state.
pub struct WsConnection {
    pub id: String,
    pub namespace: String,
    pub session_id: Option<String>,
    pub machine_id: Option<String>,
    pub conn_type: WsConnType,
    pub tx: mpsc::UnboundedSender<WsOutMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsConnType {
    Cli,
    Terminal,
}

/// Tracks pending RPC calls waiting for ack responses.
struct PendingRpc {
    tx: oneshot::Sender<Result<Value, String>>,
}

/// Central connection manager for all WebSocket connections.
/// Thread-safe via RwLock for use across axum handlers.
pub struct ConnectionManager {
    connections: RwLock<HashMap<String, WsConnection>>,
    rpc_registry: RwLock<RpcRegistry>,
    pending_rpcs: RwLock<HashMap<String, PendingRpc>>,
    /// CLI connections joined to session rooms: session_id → set of conn_ids
    session_rooms: RwLock<HashMap<String, Vec<String>>>,
    /// CLI connections joined to machine rooms: machine_id → set of conn_ids
    machine_rooms: RwLock<HashMap<String, Vec<String>>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            rpc_registry: RwLock::new(RpcRegistry::new()),
            pending_rpcs: RwLock::new(HashMap::new()),
            session_rooms: RwLock::new(HashMap::new()),
            machine_rooms: RwLock::new(HashMap::new()),
        }
    }

    pub async fn add_connection(&self, conn: WsConnection) {
        let id = conn.id.clone();
        self.connections.write().await.insert(id, conn);
    }

    pub async fn remove_connection(&self, conn_id: &str) {
        // Unregister RPC methods
        self.rpc_registry.write().await.unregister_all(conn_id);

        // Remove from rooms
        {
            let mut rooms = self.session_rooms.write().await;
            for members in rooms.values_mut() {
                members.retain(|id| id != conn_id);
            }
            rooms.retain(|_, v| !v.is_empty());
        }
        {
            let mut rooms = self.machine_rooms.write().await;
            for members in rooms.values_mut() {
                members.retain(|id| id != conn_id);
            }
            rooms.retain(|_, v| !v.is_empty());
        }

        self.connections.write().await.remove(conn_id);
    }

    pub async fn get_connection_tx(&self, conn_id: &str) -> Option<mpsc::UnboundedSender<WsOutMessage>> {
        self.connections.read().await
            .get(conn_id)
            .map(|c| c.tx.clone())
    }

    pub async fn get_connection_namespace(&self, conn_id: &str) -> Option<String> {
        self.connections.read().await
            .get(conn_id)
            .map(|c| c.namespace.clone())
    }

    /// Read-only access to the connections map.
    pub async fn connections_read(&self) -> tokio::sync::RwLockReadGuard<'_, HashMap<String, WsConnection>> {
        self.connections.read().await
    }

    /// Send a text message to a specific connection.
    pub async fn send_to(&self, conn_id: &str, msg: &str) -> bool {
        if let Some(tx) = self.get_connection_tx(conn_id).await {
            tx.send(WsOutMessage::Text(msg.to_string())).is_ok()
        } else {
            false
        }
    }

    /// Send Close to all connections for graceful shutdown.
    pub async fn close_all(&self) {
        let conns = self.connections.read().await;
        for conn in conns.values() {
            let _ = conn.tx.send(WsOutMessage::Close);
        }
    }

    /// Broadcast to all connections in a session room, optionally excluding a sender.
    pub async fn broadcast_to_session(&self, session_id: &str, msg: &str, exclude: Option<&str>) {
        let rooms = self.session_rooms.read().await;
        if let Some(members) = rooms.get(session_id) {
            let conns = self.connections.read().await;
            for member_id in members {
                if exclude == Some(member_id.as_str()) {
                    continue;
                }
                if let Some(conn) = conns.get(member_id) {
                    let _ = conn.tx.send(WsOutMessage::Text(msg.to_string()));
                }
            }
        }
    }

    /// Broadcast to all connections in a machine room, optionally excluding a sender.
    pub async fn broadcast_to_machine(&self, machine_id: &str, msg: &str, exclude: Option<&str>) {
        let rooms = self.machine_rooms.read().await;
        if let Some(members) = rooms.get(machine_id) {
            let conns = self.connections.read().await;
            for member_id in members {
                if exclude == Some(member_id.as_str()) {
                    continue;
                }
                if let Some(conn) = conns.get(member_id) {
                    let _ = conn.tx.send(WsOutMessage::Text(msg.to_string()));
                }
            }
        }
    }

    /// Join a connection to a session room.
    pub async fn join_session(&self, conn_id: &str, session_id: &str) {
        self.session_rooms.write().await
            .entry(session_id.to_string())
            .or_default()
            .push(conn_id.to_string());
    }

    /// Join a connection to a machine room.
    pub async fn join_machine(&self, conn_id: &str, machine_id: &str) {
        self.machine_rooms.write().await
            .entry(machine_id.to_string())
            .or_default()
            .push(conn_id.to_string());
    }

    /// Find a CLI connection in a session room matching a namespace.
    pub async fn pick_cli_in_session(&self, session_id: &str, namespace: &str) -> Option<String> {
        let rooms = self.session_rooms.read().await;
        let members = rooms.get(session_id)?;
        let conns = self.connections.read().await;
        for member_id in members {
            if let Some(conn) = conns.get(member_id)
                && conn.conn_type == WsConnType::Cli && conn.namespace == namespace
            {
                return Some(member_id.clone());
            }
        }
        None
    }

    // --- RPC ---

    pub async fn rpc_register(&self, conn_id: &str, method: &str) {
        tracing::info!(conn_id, method, "RPC method registered");
        self.rpc_registry.write().await.register(conn_id, method);
    }

    pub async fn rpc_unregister(&self, conn_id: &str, method: &str) {
        self.rpc_registry.write().await.unregister(conn_id, method);
    }

    /// Check whether a handler is registered for the given method.
    pub async fn has_rpc_handler(&self, method: &str) -> bool {
        self.rpc_registry.read().await.get_conn_id_for_method(method).is_some()
    }

    /// Initiate an RPC call: find the connection for the method, send request, return receiver.
    /// Returns `Err(RpcCallError::NotRegistered)` when no handler is registered,
    /// or `Err(RpcCallError::SendFailed)` when the handler exists but the send fails.
    pub async fn rpc_call_internal(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, RpcCallError> {
        let conn_id = {
            let reg = self.rpc_registry.read().await;
            match reg.get_conn_id_for_method(method) {
                Some(id) => {
                    tracing::debug!(method, conn_id = id, "RPC method resolved to connection");
                    id.to_string()
                }
                None => {
                    return Err(RpcCallError::NotRegistered);
                }
            }
        };

        let tx = self.get_connection_tx(&conn_id).await
            .ok_or(RpcCallError::SendFailed)?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let (resp_tx, resp_rx) = oneshot::channel();

        self.pending_rpcs.write().await.insert(
            request_id.clone(),
            PendingRpc { tx: resp_tx },
        );

        let msg = serde_json::json!({
            "id": request_id,
            "event": "rpc-request",
            "data": {
                "method": method,
                "params": serde_json::to_string(&params).unwrap_or_default()
            }
        });

        if tx.send(WsOutMessage::Text(msg.to_string())).is_err() {
            self.pending_rpcs.write().await.remove(&request_id);
            return Err(RpcCallError::SendFailed);
        }

        Ok(resp_rx)
    }

    /// Handle an RPC response (ack) from a connection.
    pub async fn handle_rpc_response(&self, request_id: &str, result: Result<Value, String>) {
        if let Some(pending) = self.pending_rpcs.write().await.remove(request_id) {
            let _ = pending.tx.send(result);
        }
    }
}

/// Implement RpcTransport so SyncEngine's RpcGateway can call through ConnectionManager.
impl RpcTransport for ConnectionManager {
    fn rpc_call(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, RpcCallError> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| RpcCallError::SendFailed)?;
        let method = method.to_string();

        // Use block_in_place since we're already in a tokio context
        tokio::task::block_in_place(|| {
            handle.block_on(self.rpc_call_internal(&method, params))
        })
    }

    fn has_rpc_handler(&self, method: &str) -> bool {
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => return false,
        };
        let method = method.to_string();
        tokio::task::block_in_place(|| {
            handle.block_on(self.has_rpc_handler(&method))
        })
    }
}

// Safety: ConnectionManager uses RwLock internally
unsafe impl Send for ConnectionManager {}
unsafe impl Sync for ConnectionManager {}
