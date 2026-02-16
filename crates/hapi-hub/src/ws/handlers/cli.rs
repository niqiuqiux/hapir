use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;
use tracing::debug;

use crate::store::Store;
use crate::sync::SyncEngine;
use crate::sync::todos::extract_todos_from_message_content;
use super::super::connection_manager::ConnectionManager;

/// Process an incoming WebSocket event from a CLI connection.
pub async fn handle_cli_event(
    conn_id: &str,
    namespace: &str,
    event: &str,
    data: Value,
    _request_id: Option<&str>,
    sync_engine: &Arc<Mutex<SyncEngine>>,
    store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    match event {
        "message" => handle_message(conn_id, namespace, data, sync_engine, store, conn_mgr).await,
        "update-metadata" => handle_update_metadata(namespace, data, store, sync_engine, conn_id, conn_mgr).await,
        "update-state" => handle_update_state(namespace, data, store, sync_engine, conn_id, conn_mgr).await,
        "session-alive" => {
            handle_session_alive(namespace, data, store, sync_engine).await;
            None
        }
        "session-end" => {
            handle_session_end(namespace, data, store, sync_engine).await;
            None
        }
        "machine-alive" => {
            handle_machine_alive(namespace, data, store, sync_engine).await;
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
        "rpc-response" => {
            // This is an ack to an RPC request we sent
            if let Some(rid) = data.get("id").and_then(|v| v.as_str()) {
                let result_val = data.get("result").cloned().unwrap_or(Value::Null);
                let error = data.get("error").and_then(|v| v.as_str()).map(String::from);
                if let Some(err) = error {
                    conn_mgr.handle_rpc_response(rid, Err(err)).await;
                } else {
                    conn_mgr.handle_rpc_response(rid, Ok(result_val)).await;
                }
            }
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
    sync_engine: &Arc<Mutex<SyncEngine>>,
    store: &Arc<Store>,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let sid = data.get("sid").and_then(|v| v.as_str())?;
    let local_id = data.get("localId").and_then(|v| v.as_str());

    // Validate session access
    {
        use crate::store::sessions;
        if sessions::get_session_by_namespace(&store.conn(), sid, namespace).is_none() {
            return None;
        }
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
    if let Some(ref todos) = todos {
        if let Ok(todos_val) = serde_json::to_value(todos) {
            use crate::store::sessions;
            if sessions::set_session_todos(&store.conn(), sid, Some(&todos_val), msg.created_at, namespace) {
                let mut engine = sync_engine.lock().await;
                engine.handle_realtime_event(hapi_shared::schemas::SyncEvent::SessionUpdated {
                    session_id: sid.to_string(),
                    namespace: None,
                    data: Some(serde_json::json!({"sid": sid})),
                });
            }
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
    {
        let mut engine = sync_engine.lock().await;
        engine.handle_realtime_event(hapi_shared::schemas::SyncEvent::MessageReceived {
            session_id: sid.to_string(),
            namespace: None,
            message: hapi_shared::schemas::DecryptedMessage {
                id: msg.id,
                seq: Some(msg.seq as f64),
                local_id: msg.local_id,
                content: msg.content.unwrap_or(Value::Null),
                created_at: msg.created_at as f64,
            },
        });
    }

    None
}

async fn handle_update_metadata(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<Mutex<SyncEngine>>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let sid = data.get("sid").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let metadata = data.get("metadata")?;

    use crate::store::{sessions, types::VersionedUpdateResult};
    if sessions::get_session_by_namespace(&store.conn(), sid, namespace).is_none() {
        return Some(serde_json::json!({"result": "error", "reason": "access-denied"}));
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
            sync_engine.lock().await.handle_realtime_event(
                hapi_shared::schemas::SyncEvent::SessionUpdated {
                    session_id: sid.to_string(),
                    namespace: None,
                    data: Some(serde_json::json!({"sid": sid})),
                },
            );
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
    sync_engine: &Arc<Mutex<SyncEngine>>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let sid = data.get("sid").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let agent_state = data.get("agentState");

    use crate::store::{sessions, types::VersionedUpdateResult};
    if sessions::get_session_by_namespace(&store.conn(), sid, namespace).is_none() {
        return Some(serde_json::json!({"result": "error", "reason": "access-denied"}));
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
            sync_engine.lock().await.handle_realtime_event(
                hapi_shared::schemas::SyncEvent::SessionUpdated {
                    session_id: sid.to_string(),
                    namespace: None,
                    data: Some(serde_json::json!({"sid": sid})),
                },
            );
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
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<Mutex<SyncEngine>>,
) {
    let sid = match data.get("sid").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let time = match data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => return,
    };

    use crate::store::sessions;
    if sessions::get_session_by_namespace(&store.conn(), &sid, namespace).is_none() {
        return;
    }

    let thinking = data.get("thinking").and_then(|v| v.as_bool());
    let permission_mode = data.get("permissionMode")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let model_mode = data.get("modelMode")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    sync_engine.lock().await.handle_session_alive(&sid, time, thinking, permission_mode, model_mode);
}

async fn handle_session_end(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<Mutex<SyncEngine>>,
) {
    let sid = match data.get("sid").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let time = match data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => return,
    };

    use crate::store::sessions;
    if sessions::get_session_by_namespace(&store.conn(), &sid, namespace).is_none() {
        return;
    }

    sync_engine.lock().await.handle_session_end(&sid, time);
}

async fn handle_machine_alive(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<Mutex<SyncEngine>>,
) {
    let machine_id = match data.get("machineId").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let time = match data.get("time").and_then(|v| v.as_i64()) {
        Some(t) => t,
        None => return,
    };

    use crate::store::machines;
    if machines::get_machine_by_namespace(&store.conn(), &machine_id, namespace).is_none() {
        return;
    }

    sync_engine.lock().await.handle_machine_alive(&machine_id, time);
}

async fn handle_machine_update_metadata(
    namespace: &str,
    data: Value,
    store: &Arc<Store>,
    sync_engine: &Arc<Mutex<SyncEngine>>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let mid = data.get("machineId").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let metadata = data.get("metadata")?;

    use crate::store::{machines, types::VersionedUpdateResult};
    if machines::get_machine_by_namespace(&store.conn(), mid, namespace).is_none() {
        return Some(serde_json::json!({"result": "error", "reason": "access-denied"}));
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
            sync_engine.lock().await.handle_realtime_event(
                hapi_shared::schemas::SyncEvent::MachineUpdated {
                    machine_id: mid.to_string(),
                    namespace: None,
                    data: Some(serde_json::json!({"id": mid})),
                },
            );
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
    sync_engine: &Arc<Mutex<SyncEngine>>,
    conn_id: &str,
    conn_mgr: &Arc<ConnectionManager>,
) -> Option<Value> {
    let mid = data.get("machineId").and_then(|v| v.as_str())?;
    let expected_version = data.get("expectedVersion").and_then(|v| v.as_i64())?;
    let runner_state = data.get("runnerState");

    use crate::store::{machines, types::VersionedUpdateResult};
    if machines::get_machine_by_namespace(&store.conn(), mid, namespace).is_none() {
        return Some(serde_json::json!({"result": "error", "reason": "access-denied"}));
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
            sync_engine.lock().await.handle_realtime_event(
                hapi_shared::schemas::SyncEvent::MachineUpdated {
                    machine_id: mid.to_string(),
                    namespace: None,
                    data: Some(serde_json::json!({"id": mid})),
                },
            );
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
