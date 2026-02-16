/// ACP session update type constants.
///
/// These match the `sessionUpdate` field values sent by ACP agents
/// inside `session/update` notifications.
pub const AGENT_MESSAGE_CHUNK: &str = "agent_message_chunk";
pub const AGENT_THOUGHT_CHUNK: &str = "agent_thought_chunk";
pub const TOOL_CALL: &str = "tool_call";
pub const TOOL_CALL_UPDATE: &str = "tool_call_update";
pub const PLAN: &str = "plan";
