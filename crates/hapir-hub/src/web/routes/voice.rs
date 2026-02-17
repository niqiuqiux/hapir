use axum::{http::StatusCode, routing::post, Router};

use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/voice/token", post(voice_token))
}

async fn voice_token() -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}
