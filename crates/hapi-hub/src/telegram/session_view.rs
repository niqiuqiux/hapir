use hapi_shared::schemas::Session;
use serde_json::Value;

use super::api::{InlineKeyboardButton, InlineKeyboardMarkup, WebAppInfo};
use super::callbacks::{APPROVE_ACTION, DENY_ACTION};
use super::renderer::{build_mini_app_deep_link, create_callback_data, get_session_name, truncate};

const MAX_TOOL_ARGS_LENGTH: usize = 150;

/// Format a compact session notification for permission requests.
pub fn format_session_notification(session: &Session) -> String {
    let name = get_session_name(session);
    let mut lines = vec![
        "Permission Request".to_string(),
        String::new(),
        format!("Session: {name}"),
    ];

    if let Some(ref state) = session.agent_state {
        if let Some(ref requests) = state.requests {
            if let Some((_, req)) = requests.iter().next() {
                lines.push(format!("Tool: {}", req.tool));
                let args = format_tool_arguments_detailed(&req.tool, &req.arguments);
                if !args.is_empty() {
                    lines.push(args);
                }
            }
        }
    }

    lines.join("\n")
}

/// Create notification keyboard for quick actions.
pub fn create_notification_keyboard(session: &Session, public_url: &str) -> InlineKeyboardMarkup {
    let has_requests = session
        .agent_state
        .as_ref()
        .and_then(|s| s.requests.as_ref())
        .is_some_and(|r| !r.is_empty());

    if session.active && has_requests {
        let request_id = session
            .agent_state
            .as_ref()
            .and_then(|s| s.requests.as_ref())
            .and_then(|r| r.keys().next())
            .map(|k| k.as_str())
            .unwrap_or("");
        let req_prefix_len = 8.min(request_id.len());
        let req_prefix = &request_id[..req_prefix_len];

        InlineKeyboardMarkup {
            inline_keyboard: vec![
                vec![
                    InlineKeyboardButton {
                        text: "Allow".into(),
                        callback_data: Some(create_callback_data(
                            APPROVE_ACTION,
                            &session.id,
                            Some(req_prefix),
                        )),
                        web_app: None,
                    },
                    InlineKeyboardButton {
                        text: "Deny".into(),
                        callback_data: Some(create_callback_data(
                            DENY_ACTION,
                            &session.id,
                            Some(req_prefix),
                        )),
                        web_app: None,
                    },
                ],
                vec![InlineKeyboardButton {
                    text: "Details".into(),
                    callback_data: None,
                    web_app: Some(WebAppInfo {
                        url: build_mini_app_deep_link(
                            public_url,
                            &format!("session_{}", session.id),
                        ),
                    }),
                }],
            ],
        }
    } else {
        InlineKeyboardMarkup {
            inline_keyboard: vec![vec![InlineKeyboardButton {
                text: "Open Session".into(),
                callback_data: None,
                web_app: Some(WebAppInfo {
                    url: build_mini_app_deep_link(public_url, &format!("session_{}", session.id)),
                }),
            }]],
        }
    }
}

fn format_tool_arguments_detailed(tool: &str, args: &Value) -> String {
    if args.is_null() {
        return String::new();
    }

    match tool {
        "Edit" => {
            let file = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let mut result = format!("File: {}", truncate(file, MAX_TOOL_ARGS_LENGTH));
            if let Some(old) = args.get("old_string").and_then(|v| v.as_str()) {
                result.push_str(&format!("\nOld: \"{}\"", truncate(old, 50)));
            }
            if let Some(new) = args.get("new_string").and_then(|v| v.as_str()) {
                result.push_str(&format!("\nNew: \"{}\"", truncate(new, 50)));
            }
            result
        }

        "Write" => {
            let file = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content_info = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(|c| format!(" ({} chars)", c.len()))
                .unwrap_or_default();
            format!(
                "File: {}{}",
                truncate(file, MAX_TOOL_ARGS_LENGTH),
                content_info
            )
        }

        "Read" => {
            let file = args
                .get("file_path")
                .or_else(|| args.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!("File: {}", truncate(file, MAX_TOOL_ARGS_LENGTH))
        }

        "Bash" => {
            let cmd = args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("Command: {}", truncate(cmd, MAX_TOOL_ARGS_LENGTH))
        }

        "Task" => {
            let desc = args
                .get("description")
                .or_else(|| args.get("prompt"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("Task: {}", truncate(desc, MAX_TOOL_ARGS_LENGTH))
        }

        "Grep" | "Glob" => {
            let pattern = args
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let mut result = format!("Pattern: {pattern}");
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                result.push_str(&format!("\nPath: {}", truncate(path, 80)));
            }
            result
        }

        "WebFetch" => {
            let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
            format!("URL: {}", truncate(url, MAX_TOOL_ARGS_LENGTH))
        }

        "TodoWrite" => {
            let count = args
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("Updating {count} todo items")
        }

        _ => {
            let arg_str = serde_json::to_string(args).unwrap_or_default();
            if arg_str.len() > 10 {
                format!("Args: {}", truncate(&arg_str, MAX_TOOL_ARGS_LENGTH))
            } else {
                String::new()
            }
        }
    }
}
