use std::convert::Infallible;

use axum::{
    Json, Router,
    extract::{Extension, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures::stream::Stream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::sync::sse_manager::SseMessage;
use crate::sync::visibility_tracker::VisibilityState;
use crate::web::AppState;
use crate::web::middleware::auth::AuthContext;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/events", get(events_handler))
        .route("/visibility", post(set_visibility))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsQuery {
    all: Option<String>,
    session_id: Option<String>,
    machine_id: Option<String>,
    visibility: Option<String>,
}

async fn events_handler(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<EventsQuery>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    let all = parse_bool(&query.all);
    let session_id = parse_optional_id(query.session_id.as_deref());
    let machine_id = parse_optional_id(query.machine_id.as_deref());
    let visibility = parse_visibility(query.visibility.as_deref());
    let subscription_id = uuid::Uuid::new_v4().to_string();
    let namespace = auth.namespace.clone();

    let mut resolved_session_id = session_id.clone();

    // Validate session_id and machine_id if provided
    if let Some(ref sid) = session_id {
        let access = state
            .sync_engine
            .resolve_session_access(sid, &namespace)
            .await;
        match access {
            Ok((resolved_id, _session)) => {
                resolved_session_id = Some(resolved_id);
            }
            Err("access-denied") => {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "Session access denied"})),
                ));
            }
            Err(_) => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Session not found"})),
                ));
            }
        }
    }

    if let Some(ref mid) = machine_id {
        let machine = state.sync_engine.get_machine(mid).await;
        match machine {
            None => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Machine not found"})),
                ));
            }
            Some(m) if m.namespace != namespace => {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "Machine access denied"})),
                ));
            }
            _ => {}
        }
    }

    // Subscribe via the SSE manager
    let rx = {
        let (rx, _sub) = state.sync_engine.subscribe_sse(
            subscription_id.clone(),
            namespace.clone(),
            all,
            resolved_session_id,
            machine_id,
            visibility,
        );
        rx
    };

    // Build the SSE stream from the mpsc receiver
    let sub_id_for_cleanup = subscription_id.clone();
    let state_for_cleanup = state.clone();

    let initial_event = Event::default().data(
        serde_json::to_string(&json!({
            "type": "connection-changed",
            "data": {
                "status": "connected",
                "subscriptionId": subscription_id
            }
        }))
        .unwrap_or_default(),
    );

    let receiver_stream = UnboundedReceiverStream::new(rx);

    let event_stream = futures::stream::once(async move { Ok(initial_event) }).chain(
        receiver_stream.map(move |msg| {
            Ok(match msg {
                SseMessage::Event(sync_event) => {
                    let data =
                        serde_json::to_string(&sync_event).unwrap_or_else(|_| "{}".to_string());
                    Event::default().data(data)
                }
                SseMessage::Heartbeat => Event::default().comment("heartbeat"),
            })
        }),
    );

    // Wrap with cleanup on drop
    let cleanup_stream = CleanupStream {
        inner: Box::pin(event_stream),
        state: Some(state_for_cleanup),
        subscription_id: Some(sub_id_for_cleanup),
    };

    Ok(Sse::new(cleanup_stream).keep_alive(KeepAlive::default()))
}

/// A stream wrapper that unsubscribes from the SSE manager when dropped.
struct CleanupStream<S> {
    inner: std::pin::Pin<Box<S>>,
    state: Option<AppState>,
    subscription_id: Option<String>,
}

impl<S> Drop for CleanupStream<S> {
    fn drop(&mut self) {
        if let (Some(state), Some(sub_id)) = (self.state.take(), self.subscription_id.take()) {
            state.sync_engine.unsubscribe_sse(&sub_id);
        }
    }
}

impl<S> Stream for CleanupStream<S>
where
    S: Stream,
{
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

// --- Visibility ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VisibilityBody {
    subscription_id: String,
    visibility: String,
}

async fn set_visibility(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    body: Result<Json<VisibilityBody>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid body"})),
            );
        }
    };

    if body.subscription_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid body"})),
        );
    }

    let vis = if body.visibility == "visible" {
        VisibilityState::Visible
    } else {
        VisibilityState::Hidden
    };

    let updated = state
        .sync_engine
        .set_sse_visibility(&body.subscription_id, &auth.namespace, vis);

    if !updated {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Subscription not found"})),
        );
    }

    (StatusCode::OK, Json(json!({"ok": true})))
}

// --- Helpers ---

fn parse_bool(value: &Option<String>) -> bool {
    matches!(value.as_deref(), Some("true") | Some("1"))
}

fn parse_optional_id(value: Option<&str>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_visibility(value: Option<&str>) -> VisibilityState {
    match value {
        Some("visible") => VisibilityState::Visible,
        _ => VisibilityState::Hidden,
    }
}
