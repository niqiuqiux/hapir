use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use crate::store::Store;
use crate::sync::SyncEngine;
use super::super::connection_manager::ConnectionManager;
use super::super::terminal_registry::TerminalRegistry;

/// Process an incoming WebSocket event from a terminal (webapp) connection.
pub async fn handle_terminal_event(
    conn_id: &str,
    namespace: &str,
    event: &str,
    data: Value,
    sync_engine: &Arc<Mutex<SyncEngine>>,
    store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
    max_terminals_per_socket: usize,
    max_terminals_per_session: usize,
) -> Option<Value> {
    match event {
        "terminal:create" => {
            handle_terminal_create(
                conn_id, namespace, data, sync_engine, store, conn_mgr,
                terminal_registry, max_terminals_per_socket, max_terminals_per_session,
            ).await
        }
        "terminal:write" => {
            handle_terminal_write(conn_id, data, conn_mgr, terminal_registry).await;
            None
        }
        "terminal:resize" => {
            handle_terminal_resize(conn_id, data, conn_mgr, terminal_registry).await;
            None
        }
        "terminal:close" => {
            handle_terminal_close(conn_id, namespace, data, conn_mgr, terminal_registry).await;
            None
        }
        _ => {
            debug!(event, "unhandled terminal event");
            None
        }
    }
}

async fn handle_terminal_create(
    conn_id: &str,
    namespace: &str,
    data: Value,
    sync_engine: &Arc<Mutex<SyncEngine>>,
    _store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
    max_per_socket: usize,
    max_per_session: usize,
) -> Option<Value> {
    let session_id = data.get("sessionId").and_then(|v| v.as_str())?;
    let terminal_id = data.get("terminalId").and_then(|v| v.as_str())?;
    let cols = data.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u32;
    let rows = data.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u32;

    // Check session is active
    let session_ok = {
        let mut engine = sync_engine.lock().await;
        engine.get_session_by_namespace(session_id, namespace)
            .is_some_and(|s| s.active)
    };
    if !session_ok {
        let err = serde_json::json!({
            "event": "terminal:error",
            "data": {"terminalId": terminal_id, "message": "Session is inactive or unavailable."}
        });
        conn_mgr.send_to(conn_id, &err.to_string()).await;
        return None;
    }

    // Check limits
    {
        let reg = terminal_registry.read().await;
        if reg.count_for_socket(conn_id) >= max_per_socket {
            let err = serde_json::json!({
                "event": "terminal:error",
                "data": {"terminalId": terminal_id, "message": format!("Too many terminals open (max {max_per_socket}).")}
            });
            conn_mgr.send_to(conn_id, &err.to_string()).await;
            return None;
        }
        if reg.count_for_session(session_id) >= max_per_session {
            let err = serde_json::json!({
                "event": "terminal:error",
                "data": {"terminalId": terminal_id, "message": format!("Too many terminals for this session (max {max_per_session}).")}
            });
            conn_mgr.send_to(conn_id, &err.to_string()).await;
            return None;
        }
    }

    // Find a CLI socket in the session room
    let cli_socket_id = match conn_mgr.pick_cli_in_session(session_id, namespace).await {
        Some(id) => id,
        None => {
            let err = serde_json::json!({
                "event": "terminal:error",
                "data": {"terminalId": terminal_id, "message": "CLI is not connected for this session."}
            });
            conn_mgr.send_to(conn_id, &err.to_string()).await;
            return None;
        }
    };

    // Register terminal
    let entry = {
        let mut reg = terminal_registry.write().await;
        reg.register(terminal_id, session_id, conn_id, &cli_socket_id)
    };
    if entry.is_none() {
        let err = serde_json::json!({
            "event": "terminal:error",
            "data": {"terminalId": terminal_id, "message": "Terminal ID is already in use."}
        });
        conn_mgr.send_to(conn_id, &err.to_string()).await;
        return None;
    }

    // Send terminal:open to CLI
    let open_msg = serde_json::json!({
        "event": "terminal:open",
        "data": {"sessionId": session_id, "terminalId": terminal_id, "cols": cols, "rows": rows}
    });
    conn_mgr.send_to(&cli_socket_id, &open_msg.to_string()).await;

    None
}

async fn handle_terminal_write(
    conn_id: &str,
    data: Value,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
) {
    let terminal_id = match data.get("terminalId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };
    let payload = match data.get("data").and_then(|v| v.as_str()) {
        Some(d) => d.to_string(),
        None => return,
    };

    let cli_socket_id = {
        let reg = terminal_registry.read().await;
        let entry = match reg.get(&terminal_id) {
            Some(e) if e.socket_id == conn_id => e,
            _ => return,
        };
        entry.cli_socket_id.clone()
    };

    terminal_registry.write().await.mark_activity(&terminal_id);

    let session_id = {
        let reg = terminal_registry.read().await;
        reg.get(&terminal_id).map(|e| e.session_id.clone()).unwrap_or_default()
    };
    let msg = serde_json::json!({
        "event": "terminal:write",
        "data": {
            "sessionId": session_id,
            "terminalId": terminal_id,
            "data": payload,
        }
    });
    conn_mgr.send_to(&cli_socket_id, &msg.to_string()).await;
}

async fn handle_terminal_resize(
    conn_id: &str,
    data: Value,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
) {
    let terminal_id = match data.get("terminalId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };
    let cols = data.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u32;
    let rows = data.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u32;

    let (cli_socket_id, session_id) = {
        let reg = terminal_registry.read().await;
        let entry = match reg.get(&terminal_id) {
            Some(e) if e.socket_id == conn_id => e,
            _ => return,
        };
        (entry.cli_socket_id.clone(), entry.session_id.clone())
    };

    terminal_registry.write().await.mark_activity(&terminal_id);

    let msg = serde_json::json!({
        "event": "terminal:resize",
        "data": {"sessionId": session_id, "terminalId": terminal_id, "cols": cols, "rows": rows}
    });
    conn_mgr.send_to(&cli_socket_id, &msg.to_string()).await;
}

async fn handle_terminal_close(
    conn_id: &str,
    _namespace: &str,
    data: Value,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
) {
    let terminal_id = match data.get("terminalId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };

    let entry = {
        let reg = terminal_registry.read().await;
        match reg.get(&terminal_id) {
            Some(e) if e.socket_id == conn_id => e.clone(),
            _ => return,
        }
    };

    terminal_registry.write().await.remove(&terminal_id);

    // Notify CLI
    let msg = serde_json::json!({
        "event": "terminal:close",
        "data": {"sessionId": entry.session_id, "terminalId": terminal_id}
    });
    conn_mgr.send_to(&entry.cli_socket_id, &msg.to_string()).await;
}

/// Clean up all terminals when a terminal (webapp) socket disconnects.
pub async fn cleanup_terminal_disconnect(
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
) {
    let removed = terminal_registry.write().await.remove_by_socket(conn_id);
    for entry in removed {
        let msg = serde_json::json!({
            "event": "terminal:close",
            "data": {"sessionId": entry.session_id, "terminalId": entry.terminal_id}
        });
        conn_mgr.send_to(&entry.cli_socket_id, &msg.to_string()).await;
    }
}

/// Clean up all terminals when a CLI socket disconnects.
pub async fn cleanup_cli_disconnect(
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
) {
    let removed = terminal_registry.write().await.remove_by_cli_socket(conn_id);
    for entry in removed {
        let msg = serde_json::json!({
            "event": "terminal:error",
            "data": {"terminalId": entry.terminal_id, "message": "CLI disconnected."}
        });
        conn_mgr.send_to(&entry.socket_id, &msg.to_string()).await;
    }
}
