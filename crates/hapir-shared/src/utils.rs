use serde_json::Value;

pub fn is_object(value: &Value) -> bool {
    value.is_object()
}

pub fn as_string(value: &Value) -> Option<&str> {
    value.as_str()
}

pub fn as_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite())
}

pub fn safe_stringify(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| format!("{other}")),
    }
}
