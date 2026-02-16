pub mod auth;
pub mod bind;
pub mod cli;
pub mod events;
pub mod git;
pub mod machines;
pub mod messages;
pub mod permissions;
pub mod push;
pub mod sessions;
pub mod voice;

use axum::Router;
use crate::web::AppState;

/// Build the /api router (JWT auth middleware applied externally).
pub fn api_router() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(bind::router())
        .merge(sessions::router())
        .merge(messages::router())
        .merge(permissions::router())
        .merge(machines::router())
        .merge(events::router())
        .merge(git::router())
        .merge(push::router())
        .merge(voice::router())
}
