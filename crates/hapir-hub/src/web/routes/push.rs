use axum::{
    extract::{Extension, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::store::push_subscriptions::{self, PushSubscriptionInput};
use crate::web::middleware::auth::AuthContext;
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/push/vapid-public-key", get(vapid_public_key))
        .route("/push/subscribe", post(subscribe))
        .route("/push/subscribe", delete(unsubscribe))
}

async fn vapid_public_key(
    State(state): State<AppState>,
) -> (StatusCode, Json<Value>) {
    match &state.vapid_public_key {
        Some(key) => (StatusCode::OK, Json(json!({"publicKey": key}))),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "VAPID public key not configured"})),
        ),
    }
}

#[derive(Deserialize)]
struct PushKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct SubscribeBody {
    endpoint: String,
    keys: PushKeys,
}

async fn subscribe(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    body: Result<Json<SubscribeBody>, axum::extract::rejection::JsonRejection>,
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

    if body.endpoint.is_empty() || body.keys.p256dh.is_empty() || body.keys.auth.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid body"})),
        );
    }

    let conn = state.store.conn();
    push_subscriptions::add_push_subscription(
        &conn,
        &auth.namespace,
        &PushSubscriptionInput {
            endpoint: &body.endpoint,
            p256dh: &body.keys.p256dh,
            auth: &body.keys.auth,
        },
    );

    (StatusCode::OK, Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct UnsubscribeBody {
    endpoint: String,
}

async fn unsubscribe(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    body: Result<Json<UnsubscribeBody>, axum::extract::rejection::JsonRejection>,
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

    if body.endpoint.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid body"})),
        );
    }

    let conn = state.store.conn();
    push_subscriptions::remove_push_subscription(&conn, &auth.namespace, &body.endpoint);

    (StatusCode::OK, Json(json!({"ok": true})))
}
