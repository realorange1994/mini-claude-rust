//! Tool argument coercion (Hermes-style)
//!
//! LLMs frequently pass arguments with incorrect types -- e.g., a string "42"
//! when the schema expects an integer 42. This module provides automatic
//! type coercion to fix these common mistakes before tool execution.

use serde_json::Value;
use std::collections::HashMap;

/// Result of argument coercion
pub struct CoercionResult {
    /// The coerced arguments
    pub args: HashMap<String, Value>,
    /// Warnings about coercions that were applied
    pub warnings: Vec<CoercionWarning>,
}

/// A warning about a coercion that was applied
#[derive(Debug, Clone)]
pub struct CoercionWarning {
    pub param_name: String,
    pub from_type: String,
    pub to_type: String,
    pub from_value: String,
    pub to_value: String,
}

impl std::fmt::Display for CoercionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Coerced '{}' from {} ({}) to {} ({})",
            self.param_name, self.from_type, self.from_value, self.to_type, self.to_value
        )
    }
}

/// Remap official Claude Code parameter names to internal names.
/// LLMs see the official schema (file_path, directory) but internal tool code
/// reads params["path"] and params["dir"]. This function copies the values over.
pub fn remap_file_path(args: &mut HashMap<String, Value>) {
    if let Some(fp) = args.get("file_path").and_then(|v| v.as_str()) {
        if !fp.is_empty() {
            args.insert("path".to_string(), Value::String(fp.to_string()));
        }
    }
}

pub fn remap_dir_param(args: &mut HashMap<String, Value>) {
    if let Some(dir) = args.get("directory").and_then(|v| v.as_str()) {
        if !dir.is_empty() {
            args.insert("dir".to_string(), Value::String(dir.to_string()));
        }
    }
}

/// Coerce argument types to match the tool's input schema.
/// Returns a CoercionResult with the coerced args and any warnings.
pub fn coerce_arguments(
    schema: &serde_json::Map<String, Value>,
    args: &mut HashMap<String, Value>,
) -> CoercionResult {
    let mut warnings = Vec::new();

    let properties = match schema.get("properties") {
        Some(Value::Object(props)) => props,
        _ => {
            return CoercionResult {
                args: args.clone(),
                warnings,
            };
        }
    };

    for (name, schema_val) in properties {
        let arg = match args.get(name) {
            Some(a) => a,
            None => continue,
        };

        let expected_type = match schema_val.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };

        if let Some(coerced) = coerce_value(arg, expected_type) {
            let from_type = json_type_name(arg);
            let to_type = expected_type.to_string();
            let from_value = truncate_display(arg, 50);
            let to_value = truncate_display(&coerced, 50);

            warnings.push(CoercionWarning {
                param_name: name.clone(),
                from_type,
                to_type,
                from_value,
                to_value,
            });

            args.insert(name.clone(), coerced);
        }
    }

    CoercionResult {
        args: args.clone(),
        warnings,
    }
}

/// Try to coerce a single value to the expected type.
/// Returns Some(coerced_value) if coercion was applied, None if no coercion needed.
fn coerce_value(value: &Value, expected_type: &str) -> Option<Value> {
    match expected_type {
        "integer" | "number" => coerce_to_number(value, expected_type),
        "string" => coerce_to_string(value),
        "boolean" => coerce_to_boolean(value),
        "array" => coerce_to_array(value),
        "object" => coerce_to_object(value),
        _ => None,
    }
}

/// Coerce to integer or number
fn coerce_to_number(value: &Value, expected_type: &str) -> Option<Value> {
    // Already the right type?
    if expected_type == "integer" && value.is_i64() {
        return None;
    }
    if expected_type == "number" && (value.is_f64() || value.is_i64()) {
        return None;
    }

    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if expected_type == "integer" {
                trimmed.parse::<i64>().ok().map(Value::from)
            } else {
                trimmed.parse::<f64>().ok().map(Value::from)
            }
        }
        Value::Bool(b) => {
            if expected_type == "integer" {
                Some(Value::from(if *b { 1 } else { 0 }))
            } else {
                Some(Value::from(if *b { 1.0 } else { 0.0 }))
            }
        }
        Value::Number(n) => {
            if expected_type == "integer" {
                n.as_f64().map(|f| Value::from(f as i64))
            } else {
                n.as_f64().map(Value::from)
            }
        }
        _ => None,
    }
}

/// Coerce to string
fn coerce_to_string(value: &Value) -> Option<Value> {
    if value.is_string() {
        return None;
    }

    match value {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::from(i.to_string()))
            } else if let Some(f) = n.as_f64() {
                Some(Value::from(f.to_string()))
            } else {
                None
            }
        }
        Value::Bool(b) => Some(Value::from(b.to_string())),
        _ => None,
    }
}

/// Coerce to boolean
fn coerce_to_boolean(value: &Value) -> Option<Value> {
    if value.is_boolean() {
        return None;
    }

    match value {
        Value::String(s) => {
            let lower = s.to_lowercase();
            match lower.as_str() {
                "true" | "1" | "yes" | "on" => Some(Value::Bool(true)),
                "false" | "0" | "no" | "off" => Some(Value::Bool(false)),
                _ => None,
            }
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Bool(i != 0))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Coerce to array
fn coerce_to_array(value: &Value) -> Option<Value> {
    if value.is_array() {
        return None;
    }

    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            // Try to parse as JSON array
            if (trimmed.starts_with('[') && trimmed.ends_with(']'))
                || (trimmed.starts_with('{') && trimmed.ends_with('}'))
            {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    if parsed.is_array() {
                        return Some(parsed);
                    }
                }
            }
            // Fallback: wrap in a single-element array
            Some(Value::Array(vec![Value::String(s.clone())]))
        }
        _ => None,
    }
}

/// Coerce to object
fn coerce_to_object(value: &Value) -> Option<Value> {
    if value.is_object() {
        return None;
    }

    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') && trimmed.ends_with('}') {
                serde_json::from_str::<Value>(trimmed).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Get a human-readable type name for a JSON value
fn json_type_name(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "boolean".to_string(),
        Value::Number(n) => {
            if n.is_i64() {
                "integer".to_string()
            } else {
                "number".to_string()
            }
        }
        Value::String(_) => "string".to_string(),
        Value::Array(_) => "array".to_string(),
        Value::Object(_) => "object".to_string(),
    }
}

/// Truncate a value for display in warnings
fn truncate_display(value: &Value, max_len: usize) -> String {
    let s = match value {
        Value::String(s) => s.clone(),
        _ => format!("{:?}", value),
    };
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_schema(properties: serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
        let mut schema = serde_json::Map::new();
        schema.insert("properties".to_string(), Value::Object(properties));
        schema
    }

    #[test]
    fn test_coerce_string_to_integer() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("count".to_string(), json!({"type": "integer"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("count".to_string(), json!("42"))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["count"], json!(42));
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].param_name, "count");
    }

    #[test]
    fn test_coerce_integer_to_string() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("name".to_string(), json!({"type": "string"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("name".to_string(), json!(42))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["name"], json!("42"));
    }

    #[test]
    fn test_coerce_string_to_boolean() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("verbose".to_string(), json!({"type": "boolean"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("verbose".to_string(), json!("true"))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["verbose"], json!(true));

        let mut args2 = HashMap::from([("verbose".to_string(), json!("false"))]);
        let result2 = coerce_arguments(&schema, &mut args2);
        assert_eq!(result2.args["verbose"], json!(false));
    }

    #[test]
    fn test_coerce_string_to_array() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("items".to_string(), json!({"type": "array"}));
        schema.insert("properties".to_string(), Value::Object(props));

        // JSON array string
        let mut args = HashMap::from([("items".to_string(), json!("[1, 2, 3]"))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["items"], json!([1, 2, 3]));
    }

    #[test]
    fn test_no_coercion_when_correct_type() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("count".to_string(), json!({"type": "integer"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("count".to_string(), json!(42))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["count"], json!(42));
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_coerce_bool_to_integer() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("flag".to_string(), json!({"type": "integer"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("flag".to_string(), json!(true))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["flag"], json!(1));
    }

    #[test]
    fn test_coerce_string_to_number() {
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("ratio".to_string(), json!({"type": "number"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("ratio".to_string(), json!("3.14"))]);
        let result = coerce_arguments(&schema, &mut args);
        assert_eq!(result.args["ratio"], json!(3.14));
    }

    #[test]
    fn test_coerce_invalid_string_to_integer() {
        // "hello" cannot be coerced to integer -- should remain unchanged
        let mut schema = serde_json::Map::new();
        let mut props = serde_json::Map::new();
        props.insert("count".to_string(), json!({"type": "integer"}));
        schema.insert("properties".to_string(), Value::Object(props));

        let mut args = HashMap::from([("count".to_string(), json!("hello"))]);
        let result = coerce_arguments(&schema, &mut args);
        // Should not be coerced -- remains as string
        assert_eq!(result.args["count"], json!("hello"));
        assert!(result.warnings.is_empty());
    }
}
