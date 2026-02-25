use std::sync::Arc;

use tokio::select;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use hapir_shared::session::{AgentContent, FlatMessage, RoleWrappedMessage};

use crate::agent::loop_base::LoopResult;
use crate::agent::session_base::AgentSessionBase;
use hapir_acp::codex_app_server::backend::CodexAppServerBackend;
use hapir_acp::types::{AgentBackend, AgentMessage, AgentSessionConfig, PromptContent};
use hapir_infra::ws::session_client::WsSessionClient;

use super::CodexMode;

fn codex_role_wrapped(data: serde_json::Value) -> RoleWrappedMessage {
    RoleWrappedMessage {
        role: "assistant".into(),
        content: AgentContent::Codex { data },
        meta: None,
    }
}

async fn forward_agent_message(ws: &WsSessionClient, msg: AgentMessage) {
    match msg {
        AgentMessage::Text { text } => {
            ws.send_typed_message(&codex_role_wrapped(serde_json::json!({
                "type": "message",
                "message": text,
            })))
            .await;
        }
        AgentMessage::TextDelta {
            message_id,
            text,
            is_final,
        } => {
            ws.send_message_delta(&message_id, &text, is_final).await;
        }
        AgentMessage::ToolCall {
            id,
            name,
            input,
            status,
        } => {
            ws.send_typed_message(&codex_role_wrapped(serde_json::json!({
                "type": "tool-call",
                "callId": id,
                "name": name,
                "input": input,
                "status": status,
            })))
            .await;
        }
        AgentMessage::ToolResult { id, output, status } => {
            ws.send_typed_message(&codex_role_wrapped(serde_json::json!({
                "type": "tool-call-result",
                "callId": id,
                "output": output,
                "status": status,
            })))
            .await;
        }
        AgentMessage::Plan { items } => {
            ws.send_typed_message(&codex_role_wrapped(serde_json::json!({
                "type": "plan",
                "entries": items,
            })))
            .await;
        }
        AgentMessage::Error { message } => {
            ws.send_typed_message(&codex_role_wrapped(serde_json::json!({
                "type": "message",
                "message": message,
            })))
            .await;
        }
        AgentMessage::TurnComplete { .. } | AgentMessage::ThinkingStatus { .. } => {}
    }
}

pub async fn codex_remote_launcher(
    session: &Arc<AgentSessionBase<CodexMode>>,
    backend: &Arc<CodexAppServerBackend>,
    resume_thread_id: Option<&str>,
    pending_attachments: Arc<Mutex<Vec<String>>>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[codexRemoteLauncher] Starting in {}", working_directory);

    if let Err(e) = backend.initialize().await {
        warn!(
            "[codexRemoteLauncher] Failed to initialize Codex App Server: {}",
            e
        );
        session
            .ws_client
            .send_typed_message(&FlatMessage::Error {
                message: format!("Failed to initialize codex app-server: {}", e),
                exit_reason: None,
            })
            .await;
        return LoopResult::Exit;
    }

    let resume_id = match resume_thread_id {
        Some(id) => Some(id.to_string()),
        None => session
            .ws_client
            .metadata()
            .await
            .and_then(|m| m.codex_session_id),
    };

    let thread_id = match resume_id {
        Some(ref prev_id) => {
            debug!(
                "[codexRemoteLauncher] Attempting to resume thread: {}",
                prev_id
            );
            match backend.resume_session(prev_id).await {
                Ok(tid) => {
                    debug!("[codexRemoteLauncher] Thread resumed: {}", tid);
                    session.on_session_found(&tid).await;
                    tid
                }
                Err(e) => {
                    warn!(
                        "[codexRemoteLauncher] Failed to resume thread {}: {}, starting new",
                        prev_id, e
                    );
                    match backend
                        .new_session(AgentSessionConfig {
                            cwd: working_directory.clone(),
                            mcp_servers: vec![],
                        })
                        .await
                    {
                        Ok(tid) => {
                            session.on_session_found(&tid).await;
                            tid
                        }
                        Err(e2) => {
                            warn!("[codexRemoteLauncher] Failed to start thread: {}", e2);
                            session
                                .ws_client
                                .send_typed_message(&FlatMessage::Error {
                                    message: format!("Failed to start codex thread: {}", e2),
                                    exit_reason: None,
                                })
                                .await;
                            return LoopResult::Exit;
                        }
                    }
                }
            }
        }
        None => {
            match backend
                .new_session(AgentSessionConfig {
                    cwd: working_directory.clone(),
                    mcp_servers: vec![],
                })
                .await
            {
                Ok(tid) => {
                    session.on_session_found(&tid).await;
                    tid
                }
                Err(e) => {
                    warn!("[codexRemoteLauncher] Failed to start thread: {}", e);
                    session
                        .ws_client
                        .send_typed_message(&FlatMessage::Error {
                            message: format!("Failed to start codex thread: {}", e),
                            exit_reason: None,
                        })
                        .await;
                    return LoopResult::Exit;
                }
            }
        }
    };

    loop {
        let batch = select! {
            batch = session.queue.wait_for_messages() => {
                match batch {
                    Some(b) => b,
                    None => {
                        debug!("[codexRemoteLauncher] Queue closed, exiting");
                        return LoopResult::Exit;
                    }
                }
            }
            _ = session.switch_notify.notified() => {
                info!("[codexRemoteLauncher] Switch requested, returning to local");
                return LoopResult::Switch;
            }
        };

        let prompt = batch.message;
        debug!(
            "[codexRemoteLauncher] Processing message: {}",
            if prompt.len() > 100 {
                format!("{}...", &prompt[..prompt.floor_char_boundary(100)])
            } else {
                prompt.clone()
            }
        );

        session.on_thinking_change(true).await;

        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<AgentMessage>();
        let ws_for_consumer = session.ws_client.clone();
        let ts_for_consumer = session.thinking_status.clone();
        let consumer = tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                if let AgentMessage::ThinkingStatus { ref status } = msg {
                    *ts_for_consumer.lock().await = status.clone();
                    continue;
                }
                forward_agent_message(&ws_for_consumer, msg).await;
            }
        });

        let on_update: Box<dyn Fn(AgentMessage) + Send + Sync> = Box::new(move |msg| {
            let _ = msg_tx.send(msg);
        });

        let mut content = vec![PromptContent::Text { text: prompt }];
        let image_paths: Vec<String> = pending_attachments.lock().await.drain(..).collect();
        for path in image_paths {
            content.push(PromptContent::LocalImage { path });
        }
        if let Err(e) = backend.prompt(&thread_id, content, on_update).await {
            warn!("[codexRemoteLauncher] Prompt error: {}", e);
            session
                .ws_client
                .send_typed_message(&FlatMessage::Error {
                    message: format!("Codex error: {}", e),
                    exit_reason: None,
                })
                .await;
        }

        let _ = consumer.await;
        session.on_thinking_change(false).await;

        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}
