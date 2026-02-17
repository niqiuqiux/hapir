use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::debug;

/// Run the MCP stdio bridge command.
///
/// Reads JSON-RPC messages from stdin (line-delimited), handles MCP
/// protocol messages, and forwards `tools/call` requests to an HTTP
/// backend. All diagnostic output goes to stderr; only valid JSON-RPC
/// is written to stdout.
pub async fn run(args: Vec<String>) -> Result<()> {
    debug!(?args, "mcp command starting");

    let url = parse_url(&args)?;
    eprintln!("[hapi-mcp] bridge starting, url={}", url);

    let client = reqwest::Client::new();
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // EOF
            eprintln!("[hapi-mcp] stdin closed, exiting");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[hapi-mcp] invalid JSON: {}", e);
                continue;
            }
        };

        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

        debug!("[hapi-mcp] method={}", method);

        let response = match method {
            "initialize" => Some(jsonrpc_ok(
                id,
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": {
                        "name": "hapi-mcp-bridge",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )),
            "notifications/initialized" => None,
            "tools/list" => Some(jsonrpc_ok(
                id,
                serde_json::json!({
                    "tools": [{
                        "name": "change_title",
                        "description": "Change the session title",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "The new title"
                                }
                            },
                            "required": ["title"]
                        }
                    }]
                }),
            )),
            "tools/call" => {
                let result = handle_tools_call(&client, &url, &msg).await;
                Some(match result {
                    Ok(val) => jsonrpc_ok(id, val),
                    Err(e) => jsonrpc_error(id, -32000, &e.to_string()),
                })
            }
            _ => Some(jsonrpc_error(id, -32601, "Method not found")),
        };

        if let Some(resp) = response {
            let mut out = serde_json::to_vec(&resp)?;
            out.push(b'\n');
            stdout.write_all(&out).await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

/// Parse `--url <URL>` from args, falling back to `HAPIR_HTTP_MCP_URL` env var.
fn parse_url(args: &[String]) -> Result<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--url" {
            if let Some(val) = iter.next() {
                return Ok(val.clone());
            }
        } else if let Some(val) = arg.strip_prefix("--url=") {
            return Ok(val.to_string());
        }
    }
    std::env::var("HAPIR_HTTP_MCP_URL")
        .context("No --url argument and HAPIR_HTTP_MCP_URL not set")
}

/// Handle a `tools/call` request by forwarding to the HTTP backend.
async fn handle_tools_call(
    client: &reqwest::Client,
    url: &str,
    msg: &Value,
) -> Result<Value> {
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    eprintln!("[hapi-mcp] tools/call: tool={}", tool_name);

    let resp = client
        .post(url)
        .json(&serde_json::json!({
            "tool": tool_name,
            "arguments": arguments,
        }))
        .send()
        .await
        .context("HTTP request to MCP backend failed")?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({"error": "non-JSON response"}));

    if status.is_success() {
        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": body.to_string()
            }]
        }))
    } else {
        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": format!("HTTP {}: {}", status, body)
            }],
            "isError": true
        }))
    }
}

/// Build a JSON-RPC 2.0 success response.
fn jsonrpc_ok(id: Option<Value>, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// Build a JSON-RPC 2.0 error response.
fn jsonrpc_error(id: Option<Value>, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}
