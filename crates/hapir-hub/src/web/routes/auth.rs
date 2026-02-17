use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::cli_api_token::{constant_time_eq, parse_access_token};
use crate::config::owner_id::get_or_create_owner_id;
use crate::web::middleware::auth::JwtClaims;
use crate::web::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthBody {
    access_token: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/auth", post(auth_handler))
}

async fn auth_handler(
    State(state): State<AppState>,
    Json(body): Json<AuthBody>,
) -> (StatusCode, Json<Value>) {
    let parsed = match parse_access_token(&body.access_token) {
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

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let owner_id = get_or_create_owner_id(&state.data_dir).unwrap_or(0);

    let claims = JwtClaims {
        uid: owner_id,
        ns: parsed.namespace,
        exp: now + 15 * 60,
    };

    let header = Header::new(Algorithm::HS256);
    let key = EncodingKey::from_secret(&state.jwt_secret);

    match encode(&header, &claims, &key) {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({
                "token": token,
                "user": {"id": owner_id}
            })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to create token"})),
        ),
    }
}
