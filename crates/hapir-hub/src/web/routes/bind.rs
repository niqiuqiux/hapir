use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::cli_api_token::{constant_time_eq, parse_access_token};
use crate::config::owner_id::get_or_create_owner_id;
use crate::store::users;
use crate::web::middleware::auth::JwtClaims;
use crate::web::telegram_init_data::{validate_telegram_init_data, TelegramInitDataValidation};
use crate::web::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindBody {
    init_data: String,
    access_token: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/bind", post(bind_handler))
}

/// Bind a Telegram account to a CLI namespace, enabling Telegram-based login and notifications.
async fn bind_handler(
    State(state): State<AppState>,
    Json(body): Json<BindBody>,
) -> (StatusCode, Json<Value>) {
    // Check that Telegram bot token is configured.
    let bot_token = match &state.telegram_bot_token {
        Some(t) => t.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Telegram bot not configured"})),
            );
        }
    };

    // Validate the Telegram init data.
    let validation = validate_telegram_init_data(&body.init_data, &bot_token, 300);
    let tg_user = match validation {
        TelegramInitDataValidation::Ok { user, .. } => user,
        TelegramInitDataValidation::Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": format!("Invalid init data: {e}")})),
            );
        }
    };

    // Parse the access token.
    let parsed = match parse_access_token(&body.access_token) {
        Some(p) => p,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Invalid access token"})),
            );
        }
    };

    // Verify the base token matches the CLI API token.
    if !constant_time_eq(&parsed.base_token, &state.cli_api_token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid access token"})),
        );
    }

    let namespace = parsed.namespace;
    let platform_user_id = tg_user.id.to_string();

    // Get or create user in the store.
    let conn = state.store.conn();
    let existing_user = users::get_user(&conn, "telegram", &platform_user_id);

    if let Some(ref existing) = existing_user {
        // Check if user is already bound to a different namespace.
        if existing.namespace != namespace {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "already_bound"})),
            );
        }
    }

    // Create the user if it does not exist yet.
    if existing_user.is_none() {
        if let Err(e) = users::add_user(&conn, "telegram", &platform_user_id, &namespace) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to create user: {e}")})),
            );
        }
    }

    // Drop the connection guard before potentially blocking calls.
    drop(conn);

    // Get or create the owner ID.
    let owner_id = match get_or_create_owner_id(&state.data_dir) {
        Ok(id) => id,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Failed to get owner ID: {e}")})),
            );
        }
    };

    // Issue JWT (HS256, 15 minute expiry).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let claims = JwtClaims {
        uid: owner_id,
        ns: namespace,
        exp: now + 15 * 60,
    };

    let header = Header::new(Algorithm::HS256);
    let key = EncodingKey::from_secret(&state.jwt_secret);

    match encode(&header, &claims, &key) {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({
                "token": token,
                "user": {
                    "id": owner_id,
                    "username": tg_user.username,
                    "firstName": tg_user.first_name,
                    "lastName": tg_user.last_name,
                }
            })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to create token"})),
        ),
    }
}
