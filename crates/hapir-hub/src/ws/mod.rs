pub mod connection_manager;
pub mod handlers;
pub mod rpc_registry;
pub mod terminal_registry;

use std::sync::Arc;
use std::time::SystemTime;
use axum::http::StatusCode;
use axum::{
    extract::{
        ws::{Message, WebSocket}, Query, State,
        WebSocketUpgrade,
    },
    response::IntoResponse,
    Router,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;
use hapir_shared::ws_protocol::WsMessage;
use crate::config::cli_api_token;
use crate::store::Store;
use crate::sync::SyncEngine;
use connection_manager::{ConnectionManager, WsConnType, WsConnection, WsOutMessage};
use terminal_registry::TerminalRegistry;

const DEFAULT_MAX_TERMINALS: usize = 4;
const DEFAULT_IDLE_TIMEOUT_MS: u64 = 15 * 60_000;

/// Shared state for WebSocket handlers.
#[derive(Clone)]
pub struct WsState {
    pub store: Arc<Store>,
    pub sync_engine: Arc<SyncEngine>,
    pub conn_mgr: Arc<ConnectionManager>,
    pub terminal_registry: Arc<RwLock<TerminalRegistry>>,
    pub cli_api_token: String,
    pub jwt_secret: Vec<u8>,
    pub max_terminals_per_socket: usize,
    pub max_terminals_per_session: usize,
}

pub fn ws_router(state: WsState) -> Router {
    Router::new()
        .route("/ws/cli", axum::routing::get(ws_cli_upgrade))
        .route("/ws/terminal", axum::routing::get(ws_terminal_upgrade))
        .with_state(state)
}

#[derive(Deserialize)]
struct WsCliQuery {
    token: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "machineId")]
    machine_id: Option<String>,
    /// CLI client sends clientType + scopeId instead of sessionId/machineId
    #[serde(rename = "clientType")]
    client_type: Option<String>,
    #[serde(rename = "scopeId")]
    scope_id: Option<String>,
}

async fn ws_cli_upgrade(
    State(state): State<WsState>,
    Query(query): Query<WsCliQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = query.token.unwrap_or_default();
    let parsed = cli_api_token::parse_access_token(&token);

    let namespace = match parsed {
        Some(ref p) if cli_api_token::constant_time_eq(&p.base_token, &state.cli_api_token) => {
            p.namespace.clone()
        }
        _ => {
            warn!(
                machine_id = ?query.machine_id,
                session_id = ?query.session_id,
                "CLI WebSocket 认证失败 (token 无效)"
            );
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    // Resolve session_id/machine_id from clientType+scopeId if not provided directly
    let (session_id, machine_id) = match (query.session_id, query.machine_id) {
        (s @ Some(_), m) => (s, m),
        (None, m @ Some(_)) => (None, m),
        (None, None) => match (query.client_type.as_deref(), query.scope_id) {
            (Some("session-scoped"), Some(id)) => (Some(id), None),
            (Some("machine-scoped"), Some(id)) => (None, Some(id)),
            _ => (None, None),
        },
    };

    ws.on_upgrade(move |socket| handle_cli_ws(socket, state, namespace, session_id, machine_id))
        .into_response()
}

#[derive(Deserialize)]
struct WsTerminalQuery {
    token: Option<String>,
}

async fn ws_terminal_upgrade(
    State(state): State<WsState>,
    Query(query): Query<WsTerminalQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = query.token.unwrap_or_default();

    // Verify JWT
    let claims = match verify_jwt(&token, &state.jwt_secret) {
        Some(c) => c,
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    ws.on_upgrade(move |socket| handle_terminal_ws(socket, state, claims.namespace))
        .into_response()
}

struct JwtClaims {
    namespace: String,
}

fn verify_jwt(token: &str, secret: &[u8]) -> Option<JwtClaims> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Claims {
        ns: String,
        exp: u64,
    }

    let validation = Validation::new(Algorithm::HS256);

    let data = decode::<Claims>(token, &DecodingKey::from_secret(secret), &validation).ok()?;

    Some(JwtClaims {
        namespace: data.claims.ns,
    })
}

async fn handle_cli_ws(
    socket: WebSocket,
    state: WsState,
    namespace: String,
    session_id: Option<String>,
    machine_id: Option<String>,
) {
    let conn_id = Uuid::new_v4().to_string();
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = unbounded_channel::<WsOutMessage>();

    // Register connection
    let conn = WsConnection {
        id: conn_id.clone(),
        namespace: namespace.clone(),
        session_id: session_id.clone(),
        machine_id: machine_id.clone(),
        conn_type: WsConnType::Cli,
        tx: out_tx,
    };
    state.conn_mgr.add_connection(conn).await;

    // Join session/machine rooms
    if let Some(ref sid) = session_id {
        state.conn_mgr.join_session(&conn_id, sid).await;
    }
    if let Some(ref mid) = machine_id {
        state.conn_mgr.join_machine(&conn_id, mid).await;
    }

    if machine_id.is_some() {
        info!(
            conn_id = %conn_id,
            namespace = %namespace,
            session_id = ?session_id,
            machine_id = ?machine_id,
            "Runner WebSocket 已连接"
        );
    } else {
        debug!(conn_id = %conn_id, namespace = %namespace, session_id = ?session_id, "CLI WebSocket connected");
    }

    // Outgoing message pump
    let conn_id_out = conn_id.clone();
    let out_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let result = match msg {
                WsOutMessage::Text(text) => ws_tx.send(Message::Text(text.into())).await,
                WsOutMessage::Close => {
                    let _ = ws_tx.send(Message::Close(None)).await;
                    break; // Drop ws_tx to force-close the connection
                }
            };
            if let Err(e) = result {
                debug!(conn_id = %conn_id_out, error = %e, "WebSocket send failed, closing outgoing pump");
                break;
            }
        }
    });

    // Incoming message processing
    while let Some(msg) = ws_rx.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            Message::Ping(_) => continue,
            Message::Pong(_) => continue,
            _ => continue,
        };

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event = match parsed.get("event").and_then(|v| v.as_str()) {
            Some(e) => {
                if e == "rpc-register" {
                    info!(conn_id = %conn_id, event = %e, data = ?parsed.get("data"), "CLI WS received rpc-register");
                }
                e.to_string()
            }
            None => {
                // No event field — treat as generic ack if it has an id
                if let Some(id) = parsed.get("id").and_then(|v| v.as_str()) {
                    let result_val = parsed.get("data").cloned().unwrap_or(Value::Null);
                    state.conn_mgr.handle_rpc_response(id, Ok(result_val)).await;
                }
                continue;
            }
        };

        // Handle RPC ack/response at transport level before dispatching to event handler
        if event.ends_with(":ack") || event == "rpc-response" {
            if let Some(id) = parsed.get("id").and_then(|v| v.as_str()) {
                let result_val = parsed.get("data").cloned().unwrap_or(Value::Null);
                let error = result_val
                    .get("error")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                if let Some(err) = error {
                    state.conn_mgr.handle_rpc_response(id, Err(err)).await;
                } else {
                    state.conn_mgr.handle_rpc_response(id, Ok(result_val)).await;
                }
            }
            continue;
        }

        let data = parsed.get("data").cloned().unwrap_or(Value::Null);
        let request_id = parsed.get("id").and_then(|v| v.as_str()).map(String::from);

        let response = handlers::cli::handle_cli_event(
            &conn_id,
            &namespace,
            &event,
            data,
            request_id.as_deref(),
            &state.sync_engine,
            &state.store,
            &state.conn_mgr,
            &state.terminal_registry,
        )
        .await;

        // Send ack if this was a request
        if let Some(rid) = request_id {
            let ack = WsMessage::ack(rid, &event, response.unwrap_or(Value::Null));
            state.conn_mgr.send_to(&conn_id, &serde_json::to_string(&ack).unwrap_or_default()).await;
        }
    }

    // Cleanup
    if let Some(ref mid) = machine_id {
        info!(conn_id = %conn_id, machine_id = %mid, "Runner WebSocket 已断开");
        // Mark machine offline immediately on disconnect
        state.sync_engine.mark_machine_offline(mid).await;
    }
    if let Some(ref sid) = session_id {
        info!(conn_id = %conn_id, session_id = %sid, "Session WebSocket 已断开");
        let now = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        state.sync_engine.handle_session_end(sid, now).await;
    }
    if machine_id.is_none() && session_id.is_none() {
        debug!(conn_id = %conn_id, "CLI WebSocket disconnected");
    }
    handlers::terminal::cleanup_cli_disconnect(&conn_id, &state.conn_mgr, &state.terminal_registry)
        .await;
    state.conn_mgr.remove_connection(&conn_id).await;
    out_task.abort();
}

async fn handle_terminal_ws(socket: WebSocket, state: WsState, namespace: String) {
    let conn_id = Uuid::new_v4().to_string();
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = unbounded_channel::<WsOutMessage>();

    let conn = WsConnection {
        id: conn_id.clone(),
        namespace: namespace.clone(),
        session_id: None,
        machine_id: None,
        conn_type: WsConnType::Terminal,
        tx: out_tx,
    };
    state.conn_mgr.add_connection(conn).await;

    debug!(conn_id = %conn_id, namespace = %namespace, "Terminal WebSocket connected");

    // Outgoing pump
    let conn_id_out = conn_id.clone();
    let out_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let result = match msg {
                WsOutMessage::Text(text) => ws_tx.send(Message::Text(text.into())).await,
                WsOutMessage::Close => {
                    let _ = ws_tx.send(Message::Close(None)).await;
                    break; // Drop ws_tx to force-close the connection
                }
            };
            if let Err(e) = result {
                debug!(conn_id = %conn_id_out, error = %e, "Terminal WebSocket send failed, closing outgoing pump");
                break;
            }
        }
    });

    // Incoming
    while let Some(msg) = ws_rx.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            Message::Ping(_) => continue,
            Message::Pong(_) => continue,
            _ => continue,
        };

        let parsed: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event = match parsed.get("event").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => continue,
        };
        let data = parsed.get("data").cloned().unwrap_or(Value::Null);
        let request_id = parsed.get("id").and_then(|v| v.as_str()).map(String::from);
        let response = handlers::terminal::handle_terminal_event(
            &conn_id,
            &namespace,
            &event,
            data,
            &state.sync_engine,
            &state.store,
            &state.conn_mgr,
            &state.terminal_registry,
            state.max_terminals_per_socket,
            state.max_terminals_per_session,
        )
        .await;

        if let Some(rid) = request_id {
            let ack = WsMessage::ack(rid, &event, response.unwrap_or(Value::Null));
            state.conn_mgr.send_to(&conn_id, &serde_json::to_string(&ack).unwrap_or_default()).await;
        }
    }

    debug!(conn_id = %conn_id, "Terminal WebSocket disconnected");
    handlers::terminal::cleanup_terminal_disconnect(
        &conn_id,
        &state.conn_mgr,
        &state.terminal_registry,
    )
    .await;
    state.conn_mgr.remove_connection(&conn_id).await;
    out_task.abort();
}
