use serde_json::Value;

/// A role-wrapped record from agent message envelopes.
#[derive(Debug, Clone)]
pub struct RoleWrappedRecord {
    pub role: String,
    pub content: Value,
    pub meta: Option<Value>,
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

pub fn is_role_wrapped_record(value: &Value) -> bool {
    if let Some(obj) = value.as_object() {
        obj.get("role").is_some_and(|r| r.is_string()) && obj.contains_key("content")
    } else {
        false
    }
}

fn extract_role_wrapped(value: &Value) -> Option<RoleWrappedRecord> {
    let obj = value.as_object()?;
    let role = obj.get("role")?.as_str()?.to_string();
    let content = obj.get("content")?.clone();
    let meta = obj.get("meta").cloned();
    Some(RoleWrappedRecord {
        role,
        content,
        meta,
    })
}

/// Unwrap a role-wrapped record from various envelope formats.
/// Checks: direct, .message, .data.message, .payload.message
pub fn unwrap_role_wrapped_record_envelope(value: &Value) -> Option<RoleWrappedRecord> {
    // Direct
    if let Some(r) = extract_role_wrapped(value) {
        return Some(r);
    }

    if !is_object(value) {
        return None;
    }

    let obj = value.as_object().unwrap();

    // .message
    if let Some(msg) = obj.get("message")
        && let Some(r) = extract_role_wrapped(msg)
    {
        return Some(r);
    }

    // .data.message
    if let Some(data) = obj.get("data")
        && let Some(data_obj) = data.as_object()
        && let Some(msg) = data_obj.get("message")
        && let Some(r) = extract_role_wrapped(msg)
    {
        return Some(r);
    }

    // .payload.message
    if let Some(payload) = obj.get("payload")
        && let Some(payload_obj) = payload.as_object()
        && let Some(msg) = payload_obj.get("message")
        && let Some(r) = extract_role_wrapped(msg)
    {
        return Some(r);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn direct_role_wrapped() {
        let val = json!({"role": "user", "content": "hello"});
        let r = unwrap_role_wrapped_record_envelope(&val).unwrap();
        assert_eq!(r.role, "user");
        assert_eq!(r.content, json!("hello"));
    }

    #[test]
    fn nested_message() {
        let val = json!({"message": {"role": "assistant", "content": "hi"}});
        let r = unwrap_role_wrapped_record_envelope(&val).unwrap();
        assert_eq!(r.role, "assistant");
    }

    #[test]
    fn nested_data_message() {
        let val = json!({"data": {"message": {"role": "user", "content": "x"}}});
        let r = unwrap_role_wrapped_record_envelope(&val).unwrap();
        assert_eq!(r.role, "user");
    }

    #[test]
    fn nested_payload_message() {
        let val = json!({"payload": {"message": {"role": "user", "content": "y"}}});
        let r = unwrap_role_wrapped_record_envelope(&val).unwrap();
        assert_eq!(r.role, "user");
    }

    #[test]
    fn not_role_wrapped() {
        let val = json!({"foo": "bar"});
        assert!(unwrap_role_wrapped_record_envelope(&val).is_none());
    }
}
