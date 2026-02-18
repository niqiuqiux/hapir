use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::agent::loop_base::LoopResult;
use crate::modules::claude::permission_handler::PermissionHandler;
use crate::modules::claude::sdk::query;
use crate::modules::claude::sdk::types::{QueryOptions, SdkMessage};
use crate::modules::claude::session::ClaudeSession;

use super::run::EnhancedMode;

/// Remote launcher for Claude.
///
/// Spawns the `claude` CLI process in remote/SDK mode (--print),
/// manages permission handling, message conversion, and the
/// remote launch lifecycle loop.
pub async fn claude_remote_launcher(
    session: &Arc<ClaudeSession<EnhancedMode>>,
) -> LoopResult {
    let working_directory = session.base.path.clone();
    debug!("[claudeRemoteLauncher] Starting in {}", working_directory);

    // Create PermissionHandler
    let permission_handler = Arc::new(Mutex::new(PermissionHandler::new()));

    // Main message processing loop
    loop {
        // Wait for a message from the queue
        info!("[claudeRemoteLauncher] Waiting for messages from queue...");
        let batch = match session.base.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[claudeRemoteLauncher] Queue closed, exiting");
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;
        let mode = batch.mode;
        let is_isolate = batch.isolate;

        debug!(
            "[claudeRemoteLauncher] Processing message (isolate={}): {}",
            is_isolate,
            if prompt.len() > 100 {
                format!("{}...", &prompt[..prompt.floor_char_boundary(100)])
            } else {
                prompt.clone()
            }
        );

        // Reset permission handler state for isolate messages (e.g., /clear)
        if is_isolate {
            permission_handler.lock().await.reset();
        }

        // Build query options from the current enhanced mode
        let session_id = session.base.session_id.lock().await.clone();
        let query_options = QueryOptions {
            cwd: Some(working_directory.clone()),
            model: mode.model.clone(),
            fallback_model: mode.fallback_model.clone(),
            custom_system_prompt: mode.custom_system_prompt.clone(),
            append_system_prompt: mode.append_system_prompt.clone(),
            permission_mode: mode.permission_mode.clone(),
            allowed_tools: mode.allowed_tools.clone(),
            disallowed_tools: mode.disallowed_tools.clone(),
            resume: session_id,
            continue_conversation: true,
            mcp_servers: if session.mcp_servers.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&session.mcp_servers).unwrap_or_default())
            },
            settings_path: Some(session.hook_settings_path.clone()),
            ..Default::default()
        };

        // Spawn the Claude SDK process
        info!("[claudeRemoteLauncher] Spawning claude SDK for prompt: {}", if prompt.len() > 80 { format!("{}...", &prompt[..prompt.floor_char_boundary(80)]) } else { prompt.clone() });
        let mut query_handle = match query::query(&prompt, query_options) {
            Ok(q) => q,
            Err(e) => {
                warn!("[claudeRemoteLauncher] Failed to spawn claude: {}", e);
                session
                    .base
                    .ws_client
                    .send_message(serde_json::json!({
                        "role": "assistant",
                        "content": {
                            "type": "output",
                            "data": {
                                "type": "assistant",
                                "message": {
                                    "role": "assistant",
                                    "content": [{
                                        "type": "text",
                                        "text": format!("Failed to spawn claude SDK: {}", e),
                                    }]
                                }
                            }
                        }
                    }))
                    .await;
                // Continue the loop to retry on next message
                continue;
            }
        };

        // Notify thinking state
        session.base.on_thinking_change(true).await;

        // Process SDK messages from the Claude process
        let should_switch = false;
        while let Some(msg) = query_handle.next_message().await {
            match msg {
                SdkMessage::System {
                    subtype,
                    session_id,
                    tools,
                    slash_commands,
                    ..
                } => {
                    debug!(
                        "[claudeRemoteLauncher] System message: subtype={}",
                        subtype
                    );
                    // If we got a session ID, notify the session base
                    if let Some(ref sid) = session_id {
                        session.base.on_session_found(sid).await;
                    }
                    // Update metadata with tools and slash commands if present
                    if tools.is_some() || slash_commands.is_some() {
                        let tools_clone = tools.clone();
                        let slash_clone = slash_commands.clone();
                        let _ = session
                            .base
                            .ws_client
                            .update_metadata(move |mut metadata| {
                                if let Some(t) = tools_clone {
                                    metadata.tools = Some(t);
                                }
                                if let Some(sc) = slash_clone {
                                    metadata.slash_commands = Some(sc);
                                }
                                metadata
                            })
                            .await;
                    }
                }
                SdkMessage::Assistant {
                    message,
                    parent_tool_use_id,
                } => {
                    debug!(
                        "[claudeRemoteLauncher] Assistant message: role={}, content_blocks={}, parent={:?}",
                        message.role,
                        message.content.len(),
                        parent_tool_use_id
                    );
                    session
                        .base
                        .ws_client
                        .send_message(serde_json::json!({
                            "role": message.role,
                            "content": {
                                "type": "output",
                                "data": {
                                    "type": "assistant",
                                    "parentUuid": parent_tool_use_id,
                                    "message": {
                                        "role": message.role,
                                        "content": message.content,
                                    }
                                }
                            }
                        }))
                        .await;
                }
                SdkMessage::User {
                    message,
                    parent_tool_use_id,
                } => {
                    debug!(
                        "[claudeRemoteLauncher] User message: role={}, parent={:?}",
                        message.role, parent_tool_use_id
                    );
                    session
                        .base
                        .ws_client
                        .send_message(serde_json::json!({
                            "role": message.role,
                            "content": {
                                "type": "output",
                                "data": {
                                    "type": "user",
                                    "parentUuid": parent_tool_use_id,
                                    "message": {
                                        "role": message.role,
                                        "content": message.content,
                                    }
                                }
                            }
                        }))
                        .await;
                }

                SdkMessage::Result {
                    subtype,
                    result,
                    num_turns,
                    total_cost_usd,
                    duration_ms,
                    is_error,
                    session_id: result_session_id,
                    ..
                } => {
                    info!(
                        "[claudeRemoteLauncher] Result: subtype={}, turns={}, cost={}, duration={}ms, error={}",
                        subtype, num_turns, total_cost_usd, duration_ms, is_error
                    );
                    // If the result is an error, send it as an assistant message so the user sees it
                    if is_error {
                        if let Some(ref error_text) = result {
                            session
                                .base
                                .ws_client
                                .send_message(serde_json::json!({
                                    "role": "assistant",
                                    "content": {
                                        "type": "output",
                                        "data": {
                                            "type": "assistant",
                                            "message": {
                                                "role": "assistant",
                                                "content": [{
                                                    "type": "text",
                                                    "text": error_text,
                                                }]
                                            }
                                        }
                                    }
                                }))
                                .await;
                        }
                    }
                    // Store session ID from result
                    if !result_session_id.is_empty() {
                        session.base.on_session_found(&result_session_id).await;
                    }
                }
                SdkMessage::ControlRequest {
                    request_id,
                    request,
                } => {
                    debug!(
                        "[claudeRemoteLauncher] Permission request: id={}, subtype={}, tool={}",
                        request_id, request.subtype, request.tool_name.as_deref().unwrap_or("none")
                    );
                    // TODO: Forward permission requests to frontend for interactive approval.
                    // For now, just log them — they are not chat messages.
                }
                SdkMessage::ControlResponse { response } => {
                    debug!(
                        "[claudeRemoteLauncher] Control response: id={}",
                        response.request_id
                    );
                }
                SdkMessage::ControlCancelRequest { request_id } => {
                    debug!(
                        "[claudeRemoteLauncher] Control cancel: id={}",
                        request_id
                    );
                }
                SdkMessage::Log { log } => {
                    debug!(
                        "[claudeRemoteLauncher] SDK log [{}]: {}",
                        log.level, log.message
                    );
                }
            }
        }

        // Process exited - update thinking state
        session.base.on_thinking_change(false).await;
        session.consume_one_time_flags().await;

        debug!(
            "[claudeRemoteLauncher] Claude process finished (switch={})",
            should_switch
        );

        if should_switch {
            return LoopResult::Switch;
        }

        // Check if queue is closed (session ending)
        if session.base.queue.is_closed().await {
            return LoopResult::Exit;
        }

        // Continue the loop to process the next message
    }
}
