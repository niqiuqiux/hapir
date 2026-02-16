use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};

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
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    if path == "/api/auth" || path == "/api/bind" {
        return Ok(next.run(req).await);
    }

    let token = extract_bearer_token(&req).or_else(|| extract_query_token(&req));

    let token = match token {
        Some(t) => t,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    let validation = Validation::new(Algorithm::HS256);
    let key = DecodingKey::from_secret(&state.jwt_secret);

    let data =
        decode::<JwtClaims>(&token, &key, &validation).map_err(|_| StatusCode::UNAUTHORIZED)?;

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
