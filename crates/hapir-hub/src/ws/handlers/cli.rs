use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use tracing::debug;

use crate::store::Store;
use crate::sync::SyncEngine;
use crate::sync::todos::extract_todos_from_message_content;
use super::super::connection_manager::ConnectionManager;
use super::super::terminal_registry::TerminalRegistry;

enum AccessError {
    NamespaceMissing,
    AccessDenied,
    NotFound,
}

fn resolve_session_access(store: &Store, sid: &str, namespace: &str) -> Result<(), AccessError> {
    use crate::store::sessions;
    if namespace.is_empty() {
        return Err(AccessError::NamespaceMissing);
    }
    if sessions::get_session_by_namespace(&store.conn(), sid, namespace).is_some() {
        return Ok(());
    }
    if sessions::get_session(&store.conn(), sid).is_some() {
        return Err(AccessError::AccessDenied);
    }
    Err(AccessError::NotFound)
}

fn resolve_machine_access(store: &Store, mid: &str, namespace: &str) -> Result<(), AccessError> {
    use crate::store::machines;
    if namespace.is_empty() {
        return Err(AccessError::NamespaceMissing);
    }
    if machines::get_machine_by_namespace(&store.conn(), mid, namespace).is_some() {
        return Ok(());
    }
    if machines::get_machine(&store.conn(), mid).is_some() {
        return Err(AccessError::AccessDenied);
    }
    Err(AccessError::NotFound)
}

async fn emit_access_error(
    conn_mgr: &ConnectionManager,
    conn_id: &str,
    scope: &str,
    id: &str,
    err: AccessError,
) {
    let (message, code) = match err {
        AccessError::AccessDenied => (format!("{scope} access denied"), "access-denied"),
        AccessError::NotFound => (format!("{scope} not found"), "not-found"),
        AccessError::NamespaceMissing => ("Namespace missing".to_string(), "namespace-missing"),
    };
    let msg = serde_json::json!({
        "event": "error",
        "data": { "message": message, "code": code, "scope": scope, "id": id }
    });
    conn_mgr.send_to(conn_id, &msg.to_string()).await;
}

/// Process an incoming WebSocket event from a CLI connection.
pub async fn handle_cli_event(
    conn_id: &str,
    namespace: &str,
    event: &str,
    data: Value,
    _request_id: Option<&str>,
    sync_engine: &Arc<SyncEngine>,
    store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
) -> Option<Value> {
    match event {
        "message" => handle_message(conn_id, namespace, data, sync_engine, store, conn_mgr).await,
        "message-delta" => {
            handle_message_delta(conn_id, namespace, data, sync_engine, store, conn_mgr).await;
            None
        }
        "update-metadata" => handle_update_metadata(namespace, data, store, sync_engine, conn_id, conn_mgr).await,
        "update-state" => handle_update_state(namespace, data, store, sync_engine, conn_id, conn_mgr).await,
        "session-alive" => {
            handle_session_alive(conn_id, namespace, data, store, sync_engine, conn_mgr).await;
            None
        }
        "session-end" => {
            handle_session_end(conn_id, namespace, data, store, sync_engine, conn_mgr).await;
            None
        }
        "machine-alive" => {
            handle_machine_alive(conn_id, namespace, data, store, sync_engine, conn_mgr).await;
            None
        }
        "machine-update-metadata" => handle_machine_update_metadata(namespace, data, store, sync_engine, conn_id, conn_mgr).await,
        "machine-update-state" => handle_machine_update_state(namespace, data, store, sync_engine, conn_id, conn_mgr).await,
        "rpc-register" => {
            if let Some(method) = data.get("method").and_then(|v| v.as_str()) {
                conn_mgr.rpc_register(conn_id, method).await;
            }
            None
        }
        "rpc-unregister" => {
            if let Some(method) = data.get("method").and_then(|v| v.as_str()) {
                conn_mgr.rpc_unregister(conn_id, method).await;
            }
            None
        }
        // "rpc-response" and "rpc-request:ack" are now handled at the transport
        // layer in ws/mod.rs before reaching this handler.
        // "rpc-response" and "rpc-request:ack" are now handled at the transport
        // layer in ws/mod.rs before reaching this handler.
        "rpc-response" | "rpc-request:ack" => None,
        "terminal:ready" | "terminal:output" => {
            forward_terminal_event(conn_id, event, &data, conn_mgr, terminal_registry, true).await;
            None
        }
        "terminal:error" => {
            forward_terminal_event(conn_id, event, &data, conn_mgr, terminal_registry, false).await;
            None
        }
        "terminal:exit" => {
            handle_terminal_exit_from_cli(conn_id, &data, conn_mgr, terminal_registry).await;
            None
        }
        "ping" => Some(Value::Null),
        _ => {
            debug!(event, "unhandled CLI event");
            None
        }
    }
}

async fn handle_message(
    conn_id: &str,
    namespace: &str,
    data: Value,
    sync_engine: &Arc<SyncEngine>,
    store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let sid = data.get("sid").and_then(|v| v.as_str())?;
    let local_id = data.get("localId").and_then(|v| v.as_str());

    // Validate session access
    if let Err(err) = resolve_session_access(store, sid, namespace) {
        emit_access_error(conn_mgr, conn_id, "session", sid, err).await;
        return None;
    }

    // Parse message content
    let raw = data.get("message")?;
    let content = if let Some(s) = raw.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
    } else {
        raw.clone()
    };

    // Add message to store
    let msg = {
        use crate::store::messages;
        match messages::add_message(&store.conn(), sid, &content, local_id) {
            Ok(m) => m,
            Err(_) => return None,
        }
    };

    // Extract todos
    let todos = extract_todos_from_message_content(&content);
    if let Some(ref todos) = todos
        && let Ok(todos_val) = serde_json::to_value(todos)
    {
        use crate::store::sessions;
        if sessions::set_session_todos(&store.conn(), sid, Some(&todos_val), msg.created_at, namespace) {
            sync_engine.handle_realtime_event(hapir_shared::schemas::SyncEvent::SessionUpdated {
                session_id: sid.to_string(),
                namespace: Some(namespace.to_string()),
                data: Some(serde_json::json!({"sid": sid})),
            }).await;
        }
    }

    // Build update and broadcast to session room (excluding sender)
    let update = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "seq": msg.seq,
        "createdAt": msg.created_at,
        "body": {
            "t": "new-message",
            "sid": sid,
            "message": {
                "id": msg.id,
                "seq": msg.seq,
                "createdAt": msg.created_at,
                "localId": msg.local_id,
                "content": msg.content,
            }
        }
    });
    let update_str = serde_json::to_string(&serde_json::json!({
        "event": "update",
        "data": update,
    })).unwrap_or_default();
    conn_mgr.broadcast_to_session(sid, &update_str, Some(conn_id)).await;

    // Emit to sync engine
    sync_engine.handle_realtime_event(hapir_shared::schemas::SyncEvent::MessageReceived {
        session_id: sid.to_string(),
        namespace: Some(namespace.to_string()),
        message: hapir_shared::schemas::DecryptedMessage {
            id: msg.id,
            seq: Some(msg.seq as f64),
            local_id: msg.local_id,
            content: msg.content.unwrap_or(Value::Null),
            created_at: msg.created_at as f64,
        },
    }).await;

    None
}

async fn handle_message_delta(
    conn_id: &str,
    namespace: &str,
    data: Value,
    sync_engine: &Arc<SyncEngine>,
    store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
) {
    let sid = match data.get("sid").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return,
    };

    if let Err(err) = resolve_session_access(store, sid, namespace) {
        emit_access_error(conn_mgr, conn_id, "session", sid, err).await;
        return;
    }

    let delta = match data.get("delta") {
        Some(d) => d.clone(),
        None => return,
    };

    // Broadcast delta to WebSocket clients (excluding sender)
    let update = serde_json::json!({
        "event": "message-delta",
        "data": {
            "sessionId": sid,
            "delta": delta,
        }
    });
    conn_mgr.broadcast_to_session(sid, &update.to_string(), Some(conn_id)).await;

    // Emit delta via SSE
    if let Ok(delta_data) = serde_json::from_value::<hapir_shared::schemas::MessageDeltaData>(delta) {
        sync_engine.handle_realtime_event(
            hapir_shared::schemas::SyncEvent::MessageDelta {
                session_id: sid.to_string(),
                namespace: Some(namespace.to_string()),
                delta: delta_data,
            }
        ).await;
    }
}

async fn handle_update_metadata(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let sid = data.get("sid").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let metadata = data.get("metadata")?;

    use crate::store::{sessions, types::VersionedUpdateResult};
    if let Err(err) = resolve_session_access(store, sid, namespace) {
        return Some(serde_json::json!({"result": "error", "reason": match err {
            AccessError::AccessDenied => "access-denied",
            AccessError::NotFound => "not-found",
            AccessError::NamespaceMissing => "namespace-missing",
        }}));
    }

    let metadata_val = metadata.clone();
    let result = sessions::update_session_metadata(
        &store.conn(), sid, &metadata_val, expected_version, namespace, true,
    );

    let response = match &result {
        VersionedUpdateResult::Success { version, value } => {
            // Broadcast update
            let update = serde_json::json!({
                "event": "update",
                "data": {
                    "id": uuid::Uuid::new_v4().to_string(),
                    "seq": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "createdAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "body": {
                        "t": "update-session",
                        "sid": sid,
                        "metadata": {"version": version, "value": metadata},
                        "agentState": null,
                    }
                }
            });
            conn_mgr.broadcast_to_session(sid, &update.to_string(), Some(conn_id)).await;
            sync_engine.handle_realtime_event(
                hapir_shared::schemas::SyncEvent::SessionUpdated {
                    session_id: sid.to_string(),
                    namespace: Some(namespace.to_string()),
                    data: Some(serde_json::json!({"sid": sid})),
                },
            ).await;
            serde_json::json!({"result": "success", "version": version, "metadata": value})
        }
        VersionedUpdateResult::VersionMismatch { version, value } => {
            serde_json::json!({"result": "version-mismatch", "version": version, "metadata": value})
        }
        VersionedUpdateResult::Error => {
            serde_json::json!({"result": "error"})
        }
    };

    Some(response)
}

async fn handle_update_state(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let sid = data.get("sid").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let agent_state = data.get("agentState");

    use crate::store::{sessions, types::VersionedUpdateResult};
    if let Err(err) = resolve_session_access(store, sid, namespace) {
        return Some(serde_json::json!({"result": "error", "reason": match err {
            AccessError::AccessDenied => "access-denied",
            AccessError::NotFound => "not-found",
            AccessError::NamespaceMissing => "namespace-missing",
        }}));
    }

    let result = sessions::update_session_agent_state(
        &store.conn(), sid, agent_state, expected_version, namespace,
    );

    let response = match &result {
        VersionedUpdateResult::Success { version, value } => {
            let update = serde_json::json!({
                "event": "update",
                "data": {
                    "id": uuid::Uuid::new_v4().to_string(),
                    "seq": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "createdAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "body": {
                        "t": "update-session",
                        "sid": sid,
                        "metadata": null,
                        "agentState": {"version": version, "value": agent_state},
                    }
                }
            });
            conn_mgr.broadcast_to_session(sid, &update.to_string(), Some(conn_id)).await;
            sync_engine.handle_realtime_event(
                hapir_shared::schemas::SyncEvent::SessionUpdated {
                    session_id: sid.to_string(),
                    namespace: Some(namespace.to_string()),
                    data: Some(serde_json::json!({"sid": sid})),
                },
            ).await;
            serde_json::json!({"result": "success", "version": version, "agentState": value})
        }
        VersionedUpdateResult::VersionMismatch { version, value } => {
            serde_json::json!({"result": "version-mismatch", "version": version, "agentState": value})
        }
        VersionedUpdateResult::Error => {
            serde_json::json!({"result": "error"})
        }
    };

    Some(response)
}

async fn handle_session_alive(
    conn_id: &str,
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_mgr: &Arc<ConnectionManager>,
) {
    let sid = match data.get("sid").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let time = match data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => return,
    };

    if let Err(err) = resolve_session_access(store, &sid, namespace) {
        emit_access_error(conn_mgr, conn_id, "session", &sid, err).await;
        return;
    }

    let thinking = data.get("thinking").and_then(|v| v.as_bool());
    let _mode = data.get("mode").and_then(|v| v.as_str()); // accepted for protocol compat
    let permission_mode = data.get("permissionMode")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let model_mode = data.get("modelMode")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    sync_engine.handle_session_alive(&sid, time, thinking, permission_mode, model_mode).await;
}

async fn handle_session_end(
    conn_id: &str,
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_mgr: &Arc<ConnectionManager>,
) {
    let sid = match data.get("sid").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let time = match data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => return,
    };

    if let Err(err) = resolve_session_access(store, &sid, namespace) {
        emit_access_error(conn_mgr, conn_id, "session", &sid, err).await;
        return;
    }

    sync_engine.handle_session_end(&sid, time).await;
}

async fn handle_machine_alive(
    conn_id: &str,
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_mgr: &Arc<ConnectionManager>,
) {
    let machine_id = match data.get("machineId").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let time = match data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => return,
    };

    if let Err(err) = resolve_machine_access(store, &machine_id, namespace) {
        emit_access_error(conn_mgr, conn_id, "machine", &machine_id, err).await;
        return;
    }

    sync_engine.handle_machine_alive(&machine_id, time).await;
}

async fn handle_machine_update_metadata(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let mid = data.get("machineId").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let metadata = data.get("metadata")?;

    use crate::store::{machines, types::VersionedUpdateResult};
    if let Err(err) = resolve_machine_access(store, mid, namespace) {
        return Some(serde_json::json!({"result": "error", "reason": match err {
            AccessError::AccessDenied => "access-denied",
            AccessError::NotFound => "not-found",
            AccessError::NamespaceMissing => "namespace-missing",
        }}));
    }

    let result = machines::update_machine_metadata(&store.conn(), mid, &metadata.clone(), expected_version, namespace);

    let response = match &result {
        VersionedUpdateResult::Success { version, value } => {
            let update = serde_json::json!({
                "event": "update",
                "data": {
                    "id": uuid::Uuid::new_v4().to_string(),
                    "seq": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "createdAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "body": {
                        "t": "update-machine",
                        "machineId": mid,
                        "metadata": {"version": version, "value": metadata},
                        "runnerState": null,
                    }
                }
            });
            conn_mgr.broadcast_to_machine(mid, &update.to_string(), Some(conn_id)).await;
            sync_engine.handle_realtime_event(
                hapir_shared::schemas::SyncEvent::MachineUpdated {
                    machine_id: mid.to_string(),
                    namespace: Some(namespace.to_string()),
                    data: Some(serde_json::json!({"id": mid})),
                },
            ).await;
            serde_json::json!({"result": "success", "version": version, "metadata": value})
        }
        VersionedUpdateResult::VersionMismatch { version, value } => {
            serde_json::json!({"result": "version-mismatch", "version": version, "metadata": value})
        }
        VersionedUpdateResult::Error => {
            serde_json::json!({"result": "error"})
        }
    };

    Some(response)
}

async fn handle_machine_update_state(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<SyncEngine>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let mid = data.get("machineId").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let runner_state = data.get("runnerState");

    use crate::store::{machines, types::VersionedUpdateResult};
    if let Err(err) = resolve_machine_access(store, mid, namespace) {
        return Some(serde_json::json!({"result": "error", "reason": match err {
            AccessError::AccessDenied => "access-denied",
            AccessError::NotFound => "not-found",
            AccessError::NamespaceMissing => "namespace-missing",
        }}));
    }

    let result = machines::update_machine_runner_state(&store.conn(), mid, runner_state, expected_version, namespace);

    let response = match &result {
        VersionedUpdateResult::Success { version, value } => {
            let update = serde_json::json!({
                "event": "update",
                "data": {
                    "id": uuid::Uuid::new_v4().to_string(),
                    "seq": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "createdAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64,
                    "body": {
                        "t": "update-machine",
                        "machineId": mid,
                        "metadata": null,
                        "runnerState": {"version": version, "value": runner_state},
                    }
                }
            });
            conn_mgr.broadcast_to_machine(mid, &update.to_string(), Some(conn_id)).await;
            sync_engine.handle_realtime_event(
                hapir_shared::schemas::SyncEvent::MachineUpdated {
                    machine_id: mid.to_string(),
                    namespace: Some(namespace.to_string()),
                    data: Some(serde_json::json!({"id": mid})),
                },
            ).await;
            serde_json::json!({"result": "success", "version": version, "runnerState": value})
        }
        VersionedUpdateResult::VersionMismatch { version, value } => {
            serde_json::json!({"result": "version-mismatch", "version": version, "runnerState": value})
        }
        VersionedUpdateResult::Error => {
            serde_json::json!({"result": "error"})
        }
    };

    Some(response)
}

// ---------- Terminal event forwarding (CLI → webapp) ----------

/// Forward terminal:ready, terminal:output, terminal:error from CLI to the webapp terminal socket.
/// `should_mark_activity` is true for ready/output, false for error (matching TS behavior).
async fn forward_terminal_event(
    conn_id: &str,
    event: &str,
    data: &Value,
    conn_mgr: &Arc<ConnectionManager>,
    terminal_registry: &Arc<RwLock<TerminalRegistry>>,
    should_mark_activity: bool,
) {
    let terminal_id = match data.get("terminalId").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return,
    };

    let entry = {
        let reg = terminal_registry.read().await;
        match reg.get(&terminal_id) {
            Some(e) if e.cli_socket_id == conn_id => e.clone(),
            _ => return,
        }
    };

    if data.get("sessionId").and_then(|v| v.as_str()) != Some(&entry.session_id) {
        return;
    }

    if should_mark_activity {
        terminal_registry.write().await.mark_activity(&terminal_id);
    }

    let msg = serde_json::json!({
        "event": event,
        "data": data,
    });
    conn_mgr.send_to(&entry.socket_id, &msg.to_string()).await;
}

/// Handle terminal:exit from CLI — forward to webapp and remove from registry.
async fn handle_terminal_exit_from_cli(
    conn_id: &str,
    data: &Value,
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
            Some(e) if e.cli_socket_id == conn_id => e.clone(),
            _ => return,
        }
    };

    if data.get("sessionId").and_then(|v| v.as_str()) != Some(&entry.session_id) {
        return;
    }

    terminal_registry.write().await.remove(&terminal_id);

    let msg = serde_json::json!({
        "event": "terminal:exit",
        "data": data,
    });
    conn_mgr.send_to(&entry.socket_id, &msg.to_string()).await;
}
