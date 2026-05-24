//! JSON utility functions for handling JSON data.

use serde_json::Value;

/// Extract a string value from a JSON object by key.
pub fn get_string<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(|v| v.as_str())
}

/// Extract an integer value from a JSON object by key.
pub fn get_i64(obj: &Value, key: &str) -> Option<i64> {
    obj.get(key).and_then(|v| v.as_i64())
}

/// Extract a float value from a JSON object by key.
pub fn get_f64(obj: &Value, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| v.as_f64())
}

/// Extract a boolean value from a JSON object by key.
pub fn get_bool(obj: &Value, key: &str) -> Option<bool> {
    obj.get(key).and_then(|v| v.as_bool())
}

/// Extract a nested string value using dot notation (e.g., "data.name").
pub fn get_nested_string<'a>(obj: &'a Value, path: &str) -> Option<&'a str> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = obj;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            return current.get(part).and_then(|v| v.as_str());
        }
        current = current.get(*part)?;
    }
    None
}

/// Merge two JSON objects. Values in `override_obj` take precedence.
pub fn merge_json(base: &Value, override_val: &Value) -> Value {
    match (base, override_val) {
        (Value::Object(base_map), Value::Object(override_map)) => {
            let mut result = base_map.clone();
            for (key, value) in override_map {
                if let Some(base_value) = result.get(key) {
                    result.insert(key.clone(), merge_json(base_value, value));
                } else {
                    result.insert(key.clone(), value.clone());
                }
            }
            Value::Object(result)
        }
        (_, override_val) => override_val.clone(),
    }
}

/// Truncate a JSON value to a string representation with a max length.
pub fn truncate_json_string(value: &Value, max_len: usize) -> String {
    let s = value.to_string();
    if s.len() <= max_len {
        s
    } else {
        format!("{}... (truncated, {} bytes total)", &s[..max_len], s.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_string() {
        let obj = json!({"name": "test", "count": 42});
        assert_eq!(get_string(&obj, "name"), Some("test"));
        assert_eq!(get_string(&obj, "missing"), None);
    }

    #[test]
    fn test_get_i64() {
        let obj = json!({"count": 42});
        assert_eq!(get_i64(&obj, "count"), Some(42));
    }

    #[test]
    fn test_get_bool() {
        let obj = json!({"active": true});
        assert_eq!(get_bool(&obj, "active"), Some(true));
    }

    #[test]
    fn test_get_nested_string() {
        let obj = json!({"data": {"name": "inner"}});
        assert_eq!(get_nested_string(&obj, "data.name"), Some("inner"));
        assert_eq!(get_nested_string(&obj, "data.missing"), None);
    }

    #[test]
    fn test_merge_json() {
        let base = json!({"a": 1, "b": 2});
        let override_val = json!({"b": 3, "c": 4});
        let result = merge_json(&base, &override_val);
        assert_eq!(result["a"], 1);
        assert_eq!(result["b"], 3);
        assert_eq!(result["c"], 4);
    }

    #[test]
    fn test_truncate_json_string() {
        let value = json!({"key": "a very long value that should be truncated"});
        let result = truncate_json_string(&value, 20);
        assert!(result.contains("truncated"));
    }
}
