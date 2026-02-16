pub mod connection_manager;
pub mod handlers;
pub mod rpc_registry;
pub mod terminal_registry;

use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

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
    pub sync_engine: Arc<Mutex<SyncEngine>>,
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
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    };

    ws.on_upgrade(move |socket| {
        handle_cli_ws(socket, state, namespace, query.session_id, query.machine_id)
    })
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
        None => return axum::http::StatusCode::UNAUTHORIZED.into_response(),
    };

    ws.on_upgrade(move |socket| {
        handle_terminal_ws(socket, state, claims.namespace)
    })
    .into_response()
}

struct JwtClaims {
    namespace: String,
}

fn verify_jwt(token: &str, secret: &[u8]) -> Option<JwtClaims> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    #[derive(serde::Deserialize)]
    struct Claims {
        ns: String,
    }

    let mut validation = Validation::new(Algorithm::HS256);
    validation.required_spec_claims.clear();

    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret),
        &validation,
    ).ok()?;

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
    let conn_id = uuid::Uuid::new_v4().to_string();
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<WsOutMessage>();

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

    debug!(conn_id = %conn_id, namespace = %namespace, "CLI WebSocket connected");

    // Outgoing message pump
    let _conn_id_out = conn_id.clone();
    let out_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let result = match msg {
                WsOutMessage::Text(text) => ws_tx.send(Message::Text(text.into())).await,
                WsOutMessage::Close => ws_tx.send(Message::Close(None)).await,
            };
            if result.is_err() {
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
            Some(e) => e.to_string(),
            None => {
                // Might be an RPC response (ack)
                if let Some(id) = parsed.get("id").and_then(|v| v.as_str()) {
                    if let Some(event_str) = parsed.get("event").and_then(|v| v.as_str()) {
                        if event_str.ends_with(":ack") {
                            let result_val = parsed.get("data").cloned().unwrap_or(Value::Null);
                            state.conn_mgr.handle_rpc_response(id, Ok(result_val)).await;
                            continue;
                        }
                    }
                    // Generic ack
                    let result_val = parsed.get("data").cloned().unwrap_or(Value::Null);
                    state.conn_mgr.handle_rpc_response(id, Ok(result_val)).await;
                }
                continue;
            }
        };

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
        ).await;

        // Send ack if this was a request
        if let Some(rid) = request_id {
            let ack = serde_json::json!({
                "id": rid,
                "event": format!("{event}:ack"),
                "data": response.unwrap_or(Value::Null),
            });
            state.conn_mgr.send_to(&conn_id, &ack.to_string()).await;
        }
    }

    // Cleanup
    debug!(conn_id = %conn_id, "CLI WebSocket disconnected");
    handlers::terminal::cleanup_cli_disconnect(&conn_id, &state.conn_mgr, &state.terminal_registry).await;
    state.conn_mgr.remove_connection(&conn_id).await;
    out_task.abort();
}

async fn handle_terminal_ws(
    socket: WebSocket,
    state: WsState,
    namespace: String,
) {
    let conn_id = uuid::Uuid::new_v4().to_string();
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<WsOutMessage>();

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
    let out_task = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let result = match msg {
                WsOutMessage::Text(text) => ws_tx.send(Message::Text(text.into())).await,
                WsOutMessage::Close => ws_tx.send(Message::Close(None)).await,
            };
            if result.is_err() {
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

        // Also handle forwarded terminal events from CLI (ready, output, exit, error)
        match event.as_str() {
            "terminal:ready" | "terminal:output" | "terminal:exit" | "terminal:error" => {
                // These come from CLI → forward to terminal socket
                // But in our architecture, the terminal socket also sends create/write/resize/close
                // Fall through to terminal handler
            }
            _ => {}
        }

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
        ).await;

        if let Some(rid) = request_id {
            let ack = serde_json::json!({
                "id": rid,
                "event": format!("{event}:ack"),
                "data": response.unwrap_or(Value::Null),
            });
            state.conn_mgr.send_to(&conn_id, &ack.to_string()).await;
        }
    }

    debug!(conn_id = %conn_id, "Terminal WebSocket disconnected");
    handlers::terminal::cleanup_terminal_disconnect(&conn_id, &state.conn_mgr, &state.terminal_registry).await;
    state.conn_mgr.remove_connection(&conn_id).await;
    out_task.abort();
}
