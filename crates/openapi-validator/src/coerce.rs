//! Conversion of string-typed input (query strings, headers, form fields,
//! XML text) into the JSON values a schema expects.

use serde_json::Value;

/// Resolve a local JSON reference such as `#/components/schemas/Pet`.
pub fn resolve_pointer<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    root.pointer(pointer)
}

/// Follow `$ref` chains so keyword lookups see the real schema.
///
/// Bounded so a reference cycle cannot loop forever.
pub fn deref<'a>(root: &'a Value, mut schema: &'a Value) -> &'a Value {
    for _ in 0..32 {
        match schema.get("$ref").and_then(Value::as_str) {
            Some(reference) => match resolve_pointer(root, reference) {
                Some(target) => schema = target,
                None => break,
            },
            None => break,
        }
    }
    schema
}

/// The declared `type` of a schema as a list (`type` may be a string or an array).
pub fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

pub fn is_array_schema(root: &Value, schema: Option<&Value>) -> bool {
    schema
        .map(|s| schema_types(deref(root, s)).contains(&"array"))
        .unwrap_or(false)
}

pub fn is_object_schema(root: &Value, schema: Option<&Value>) -> bool {
    schema
        .map(|s| {
            let s = deref(root, s);
            schema_types(s).contains(&"object") || s.get("properties").is_some()
        })
        .unwrap_or(false)
}

/// The `items` schema of an array schema, following references.
pub fn items_schema<'a>(root: &'a Value, schema: Option<&'a Value>) -> Option<&'a Value> {
    schema
        .map(|s| deref(root, s))
        .and_then(|s| s.get("items"))
        .map(|s| deref(root, s))
}

/// The schema of a named property, following references.
pub fn property_schema<'a>(
    root: &'a Value,
    schema: Option<&'a Value>,
    name: &str,
) -> Option<&'a Value> {
    schema
        .map(|s| deref(root, s))
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.get(name))
        .map(|s| deref(root, s))
}

/// Convert a raw string into the JSON value the schema expects.
///
/// A `type: string` schema never reinterprets its input, so values like `123`,
/// `true` or `null` stay strings. When several types are allowed, the most
/// specific parse that succeeds wins. Without a declared type the value is
/// parsed as JSON when possible and kept as a string otherwise.
pub fn coerce(raw: &str, root: &Value, schema: Option<&Value>) -> Value {
    let types = schema.map(|s| schema_types(deref(root, s))).unwrap_or_default();
    if types.is_empty() {
        return serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
    }

    if types.contains(&"integer") {
        if let Ok(i) = raw.parse::<i64>() {
            return Value::from(i);
        }
        if let Ok(u) = raw.parse::<u64>() {
            return Value::from(u);
        }
    }
    if types.contains(&"number") {
        if let Some(n) = raw.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
            return Value::Number(n);
        }
    }
    if types.contains(&"boolean") {
        match raw {
            "true" => return Value::Bool(true),
            "false" => return Value::Bool(false),
            _ => {}
        }
    }
    if types.contains(&"null") && (raw.is_empty() || raw == "null") {
        return Value::Null;
    }
    if types.contains(&"string") {
        return Value::String(raw.to_string());
    }
    if types.contains(&"object") || types.contains(&"array") {
        if let Ok(v) = serde_json::from_str::<Value>(raw) {
            return v;
        }
    }

    // Nothing matched: hand the raw string to the validator so it reports
    // the type mismatch.
    Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deref_follows_local_refs() {
        let root = json!({"components": {"schemas": {
            "A": {"$ref": "#/components/schemas/B"}, "B": {"type": "integer"}
        }}});
        let a = &root["components"]["schemas"]["A"];
        assert_eq!(deref(&root, a), &json!({"type": "integer"}));
    }

    #[test]
    fn deref_survives_cycles() {
        let root = json!({"a": {"$ref": "#/b"}, "b": {"$ref": "#/a"}});
        let _ = deref(&root, &root["a"]);
    }

    #[test]
    fn coerce_by_type() {
        let root = Value::Null;
        assert_eq!(coerce("12", &root, Some(&json!({"type": "integer"}))), json!(12));
        assert_eq!(coerce("12", &root, Some(&json!({"type": "string"}))), json!("12"));
        assert_eq!(coerce("1.5", &root, Some(&json!({"type": "number"}))), json!(1.5));
        assert_eq!(coerce("true", &root, Some(&json!({"type": "boolean"}))), json!(true));
        assert_eq!(coerce("", &root, Some(&json!({"type": ["integer", "null"]}))), Value::Null);
        assert_eq!(coerce("x", &root, Some(&json!({"type": "integer"}))), json!("x"));
        assert_eq!(coerce("[1]", &root, None), json!([1]));
        assert_eq!(coerce("abc", &root, None), json!("abc"));
    }
}
