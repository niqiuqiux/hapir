use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::web::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    pub uid: i64,
    pub ns: String,
    pub exp: u64,
}

/// Auth context extracted from JWT, stored in request extensions.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: i64,
    pub namespace: String,
}

/// JWT auth middleware. Skips /auth and /bind paths.
pub async fn jwt_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, Response> {
    let path = req.uri().path();

    // Skip JWT auth for non-API routes and public API endpoints
    if !path.starts_with("/api/") || path == "/api/auth" || path == "/api/bind" {
        return Ok(next.run(req).await);
    }

    let token = extract_bearer_token(&req).or_else(|| {
        // Only allow query param token for /api/events
        if path == "/api/events" {
            extract_query_token(&req)
        } else {
            None
        }
    });

    let token = match token {
        Some(t) => t,
        None => {
            return Err((StatusCode::UNAUTHORIZED, Json(json!({"error": "Missing authorization token"}))).into_response());
        }
    };

    let validation = Validation::new(Algorithm::HS256);
    let key = DecodingKey::from_secret(&state.jwt_secret);

    let data = match decode::<JwtClaims>(&token, &key, &validation) {
        Ok(d) => d,
        Err(_) => {
            return Err((StatusCode::UNAUTHORIZED, Json(json!({"error": "Invalid token"}))).into_response());
        }
    };

    req.extensions_mut().insert(AuthContext {
        user_id: data.claims.uid,
        namespace: data.claims.ns,
    });

    Ok(next.run(req).await)
}

fn extract_bearer_token(req: &Request) -> Option<String> {
    req.headers()
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(|s| s.to_string())
}

fn extract_query_token(req: &Request) -> Option<String> {
    let query = req.uri().query()?;
    for pair in query.split('&') {
        if let Some(val) = pair.strip_prefix("token=") {
            return Some(val.to_string());
        }
    }
    None
}
