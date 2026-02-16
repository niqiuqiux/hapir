use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::error;

use hapi_shared::schemas::Session;

use crate::sync::SyncEngine;

use super::api::{InlineKeyboardMarkup, TelegramApi};
use super::renderer::{find_session_by_prefix, parse_callback_data};

pub const APPROVE_ACTION: &str = "ap";
pub const DENY_ACTION: &str = "dn";

/// Handle a Telegram callback query from an inline keyboard button press.
pub async fn handle_callback(
    data: &str,
    sync_engine: &Arc<Mutex<SyncEngine>>,
    namespace: &str,
    api: &TelegramApi,
    chat_id: i64,
    message_id: i64,
    callback_query_id: &str,
) {
    let (action, session_prefix, extra) = parse_callback_data(data);

    let result: anyhow::Result<()> = async {
        match action.as_str() {
            APPROVE_ACTION => {
                let session = get_session_or_answer(
                    sync_engine, namespace, &session_prefix, api, callback_query_id, true,
                )
                .await?;
                let Some(session) = session else {
                    return Ok(());
                };

                let request_id =
                    find_request_by_prefix(&session, extra.as_deref().unwrap_or(""));
                let Some(request_id) = request_id else {
                    api.answer_callback_query(
                        callback_query_id,
                        Some("Request not found or already processed"),
                    )
                    .await?;
                    return Ok(());
                };

                sync_engine
                    .lock()
                    .await
                    .approve_permission(&session.id, &request_id, None, None, None, None)
                    .await?;
                api.answer_callback_query(callback_query_id, Some("Approved!"))
                    .await?;
                let empty_keyboard = InlineKeyboardMarkup {
                    inline_keyboard: vec![],
                };
                api.edit_message_text(
                    chat_id,
                    message_id,
                    "Permission approved.",
                    Some(&empty_keyboard),
                )
                .await?;
            }

            DENY_ACTION => {
                let session = get_session_or_answer(
                    sync_engine, namespace, &session_prefix, api, callback_query_id, true,
                )
                .await?;
                let Some(session) = session else {
                    return Ok(());
                };

                let request_id =
                    find_request_by_prefix(&session, extra.as_deref().unwrap_or(""));
                let Some(request_id) = request_id else {
                    api.answer_callback_query(
                        callback_query_id,
                        Some("Request not found or already processed"),
                    )
                    .await?;
                    return Ok(());
                };

                sync_engine
                    .lock()
                    .await
                    .deny_permission(&session.id, &request_id, None)
                    .await?;
                api.answer_callback_query(callback_query_id, Some("Denied"))
                    .await?;
                let empty_keyboard = InlineKeyboardMarkup {
                    inline_keyboard: vec![],
                };
                api.edit_message_text(
                    chat_id,
                    message_id,
                    "Permission denied.",
                    Some(&empty_keyboard),
                )
                .await?;
            }

            _ => {
                api.answer_callback_query(callback_query_id, Some("Unknown action"))
                    .await?;
            }
        }
        Ok(())
    }
    .await;

    if let Err(e) = result {
        error!(error = %e, "callback handling error");
        let _ = api
            .answer_callback_query(callback_query_id, Some("An error occurred"))
            .await;
    }
}

async fn get_session_or_answer(
    sync_engine: &Arc<Mutex<SyncEngine>>,
    namespace: &str,
    session_prefix: &str,
    api: &TelegramApi,
    callback_query_id: &str,
    require_active: bool,
) -> anyhow::Result<Option<Session>> {
    let sessions = sync_engine
        .lock()
        .await
        .get_sessions_by_namespace(namespace);
    let session = find_session_by_prefix(&sessions, session_prefix).cloned();

    match session {
        None => {
            api.answer_callback_query(callback_query_id, Some("Session not found"))
                .await?;
            Ok(None)
        }
        Some(s) if require_active && !s.active => {
            api.answer_callback_query(callback_query_id, Some("Session is inactive"))
                .await?;
            Ok(None)
        }
        Some(s) => Ok(Some(s)),
    }
}

fn find_request_by_prefix(session: &Session, prefix: &str) -> Option<String> {
    let requests = session
        .agent_state
        .as_ref()
        .and_then(|s| s.requests.as_ref())?;

    // Try prefix match first
    for req_id in requests.keys() {
        if req_id.starts_with(prefix) {
            return Some(req_id.clone());
        }
    }

    // Fallback to first request
    requests.keys().next().cloned()
}
