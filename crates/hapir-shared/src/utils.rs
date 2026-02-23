use serde_json::Value;

pub fn is_object(value: &Value) -> bool {
    value.is_object()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_object_returns_true_for_objects() {
        assert!(is_object(&json!({"key": "value"})));
        assert!(is_object(&json!({})));
    }

    #[test]
    fn is_object_returns_false_for_non_objects() {
        assert!(!is_object(&json!("string")));
        assert!(!is_object(&json!(42)));
        assert!(!is_object(&json!([1, 2])));
        assert!(!is_object(&json!(null)));
        assert!(!is_object(&json!(true)));
    }
}
