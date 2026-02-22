use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tracing::debug;

use crate::agent::session_base::SessionMode;
use hapir_infra::ws::session_client::WsSessionClient;

/// Data received from Claude's SessionStart hook.
pub type SessionHookData = HashMap<String, serde_json::Value>;

/// Callback invoked when a session hook is received with a valid session ID.
pub type OnSessionHook = Arc<dyn Fn(String, SessionHookData) + Send + Sync>;

/// Callback invoked when thinking state changes.
pub type OnThinkingChange = Arc<dyn Fn(bool) + Send + Sync>;

/// A running hook server handle.
pub struct HookServer {
    pub port: u16,
    pub token: String,
    shutdown_tx: oneshot::Sender<()>,
}

impl HookServer {
    /// Stop the hook server.
    pub fn stop(self) {
        let _ = self.shutdown_tx.send(());
        debug!("[hookServer] Stopped");
    }
}

struct AppState {
    token: String,
    on_session_hook: OnSessionHook,
    on_thinking_change: Option<OnThinkingChange>,
    ws_client: Option<Arc<WsSessionClient>>,
    session_mode: Arc<Mutex<SessionMode>>,
}

/// Start a dedicated HTTP server on 127.0.0.1:0 for receiving Claude session hooks.
///
/// The server exposes:
/// - `POST /hook/session-start` for session ID discovery
/// - `POST /hook/event` for generic hook events (thinking state, etc.)
pub async fn start_hook_server(
    on_session_hook: OnSessionHook,
    token: Option<String>,
    on_thinking_change: Option<OnThinkingChange>,
    ws_client: Option<Arc<WsSessionClient>>,
    session_mode: Arc<Mutex<SessionMode>>,
) -> anyhow::Result<HookServer> {
    let hook_token = token.unwrap_or_else(|| {
        use rand::RngCore;
        let mut rng = rand::thread_rng();
        let mut bytes = [0u8; 16];
        rng.fill_bytes(&mut bytes);
        hex::encode(bytes)
    });

    let state = Arc::new(AppState {
        token: hook_token.clone(),
        on_session_hook,
        on_thinking_change,
        ws_client,
        session_mode,
    });

    let app = Router::new()
        .route("/hook/session-start", post(handle_session_start))
        .route("/hook/event", post(handle_event))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let port = addr.port();

    debug!("[hookServer] Started on port {}", port);

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    Ok(HookServer {
        port,
        token: hook_token,
        shutdown_tx,
    })
}

async fn handle_session_start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    if !check_token(&state, &headers) {
        return StatusCode::UNAUTHORIZED;
    }

    debug!("[hookServer] Received session hook: {}", body);

    let data: SessionHookData = match serde_json::from_str(&body) {
        Ok(d) => d,
        Err(_) => {
            debug!("[hookServer] Failed to parse hook data as JSON");
            return StatusCode::BAD_REQUEST;
        }
    };

    let session_id = data
        .get("session_id")
        .or_else(|| data.get("sessionId"))
        .and_then(|v: &serde_json::Value| v.as_str())
        .map(|s: &str| s.to_string());

    match session_id {
        Some(sid) => {
            debug!("[hookServer] Session hook received session ID: {}", sid);
            (state.on_session_hook)(sid, data);
            StatusCode::OK
        }
        None => {
            debug!("[hookServer] Session hook received but no session_id found");
            StatusCode::UNPROCESSABLE_ENTITY
        }
    }
}

async fn handle_event(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    if !check_token(&state, &headers) {
        return StatusCode::UNAUTHORIZED;
    }

    let data: serde_json::Value = match serde_json::from_str(&body) {
        Ok(d) => d,
        Err(_) => {
            debug!("[hookServer] Failed to parse event data as JSON");
            return StatusCode::BAD_REQUEST;
        }
    };

    let event_name = data
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    debug!("[hookServer] Received event: {}", event_name);

    let is_local = *state.session_mode.lock().unwrap() == SessionMode::Local;

    match event_name {
        "UserPromptSubmit" => {
            if let Some(ref cb) = state.on_thinking_change {
                cb(true);
            }
            // Only forward user messages in local mode; in remote mode the SDK
            // already emits a User message that the remote launcher forwards.
            if is_local {
                if let Some(ref ws) = state.ws_client {
                    let prompt = data
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !prompt.is_empty() {
                        ws.send_message(serde_json::json!({
                            "role": "user",
                            "content": { "type": "text", "text": prompt },
                            "meta": { "sentFrom": "cli" }
                        }))
                        .await;
                    }
                }
            }
        }
        "Stop" => {
            if let Some(ref cb) = state.on_thinking_change {
                cb(false);
            }
            // Only forward assistant messages in local mode; in remote mode the
            // remote launcher sends the full message with thinking blocks + usage.
            if is_local {
                if let Some(ref ws) = state.ws_client {
                    let text = data
                        .get("last_assistant_message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !text.is_empty() {
                        ws.send_message(serde_json::json!({
                            "role": "assistant",
                            "content": {
                                "type": "output",
                                "data": {
                                    "type": "assistant",
                                    "message": {
                                        "role": "assistant",
                                        "content": [
                                            { "type": "text", "text": text }
                                        ]
                                    }
                                }
                            },
                            "meta": { "sentFrom": "cli" }
                        }))
                        .await;
                    }
                }
            }
        }
        other => {
            debug!("[hookServer] Unhandled event type: {}", other);
        }
    }

    StatusCode::OK
}

fn check_token(state: &AppState, headers: &HeaderMap) -> bool {
    let provided_token = headers
        .get("x-hapir-hook-token")
        .and_then(|v| v.to_str().ok());

    match provided_token {
        Some(t) if t == state.token => true,
        _ => {
            debug!("[hookServer] Unauthorized hook request");
            false
        }
    }
}
