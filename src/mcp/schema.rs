//! Lightweight JSON Schema validation for MCP tool inputs.
//! Ported from upstream mcp/schema.go (201 lines).
//!
//! Validates arguments against tool input schemas with helpful error messages.

use std::collections::HashMap;

/// Validate args against a JSON Schema (InputSchema from MCP tool definition).
/// Returns an error if validation fails, None if valid.
pub fn validate_schema(
    args: &HashMap<String, serde_json::Value>,
    schema: &HashMap<String, serde_json::Value>,
) -> Option<String> {
    if schema.is_empty() {
        return None; // No schema = no validation
    }

    if let Some(serde_json::Value::String(schema_type)) = schema.get("type") {
        if schema_type != "object" {
            return Some(format!(
                "expected schema type 'object', got {:?}",
                schema_type
            ));
        }
    }

    // Required fields check
    if let Some(serde_json::Value::Array(required)) = schema.get("required") {
        for req in required {
            if let serde_json::Value::String(field) = req {
                if field.is_empty() {
                    continue;
                }
                if !args.contains_key(field) {
                    return Some(format!("missing required parameter: {:?}", field));
                }
            }
        }
    }

    // Property type validation
    let Some(serde_json::Value::Object(properties)) = schema.get("properties") else {
        return None; // No properties defined
    };

    for (name, prop_schema) in properties {
        let Some(prop_def) = prop_schema.as_object() else {
            continue;
        };

        if let Some(val) = args.get(name) {
            if let Some(err) = validate_property(name, val, prop_def) {
                return Some(err);
            }
        }
    }

    None
}

fn validate_property(
    name: &str,
    val: &serde_json::Value,
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let Some(serde_json::Value::String(prop_type)) = schema.get("type") else {
        return None; // No type constraint
    };

    match prop_type.as_str() {
        "null" => {
            if !val.is_null() {
                return Some(format!(
                    "parameter {:?} must be null, got {:?}",
                    name,
                    value_type_name(val)
                ));
            }
            return None;
        }
        "string" => {
            if let Some(s) = val.as_str() {
                if let Some(serde_json::Value::Number(min_len)) = schema.get("minLength") {
                    if let Some(min) = min_len.as_f64() {
                        if (s.len() as f64) < min {
                            return Some(format!(
                                "parameter {:?} must be at least {:.0} characters, got {}",
                                name,
                                min,
                                s.len()
                            ));
                        }
                    }
                }
                if let Some(serde_json::Value::Number(max_len)) = schema.get("maxLength") {
                    if let Some(max) = max_len.as_f64() {
                        if (s.len() as f64) > max {
                            return Some(format!(
                                "parameter {:?} must be at most {:.0} characters, got {}",
                                name,
                                max,
                                s.len()
                            ));
                        }
                    }
                }
            } else {
                return Some(format!(
                    "parameter {:?} must be a string, got {:?}",
                    name,
                    value_type_name(val)
                ));
            }
        }
        "number" | "integer" => {
            if let Some(f) = val.as_f64() {
                if let Some(serde_json::Value::Number(min_val)) = schema.get("minimum") {
                    if let Some(min) = min_val.as_f64() {
                        if f < min {
                            return Some(format!(
                                "parameter {:?} must be >= {:.0}, got {}",
                                name, min, f
                            ));
                        }
                    }
                }
                if let Some(serde_json::Value::Number(max_val)) = schema.get("maximum") {
                    if let Some(max) = max_val.as_f64() {
                        if f > max {
                            return Some(format!(
                                "parameter {:?} must be <= {:.0}, got {}",
                                name, max, f
                            ));
                        }
                    }
                }
            } else {
                return Some(format!(
                    "parameter {:?} must be a number, got {:?}",
                    name,
                    value_type_name(val)
                ));
            }
        }
        "boolean" => {
            if !val.is_boolean() {
                return Some(format!(
                    "parameter {:?} must be a boolean, got {:?}",
                    name,
                    value_type_name(val)
                ));
            }
        }
        "array" => {
            if let Some(arr) = val.as_array() {
                // Validate array items
                if let Some(serde_json::Value::Object(items)) = schema.get("items") {
                    for (i, item) in arr.iter().enumerate() {
                        if let Some(err) =
                            validate_property(&format!("{}[{}]", name, i), item, items)
                        {
                            return Some(err);
                        }
                    }
                }
                // Min/Max items
                if let Some(serde_json::Value::Number(min_items)) = schema.get("minItems") {
                    if let Some(min) = min_items.as_f64() {
                        if (arr.len() as f64) < min {
                            return Some(format!(
                                "parameter {:?} must have at least {:.0} items, got {}",
                                name,
                                min,
                                arr.len()
                            ));
                        }
                    }
                }
                if let Some(serde_json::Value::Number(max_items)) = schema.get("maxItems") {
                    if let Some(max) = max_items.as_f64() {
                        if (arr.len() as f64) > max {
                            return Some(format!(
                                "parameter {:?} must have at most {:.0} items, got {}",
                                name,
                                max,
                                arr.len()
                            ));
                        }
                    }
                }
            } else {
                return Some(format!(
                    "parameter {:?} must be an array, got {:?}",
                    name,
                    value_type_name(val)
                ));
            }
        }
        "object" => {
            if let Some(obj) = val.as_object() {
                // Recursively validate nested objects
                if let Some(serde_json::Value::Object(properties)) = schema.get("properties") {
                    let mut nested_schema = HashMap::new();
                    nested_schema.insert(
                        "type".to_string(),
                        serde_json::Value::String("object".to_string()),
                    );
                    nested_schema.insert(
                        "properties".to_string(),
                        serde_json::Value::Object(properties.clone()),
                    );
                    if let Some(required) = schema.get("required") {
                        nested_schema
                            .insert("required".to_string(), required.clone());
                    }
                    // Convert HashMap<String, Value> to HashMap<String, Value>
                    let nested_args: HashMap<String, serde_json::Value> = obj
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    return validate_schema(&nested_args, &nested_schema);
                }
            } else {
                return Some(format!(
                    "parameter {:?} must be an object, got {:?}",
                    name,
                    value_type_name(val)
                ));
            }
        }
        _ => {} // Unknown type — skip
    }

    // Enum check (works for all types)
    if let Some(serde_json::Value::Array(enum_values)) = schema.get("enum") {
        if !enum_values.is_empty() && !contains_enum(val, enum_values) {
            let vals: Vec<String> = enum_values.iter().map(format_value).collect();
            return Some(format!(
                "parameter {:?} must be one of [{}], got {}",
                name,
                vals.join(", "),
                format_value(val)
            ));
        }
    }

    None
}

fn contains_enum(val: &serde_json::Value, enum_values: &[serde_json::Value]) -> bool {
    for e in enum_values {
        if e == val {
            return true;
        }
        // String comparison for numbers
        if let (Some(f1), Some(f2)) = (val.as_f64(), e.as_f64()) {
            if f1 == f2 {
                return true;
            }
        }
        if let (Some(s1), Some(s2)) = (val.as_str(), e.as_str()) {
            if s1 == s2 {
                return true;
            }
        }
    }
    false
}

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => format!("{:?}", s),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                format!("{}", i)
            } else if let Some(f) = n.as_f64() {
                if f == f.floor() {
                    format!("{}", f as i64)
                } else {
                    format!("{}", f)
                }
            } else {
                format!("{}", n)
            }
        }
        serde_json::Value::Bool(b) => format!("{}", b),
        serde_json::Value::Null => "null".to_string(),
        _ => format!("{}", v),
    }
}

fn value_type_name(val: &serde_json::Value) -> &'static str {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_schema(
        required: Vec<&str>,
        properties: Vec<(&str, serde_json::Value)>,
    ) -> HashMap<String, serde_json::Value> {
        let mut schema = HashMap::new();
        schema.insert(
            "type".to_string(),
            serde_json::Value::String("object".to_string()),
        );
        if !required.is_empty() {
            schema.insert(
                "required".to_string(),
                serde_json::Value::Array(
                    required
                        .into_iter()
                        .map(|s| serde_json::Value::String(s.to_string()))
                        .collect(),
                ),
            );
        }
        schema.insert(
            "properties".to_string(),
            serde_json::Value::Object(
                properties
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            ),
        );
        schema
    }

    fn make_string_schema() -> HashMap<String, serde_json::Value> {
        make_schema(
            vec!["name"],
            vec![(
                "name",
                serde_json::Value::Object(
                    vec![(
                        "type".to_string(),
                        serde_json::Value::String("string".to_string()),
                    )]
                    .into_iter()
                    .collect(),
                ),
            )],
        )
    }

    #[test]
    fn test_validate_schema_no_schema() {
        assert!(validate_schema(&HashMap::new(), &HashMap::new()).is_none());
    }

    #[test]
    fn test_validate_schema_missing_required() {
        let schema = make_string_schema();
        let args = HashMap::new();
        let err = validate_schema(&args, &schema).unwrap();
        assert!(err.contains("missing required parameter"));
    }

    #[test]
    fn test_validate_schema_valid() {
        let schema = make_string_schema();
        let mut args = HashMap::new();
        args.insert(
            "name".to_string(),
            serde_json::Value::String("test".to_string()),
        );
        assert!(validate_schema(&args, &schema).is_none());
    }

    #[test]
    fn test_validate_schema_wrong_type() {
        let schema = make_string_schema();
        let mut args = HashMap::new();
        args.insert("name".to_string(), serde_json::Value::Number(42.into()));
        let err = validate_schema(&args, &schema).unwrap();
        assert!(err.contains("must be a string"));
    }

    #[test]
    fn test_validate_schema_enum() {
        let mut prop = serde_json::Map::new();
        prop.insert(
            "type".to_string(),
            serde_json::Value::String("string".to_string()),
        );
        prop.insert(
            "enum".to_string(),
            serde_json::Value::Array(vec![
                serde_json::Value::String("a".to_string()),
                serde_json::Value::String("b".to_string()),
            ]),
        );
        let schema = make_schema(vec!["choice"], vec![("choice", serde_json::Value::Object(prop))]);

        let mut args = HashMap::new();
        args.insert(
            "choice".to_string(),
            serde_json::Value::String("c".to_string()),
        );
        let err = validate_schema(&args, &schema).unwrap();
        assert!(err.contains("must be one of"));
    }

    #[test]
    fn test_validate_schema_string_length() {
        let mut prop = serde_json::Map::new();
        prop.insert(
            "type".to_string(),
            serde_json::Value::String("string".to_string()),
        );
        prop.insert(
            "minLength".to_string(),
            serde_json::Value::Number(3.into()),
        );
        prop.insert(
            "maxLength".to_string(),
            serde_json::Value::Number(10.into()),
        );
        let schema = make_schema(vec!["name"], vec![("name", serde_json::Value::Object(prop))]);

        // Too short
        let mut args = HashMap::new();
        args.insert(
            "name".to_string(),
            serde_json::Value::String("ab".to_string()),
        );
        let err = validate_schema(&args, &schema).unwrap();
        assert!(err.contains("at least 3"));

        // Too long
        let mut args = HashMap::new();
        args.insert(
            "name".to_string(),
            serde_json::Value::String("this is too long".to_string()),
        );
        let err = validate_schema(&args, &schema).unwrap();
        assert!(err.contains("at most 10"));
    }
}
