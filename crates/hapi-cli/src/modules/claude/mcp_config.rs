use serde_json::Value;
use std::collections::HashMap;

/// Serialize MCP server config to a JSON string suitable for the `--mcp-config` CLI arg.
///
/// Returns `None` if the map is empty.
pub fn resolve_mcp_config_arg(mcp_servers: &HashMap<String, Value>) -> Option<String> {
    if mcp_servers.is_empty() {
        return None;
    }
    let config = serde_json::json!({ "mcpServers": mcp_servers });
    Some(config.to_string())
}

/// Append `--mcp-config <json>` to the args list if servers are present.
///
/// Returns `true` if the arg was appended.
pub fn append_mcp_config_arg(args: &mut Vec<String>, mcp_servers: &HashMap<String, Value>) -> bool {
    match resolve_mcp_config_arg(mcp_servers) {
        Some(config_json) => {
            args.push("--mcp-config".to_string());
            args.push(config_json);
            true
        }
        None => false,
    }
}
