use serde_json::Value;

/// Derive a human-readable tool name from a permission request's fields.
///
/// Priority: title > rawInput.name > rawInput.tool > kind > "Tool"
pub fn derive_tool_name(
    title: Option<&str>,
    kind: Option<&str>,
    raw_input: Option<&Value>,
) -> String {
    if let Some(t) = title {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    if let Some(Value::Object(obj)) = raw_input {
        if let Some(Value::String(name)) = obj.get("name") {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(Value::String(tool)) = obj.get("tool") {
            let trimmed = tool.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    if let Some(k) = kind {
        let trimmed = k.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    "Tool".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn returns_title_when_present() {
        assert_eq!(derive_tool_name(Some("bash"), None, None), "bash");
    }

    #[test]
    fn returns_raw_input_name_when_no_title() {
        let input = json!({"name": "read_file"});
        assert_eq!(derive_tool_name(None, None, Some(&input)), "read_file");
    }

    #[test]
    fn returns_raw_input_tool_when_no_name() {
        let input = json!({"tool": "write_file"});
        assert_eq!(derive_tool_name(None, None, Some(&input)), "write_file");
    }

    #[test]
    fn returns_kind_when_no_raw_input() {
        assert_eq!(derive_tool_name(None, Some("execute"), None), "execute");
    }

    #[test]
    fn returns_default_when_nothing() {
        assert_eq!(derive_tool_name(None, None, None), "Tool");
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(derive_tool_name(Some("  bash  "), None, None), "bash");
    }

    #[test]
    fn skips_empty_title() {
        assert_eq!(
            derive_tool_name(Some("  "), Some("fallback"), None),
            "fallback"
        );
    }
}
