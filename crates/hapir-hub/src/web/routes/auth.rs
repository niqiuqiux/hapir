use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

use crate::config::cli_api_token::{constant_time_eq, parse_access_token};
use crate::config::owner_id::get_or_create_owner_id;
use crate::store::users;
use crate::web::middleware::auth::JwtClaims;
use crate::web::telegram_init_data::{validate_telegram_init_data, TelegramInitDataValidation};
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/auth", post(auth_handler))
}

async fn auth_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let has_init_data = body.get("initData").and_then(|v| v.as_str()).is_some();
    let has_access_token = body.get("accessToken").and_then(|v| v.as_str()).is_some();

    if !has_init_data && !has_access_token {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid body"})),
        );
    }

    let namespace: String;
    let mut username: Option<String> = None;
    let mut first_name: Option<String> = None;
    let mut last_name: Option<String> = None;

    if has_access_token {
        // CLI access token authentication
        let token_str = body["accessToken"].as_str().unwrap();
        let parsed = match parse_access_token(token_str) {
            Some(p) => p,
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Invalid access token"})),
                );
            }
        };

        if !constant_time_eq(&parsed.base_token, &state.cli_api_token) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Invalid access token"})),
            );
        }

        first_name = Some("Web User".to_string());
        namespace = parsed.namespace;
    } else {
        // Telegram initData authentication
        let bot_token = match &state.telegram_bot_token {
            Some(t) => t.clone(),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error": "Telegram authentication is disabled. Configure TELEGRAM_BOT_TOKEN."})),
                );
            }
        };

        let init_data = body["initData"].as_str().unwrap();
        let result = validate_telegram_init_data(init_data, &bot_token, 300);
        let tg_user = match result {
            TelegramInitDataValidation::Ok { user, .. } => user,
            TelegramInitDataValidation::Err(e) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": e})),
                );
            }
        };

        let telegram_user_id = tg_user.id.to_string();
        let conn = state.store.conn();
        let stored_user = users::get_user(&conn, "telegram", &telegram_user_id);
        drop(conn);

        let stored_user = match stored_user {
            Some(u) => u,
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "not_bound"})),
                );
            }
        };

        username = tg_user.username;
        first_name = tg_user.first_name;
        last_name = tg_user.last_name;
        namespace = stored_user.namespace;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let owner_id = get_or_create_owner_id(&state.data_dir).unwrap_or(0);

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
                    "username": username,
                    "firstName": first_name,
                    "lastName": last_name,
                }
            })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to create token"})),
        ),
    }
}
