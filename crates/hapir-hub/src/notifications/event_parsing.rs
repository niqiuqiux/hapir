use serde_json::Value;

/// Given a message content value, try to extract the event envelope.
///
/// Handles two formats:
/// - Direct: `{type: "event", data: {type: "ready"}}`
/// - Role-wrapped: `{role: "agent", content: {type: "event", data: {type: "ready"}}}`
fn extract_event_envelope(content: &Value) -> Option<&Value> {
    let obj = content.as_object()?;

    // Direct envelope: content itself has type == "event"
    if obj.get("type").and_then(|v| v.as_str()) == Some("event") {
        return Some(content);
    }

    // Role-wrapped: unwrap .content and check for type == "event"
    let inner = obj.get("content")?;
    let inner_obj = inner.as_object()?;
    if inner_obj.get("type").and_then(|v| v.as_str()) == Some("event") {
        return Some(inner);
    }

    None
}

/// Extract the event type string from a message content value.
///
/// For a message with content `{type: "event", data: {type: "ready"}}`,
/// this returns `Some("ready")`.
///
/// For a role-wrapped message `{role: "agent", content: {type: "event", data: {type: "ready"}}}`,
/// this also returns `Some("ready")`.
pub fn extract_message_event_type(content: &Value) -> Option<String> {
    let envelope = extract_event_envelope(content)?;
    let data = envelope.get("data")?;
    let data_obj = data.as_object()?;
    data_obj.get("type")?.as_str().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn direct_event_envelope() {
        let content = json!({
            "type": "event",
            "data": { "type": "ready" }
        });
        assert_eq!(
            extract_message_event_type(&content),
            Some("ready".to_string())
        );
    }

    #[test]
    fn role_wrapped_event_envelope() {
        let content = json!({
            "role": "agent",
            "content": {
                "type": "event",
                "data": { "type": "ready" }
            }
        });
        assert_eq!(
            extract_message_event_type(&content),
            Some("ready".to_string())
        );
    }

    #[test]
    fn no_event_type() {
        let content = json!({
            "type": "message",
            "data": { "text": "hello" }
        });
        assert_eq!(extract_message_event_type(&content), None);
    }

    #[test]
    fn missing_data() {
        let content = json!({
            "type": "event"
        });
        assert_eq!(extract_message_event_type(&content), None);
    }

    #[test]
    fn data_type_not_string() {
        let content = json!({
            "type": "event",
            "data": { "type": 42 }
        });
        assert_eq!(extract_message_event_type(&content), None);
    }

    #[test]
    fn non_object_content() {
        let content = json!("just a string");
        assert_eq!(extract_message_event_type(&content), None);
    }

    #[test]
    fn null_content() {
        let content = Value::Null;
        assert_eq!(extract_message_event_type(&content), None);
    }
}
