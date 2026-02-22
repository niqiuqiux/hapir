use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::agent::loop_base::LoopResult;
use crate::modules::claude::permission_handler::PermissionHandler;
use crate::modules::claude::sdk::query;
use crate::modules::claude::sdk::types::{PermissionResult, QueryOptions, SdkMessage};
use crate::modules::claude::session::ClaudeSession;

use super::run::EnhancedMode;

/// A tool_use block tracked from assistant messages, used to resolve
/// the tool_use.id when a control_request arrives.
struct TrackedToolCall {
    id: String,
    name: String,
    input: Value,
    used: bool,
}

/// Resolve the tool_use.id for a permission request by matching tool name and input
/// against tracked tool calls (most recent first).
fn resolve_tool_call_id(
    tracked: &mut Vec<TrackedToolCall>,
    tool_name: &str,
    input: &Option<Value>,
) -> Option<String> {
    let input_val = input.as_ref().cloned().unwrap_or(Value::Null);
    for call in tracked.iter_mut().rev() {
        if !call.used && call.name == tool_name && call.input == input_val {
            call.used = true;
            return Some(call.id.clone());
        }
    }
    None
}

/// Remote launcher for Claude.
///
/// Spawns the `claude` CLI process in interactive mode (--input-format stream-json),
/// enabling bidirectional communication for streaming and permission handling.
pub async fn claude_remote_launcher(session: &Arc<ClaudeSession<EnhancedMode>>) -> LoopResult {
    let working_directory = session.base.path.clone();
    debug!("[claudeRemoteLauncher] Starting in {}", working_directory);

    let permission_handler = Arc::new(Mutex::new(PermissionHandler::new()));

    // Streaming state for typewriter effect
    let mut streaming_message_id: Option<String> = None;
    let mut accumulated_text = String::new();

    // Track the last assistant message per turn so we can send a final
    // complete message (with thinking blocks + usage) when Result arrives.
    let mut last_assistant_content: Option<Vec<Value>> = None;
    let mut last_assistant_usage: Option<Value> = None;

    // Track tool_use blocks from assistant messages so we can resolve
    // the tool_use.id when a control_request arrives (the SDK's request_id
    // is different from the tool_use.id).
    let mut tracked_tool_calls: Vec<TrackedToolCall> = Vec::new();
    // Map tool_use.id → SDK request_id (needed to send control_response)
    let mut tool_use_to_request_id: HashMap<String, String> = HashMap::new();

    // The interactive query handle (lazily spawned on first message)
    let mut query_handle: Option<query::InteractiveQuery> = None;
    let mut current_mode_hash: Option<String> = None;

    loop {
        info!("[claudeRemoteLauncher] Waiting for messages from queue...");
        let batch = match session.base.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[claudeRemoteLauncher] Queue closed, exiting");
                if let Some(ref qh) = query_handle {
                    qh.close_stdin().await;
                }
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;
        let mode = batch.mode;
        let is_isolate = batch.isolate;

        // If the mode hash changed (e.g. model or permission mode switch),
        // kill the existing process so it gets re-spawned with new parameters.
        let mode_changed = current_mode_hash
            .as_ref()
            .is_some_and(|h| *h != batch.hash);
        if mode_changed && query_handle.is_some() {
            info!(
                "[claudeRemoteLauncher] Mode changed ({} -> {}), restarting process",
                current_mode_hash.as_deref().unwrap_or("?"),
                batch.hash
            );
            if let Some(ref qh) = query_handle {
                qh.kill().await;
            }
            query_handle = None;
            session.active_pid.store(0, Ordering::Relaxed);
            streaming_message_id = None;
            accumulated_text.clear();
            tracked_tool_calls.clear();
            tool_use_to_request_id.clear();
        }
        current_mode_hash = Some(batch.hash.clone());

        debug!(
            "[claudeRemoteLauncher] Processing message (isolate={}): {}",
            is_isolate,
            if prompt.len() > 100 {
                format!("{}...", &prompt[..prompt.floor_char_boundary(100)])
            } else {
                prompt.clone()
            }
        );

        // Reset state for isolate messages (e.g., /clear)
        if is_isolate {
            permission_handler.lock().await.reset();
            streaming_message_id = None;
            accumulated_text.clear();
            tracked_tool_calls.clear();
            tool_use_to_request_id.clear();
            // Close existing process and force re-spawn
            if let Some(ref qh) = query_handle {
                qh.close_stdin().await;
            }
            query_handle = None;
            session.active_pid.store(0, Ordering::Relaxed);
        }

        // Spawn or reuse the interactive Claude process
        let needs_spawn = query_handle.is_none();
        if needs_spawn {
            let mut resume_id = session.base.session_id.lock().await.clone();

            // If no session_id yet, check claude_args for --resume token
            // (passed by runner when resuming an inactive session)
            if resume_id.is_none() {
                if let Some(ref args) = *session.claude_args.lock().await {
                    if let Some(pos) = args.iter().position(|a| a == "--resume") {
                        resume_id = args.get(pos + 1).cloned();
                    }
                }
            }

            info!(
                "[claudeRemoteLauncher] Spawning: resume_id={:?}, mode_changed={}",
                resume_id, mode_changed
            );
            let query_options = QueryOptions {
                cwd: Some(working_directory.clone()),
                model: mode.model.clone(),
                fallback_model: mode.fallback_model.clone(),
                custom_system_prompt: mode.custom_system_prompt.clone(),
                append_system_prompt: mode.append_system_prompt.clone(),
                permission_mode: mode.permission_mode.clone(),
                allowed_tools: mode.allowed_tools.clone(),
                disallowed_tools: mode.disallowed_tools.clone(),
                continue_conversation: false,
                resume: resume_id,
                mcp_servers: if session.mcp_servers.is_empty() {
                    None
                } else {
                    Some(serde_json::to_value(&session.mcp_servers).unwrap_or_default())
                },
                settings_path: Some(session.hook_settings_path.clone()),
                ..Default::default()
            };

            info!(
                "[claudeRemoteLauncher] Spawning interactive claude SDK: resume={:?}, model={:?}",
                query_options.resume, query_options.model
            );
            match query::query_interactive(query_options) {
                Ok(qh) => {
                    session
                        .active_pid
                        .store(qh.pid().unwrap_or(0), Ordering::Relaxed);
                    query_handle = Some(qh);
                }
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
                    continue;
                }
            }
        }

        let qh = query_handle.as_ref().unwrap();

        // Send user message via stdin
        if let Err(e) = qh.send_user_message(&prompt).await {
            warn!("[claudeRemoteLauncher] Failed to send message: {}", e);
            query_handle = None;
            session.active_pid.store(0, Ordering::Relaxed);
            continue;
        }

        session.base.on_thinking_change(true).await;

        // Process SDK messages until we get a Result (turn complete)
        let mut got_result = false;
        let should_switch = false;

        while let Some(qh) = query_handle.as_mut() {
            let msg = match qh.next_message().await {
                Some(m) => m,
                None => {
                    debug!("[claudeRemoteLauncher] Process exited");
                    query_handle = None;
                    session.active_pid.store(0, Ordering::Relaxed);
                    break;
                }
            };

            match msg {
                SdkMessage::System {
                    subtype,
                    session_id,
                    tools,
                    slash_commands,
                    ..
                } => {
                    debug!("[claudeRemoteLauncher] System: subtype={}", subtype);
                    if let Some(ref sid) = session_id {
                        session.base.on_session_found(sid).await;
                    }
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
                        "[claudeRemoteLauncher] Assistant: blocks={}, parent={:?}",
                        message.content.len(),
                        parent_tool_use_id
                    );

                    // Always track latest content + usage for the final message
                    last_assistant_content = Some(message.content.clone());
                    if message.usage.is_some() {
                        last_assistant_usage = message.usage.clone();
                    }

                    // Track tool_use blocks for permission ID resolution
                    for block in &message.content {
                        if block.get("type").and_then(|v| v.as_str()) == Some("tool_use")
                            && let Some(id) = block.get("id").and_then(|v| v.as_str())
                        {
                            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            // Only add if not already tracked
                            if !tracked_tool_calls.iter().any(|tc| tc.id == id) {
                                tracked_tool_calls.push(TrackedToolCall {
                                    id: id.to_string(),
                                    name: name.to_string(),
                                    input,
                                    used: false,
                                });
                            }
                        }
                    }

                    // Extract text for streaming
                    let mut current_text = String::new();
                    for block in &message.content {
                        if let Some(block_type) = block.get("type").and_then(|v| v.as_str())
                            && block_type == "text"
                            && let Some(text) = block.get("text").and_then(|v| v.as_str())
                        {
                            current_text.push_str(text);
                        }
                    }

                    let is_text_only = message
                        .content
                        .iter()
                        .all(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"));

                    if is_text_only && !current_text.is_empty() {
                        if streaming_message_id.is_none() {
                            streaming_message_id = Some(uuid::Uuid::new_v4().to_string());
                            accumulated_text.clear();
                        }

                        let mid = streaming_message_id.as_ref().unwrap();
                        let delta_text = if current_text.starts_with(&accumulated_text) {
                            &current_text[accumulated_text.len()..]
                        } else {
                            &current_text
                        };

                        if !delta_text.is_empty() {
                            session
                                .base
                                .ws_client
                                .send_message_delta(mid, delta_text, false)
                                .await;
                            accumulated_text = current_text.clone();
                        }
                    } else {
                        // Non-text content - finalize stream and send complete message
                        if let Some(mid) = streaming_message_id.take() {
                            session
                                .base
                                .ws_client
                                .send_message_delta(&mid, &accumulated_text, true)
                                .await;
                            accumulated_text.clear();
                        }

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
                }
                SdkMessage::User {
                    message,
                    parent_tool_use_id,
                } => {
                    debug!(
                        "[claudeRemoteLauncher] User: parent={:?}",
                        parent_tool_use_id
                    );
                    // Use "assistant" as outer role so the frontend routes through
                    // normalizeAgentRecord → normalizeUserOutput, which handles
                    // tool_result content blocks. The inner data.type="user" preserves
                    // the original message semantics.
                    session
                        .base
                        .ws_client
                        .send_message(serde_json::json!({
                            "role": "assistant",
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
                    usage: result_usage,
                    ..
                } => {
                    info!(
                        "[claudeRemoteLauncher] Result: subtype={}, turns={}, cost={}, duration={}ms, error={}",
                        subtype, num_turns, total_cost_usd, duration_ms, is_error
                    );

                    // Finalize any ongoing stream
                    if let Some(mid) = streaming_message_id.take() {
                        session
                            .base
                            .ws_client
                            .send_message_delta(&mid, &accumulated_text, true)
                            .await;
                        accumulated_text.clear();
                    }

                    // Prefer Result usage (has cache_* fields), fall back to last assistant usage
                    let final_usage = result_usage
                        .and_then(|u| serde_json::to_value(u).ok())
                        .or(last_assistant_usage.take());

                    // Send the final complete message with full content + usage.
                    // This includes thinking blocks that streaming deltas skipped.
                    if let Some(content) = last_assistant_content.take() {
                        let has_content = content
                            .iter()
                            .any(|b| b.get("type").and_then(|v| v.as_str()) != Some("thinking"));

                        if has_content || !is_error {
                            let mut msg_body = serde_json::json!({
                                "role": "assistant",
                                "content": content,
                            });
                            if let Some(usage) = &final_usage {
                                msg_body["usage"] = usage.clone();
                            }

                            session
                                .base
                                .ws_client
                                .send_message(serde_json::json!({
                                    "role": "assistant",
                                    "content": {
                                        "type": "output",
                                        "data": {
                                            "type": "assistant",
                                            "message": msg_body
                                        }
                                    }
                                }))
                                .await;
                        }
                    }

                    if is_error && let Some(ref error_text) = result {
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

                    if !result_session_id.is_empty() {
                        session.base.on_session_found(&result_session_id).await;
                    }

                    // Clean up turn state
                    tracked_tool_calls.clear();
                    tool_use_to_request_id.clear();
                    last_assistant_content = None;
                    last_assistant_usage = None;

                    // Clear completedRequests from agent state so stale entries
                    // don't create phantom tool cards after page refresh.
                    let _ = session
                        .base
                        .ws_client
                        .update_agent_state(|mut state| {
                            if let Some(obj) = state.as_object_mut() {
                                obj.remove("completedRequests");
                            }
                            state
                        })
                        .await;

                    // Turn complete - break to wait for next user message
                    got_result = true;
                    break;
                }
                SdkMessage::ControlRequest {
                    request_id,
                    request,
                } => {
                    let tool_name = request.tool_name.clone().unwrap_or_default();
                    let tool_input = request.input.clone();

                    // Resolve the tool_use.id from tracked assistant messages.
                    // The frontend matches permissions to tool cards by tool_use.id,
                    // not by the SDK's request_id.
                    let permission_key =
                        resolve_tool_call_id(&mut tracked_tool_calls, &tool_name, &tool_input)
                            .unwrap_or_else(|| request_id.clone());

                    info!(
                        "[claudeRemoteLauncher] Permission request: sdk_id={}, resolved_key={}, tool={}",
                        request_id, permission_key, tool_name
                    );

                    // Store mapping so we can send control_response with the SDK's request_id
                    tool_use_to_request_id.insert(permission_key.clone(), request_id.clone());

                    // Create oneshot channel for the response
                    let (tx, rx) =
                        tokio::sync::oneshot::channel::<(bool, Option<serde_json::Value>)>();
                    session
                        .pending_permissions
                        .lock()
                        .await
                        .insert(permission_key.clone(), tx);

                    // Update agent state with the permission request (keyed by tool_use.id)
                    let key_for_state = permission_key.clone();
                    let tool_name_for_state = tool_name.clone();
                    let tool_input_for_state = tool_input.clone();
                    let requested_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as f64;

                    let _ = session
                        .base
                        .ws_client
                        .update_agent_state(move |mut state| {
                            if let Some(obj) = state.as_object_mut() {
                                let requests = obj
                                    .entry("requests")
                                    .or_insert_with(|| serde_json::json!({}));
                                if let Some(req_map) = requests.as_object_mut() {
                                    req_map.insert(
                                        key_for_state,
                                        serde_json::json!({
                                            "tool": tool_name_for_state,
                                            "arguments": tool_input_for_state,
                                            "createdAt": requested_at,
                                        }),
                                    );
                                }
                            }
                            state
                        })
                        .await;

                    // Wait for the permission response from the web UI
                    let sdk_rid = request_id;
                    match rx.await {
                        Ok((approved, updated_input)) => {
                            let qh_ref = query_handle.as_ref().unwrap();
                            if approved {
                                let input = updated_input
                                    .or(tool_input)
                                    .unwrap_or(serde_json::json!({}));
                                if let Err(e) = qh_ref
                                    .send_control_response(
                                        &sdk_rid,
                                        PermissionResult::Allow {
                                            updated_input: input,
                                        },
                                    )
                                    .await
                                {
                                    warn!(
                                        "[claudeRemoteLauncher] Failed to send permission response: {}",
                                        e
                                    );
                                }
                            } else {
                                if let Err(e) = qh_ref
                                    .send_control_error(&sdk_rid, "Permission denied by user")
                                    .await
                                {
                                    warn!(
                                        "[claudeRemoteLauncher] Failed to send permission denial: {}",
                                        e
                                    );
                                }
                            }
                        }
                        Err(_) => {
                            // Channel dropped (session closing), deny
                            if let Some(qh_ref) = query_handle.as_ref() {
                                let _ =
                                    qh_ref.send_control_error(&sdk_rid, "Session closing").await;
                            }
                        }
                    }
                }
                SdkMessage::ControlResponse { response } => {
                    debug!(
                        "[claudeRemoteLauncher] Control response: id={}",
                        response.request_id
                    );
                }
                SdkMessage::ControlCancelRequest { request_id } => {
                    debug!("[claudeRemoteLauncher] Control cancel: id={}", request_id);
                    // The cancel may come with the SDK's request_id; find the
                    // permission_key (tool_use.id) we mapped it to.
                    let permission_key = tool_use_to_request_id
                        .iter()
                        .find(|(_, v)| **v == request_id)
                        .map(|(k, _)| k.clone())
                        .unwrap_or_else(|| request_id.clone());

                    session
                        .pending_permissions
                        .lock()
                        .await
                        .remove(&permission_key);
                    tool_use_to_request_id.remove(&permission_key);

                    // Remove from agent state
                    let _ = session
                        .base
                        .ws_client
                        .update_agent_state(move |mut state| {
                            if let Some(requests) =
                                state.get_mut("requests").and_then(|v| v.as_object_mut())
                            {
                                requests.remove(&permission_key);
                            }
                            state
                        })
                        .await;
                }
                SdkMessage::Log { log } => {
                    debug!(
                        "[claudeRemoteLauncher] SDK log [{}]: {}",
                        log.level, log.message
                    );
                }
            }
        }

        session.base.on_thinking_change(false).await;
        // Only consume --resume after we've successfully received the session ID
        // from the Claude process. If the process was killed before sending the
        // System message, session_id is still None and we need --resume for re-spawn.
        if session.base.session_id.lock().await.is_some() {
            session.consume_one_time_flags().await;
        }

        if should_switch {
            if let Some(ref qh) = query_handle {
                qh.close_stdin().await;
            }
            return LoopResult::Switch;
        }

        // If process died (no result), clear handle so next message re-spawns
        if !got_result {
            query_handle = None;
            session.active_pid.store(0, Ordering::Relaxed);
        }

        if session.base.queue.is_closed().await {
            if let Some(ref qh) = query_handle {
                qh.close_stdin().await;
            }
            return LoopResult::Exit;
        }
    }
}
