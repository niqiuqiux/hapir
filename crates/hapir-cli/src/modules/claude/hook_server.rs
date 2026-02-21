use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use tokio::net::TcpListener;
use tracing::debug;

/// Data received from Claude's SessionStart hook.
pub type SessionHookData = HashMap<String, serde_json::Value>;

/// Callback invoked when a session hook is received with a valid session ID.
pub type OnSessionHook = Arc<dyn Fn(String, SessionHookData) + Send + Sync>;

/// A running hook server handle.
pub struct HookServer {
    pub port: u16,
    pub token: String,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
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
}

/// Start a dedicated HTTP server on 127.0.0.1:0 for receiving Claude session hooks.
///
/// The server exposes a single `POST /hook/session-start` endpoint that is
/// token-authenticated via the `x-hapi-hook-token` header.
pub async fn start_hook_server(
    on_session_hook: OnSessionHook,
    token: Option<String>,
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
    });

    let app = Router::new()
        .route("/hook/session-start", post(handle_session_start))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let port = addr.port();

    debug!("[hookServer] Started on port {}", port);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

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
    // Check token
    let provided_token = headers
        .get("x-hapi-hook-token")
        .and_then(|v| v.to_str().ok());

    match provided_token {
        Some(t) if t == state.token => {}
        _ => {
            debug!("[hookServer] Unauthorized hook request");
            return StatusCode::UNAUTHORIZED;
        }
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
