use sha2::{Digest, Sha256};

/// Deterministically stringify a JSON value with sorted keys recursively.
///
/// Objects have their keys sorted alphabetically. Arrays preserve order.
/// The output is a compact JSON string with no extra whitespace.
pub fn deterministic_stringify(value: &serde_json::Value) -> String {
    use serde_json::Value;

    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).unwrap_or_default()
        }
        Value::Array(arr) => {
            let items: Vec<String> =
                arr.iter().map(deterministic_stringify).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let entries: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    let v = deterministic_stringify(&map[k]);
                    format!("{}:{}", serde_json::to_string(k).unwrap(), v)
                })
                .collect();
            format!("{{{}}}", entries.join(","))
        }
    }
}

/// Calculate SHA-256 hash of a JSON value using deterministic stringification.
/// Returns the hash as a lowercase hex string.
pub fn hash_object(value: &serde_json::Value) -> String {
    let json = deterministic_stringify(value);
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    hex::encode(hasher.finalize())
}

/// Deep equality comparison for two JSON values.
/// `serde_json::Value` already implements `PartialEq`, so this is trivial.
pub fn deep_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    a == b
}
