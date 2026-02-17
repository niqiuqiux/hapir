use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::web::AppState;

/// CLI auth context stored in request extensions.
#[derive(Debug, Clone)]
pub struct CliAuthContext {
    pub namespace: String,
}

/// Parse access token format: "baseToken:namespace" or just "baseToken" (default namespace).
fn parse_access_token(raw: &str) -> Option<(String, String)> {
    if raw.is_empty() {
        return None;
    }
    if let Some(pos) = raw.rfind(':') {
        let base = &raw[..pos];
        let ns = &raw[pos + 1..];
        if !base.is_empty() && !ns.is_empty() {
            return Some((base.to_string(), ns.to_string()));
        }
    }
    Some((raw.to_string(), "default".to_string()))
}

/// CLI auth middleware. Validates bearer token with constant-time comparison.
pub async fn cli_auth(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let (base_token, namespace) =
        parse_access_token(token).ok_or(StatusCode::UNAUTHORIZED)?;

    let expected = state.cli_api_token.as_bytes();
    let provided = base_token.as_bytes();
    if expected.len() != provided.len() || expected.ct_eq(provided).unwrap_u8() != 1 {
        return Err(StatusCode::UNAUTHORIZED);
    }

    req.extensions_mut().insert(CliAuthContext { namespace });
    Ok(next.run(req).await)
}
