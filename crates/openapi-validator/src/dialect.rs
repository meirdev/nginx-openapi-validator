//! Document-level rewrites applied before schemas are compiled.
//!
//! * OpenAPI 3.0.x schemas are a dialect of JSON Schema with `nullable` and
//!   boolean `exclusiveMinimum`/`exclusiveMaximum`. They are rewritten into
//!   their Draft 2020-12 equivalents so one validator engine serves both
//!   3.0 and 3.1 documents.
//! * For request validation, `readOnly` properties are removed from
//!   `required`, since a client is not expected to send them.

use serde_json::{Map, Value};

use crate::coerce::deref;
use crate::error::SpecError;

/// Keys whose values are data, not schemas, and must not be rewritten.
const DATA_KEYS: &[&str] = &["example", "examples", "default", "const", "enum"];

pub fn is_openapi_30(version: &str) -> bool {
    version.starts_with("3.0")
}

/// Rewrite OpenAPI 3.0 schema keywords into JSON Schema 2020-12 form.
pub fn apply_openapi_30(doc: &mut Value) -> Result<(), SpecError> {
    walk_schemas(doc, &mut |obj| {
        if let Some(nullable) = obj.remove("nullable") {
            if nullable.as_bool() == Some(true) {
                add_null_type(obj);
            }
        }
        rewrite_exclusive_bound(obj, "exclusiveMinimum", "minimum")?;
        rewrite_exclusive_bound(obj, "exclusiveMaximum", "maximum")?;
        Ok(())
    })
}

fn add_null_type(obj: &mut Map<String, Value>) {
    match obj.get_mut("type") {
        Some(Value::String(t)) => {
            let types = vec![Value::String(t.clone()), Value::String("null".into())];
            obj.insert("type".into(), Value::Array(types));
        }
        Some(Value::Array(types)) => {
            if !types.iter().any(|t| t == "null") {
                types.push(Value::String("null".into()));
            }
        }
        _ => {}
    }
    if let Some(Value::Array(values)) = obj.get_mut("enum") {
        if !values.iter().any(Value::is_null) {
            values.push(Value::Null);
        }
    }
}

/// OpenAPI 3.0: `exclusiveMinimum: true` modifies `minimum`. Draft 2020-12:
/// `exclusiveMinimum` is itself the bound.
fn rewrite_exclusive_bound(
    obj: &mut Map<String, Value>,
    exclusive: &str,
    bound: &str,
) -> Result<(), SpecError> {
    match obj.get(exclusive) {
        None => Ok(()),
        Some(Value::Bool(true)) => {
            obj.remove(exclusive);
            if let Some(limit) = obj.remove(bound) {
                obj.insert(exclusive.to_string(), limit);
            }
            Ok(())
        }
        Some(Value::Bool(false)) => {
            obj.remove(exclusive);
            Ok(())
        }
        Some(other) => Err(SpecError::SchemaCompileError(format!(
            "'{exclusive}' must be a boolean in OpenAPI 3.0.x, found {other}"
        ))),
    }
}

/// Remove `readOnly` properties from every `required` list.
pub fn strip_read_only_required(doc: &mut Value) {
    // Lookups follow `$ref`, which needs an unmodified view of the document.
    let root = doc.clone();
    let _ = walk_schemas(doc, &mut |obj| {
        let Some(Value::Array(required)) = obj.get("required") else {
            return Ok(());
        };
        let Some(properties) = obj.get("properties") else {
            return Ok(());
        };
        let keep: Vec<Value> = required
            .iter()
            .filter(|name| {
                let read_only = name
                    .as_str()
                    .and_then(|n| properties.get(n))
                    .map(|p| deref(&root, p))
                    .and_then(|p| p.get("readOnly"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                !read_only
            })
            .cloned()
            .collect();
        obj.insert("required".into(), Value::Array(keep));
        Ok(())
    });
}

/// Visit every JSON object in the document that could be a schema, depth
/// first, skipping data-valued keys.
fn walk_schemas<F>(value: &mut Value, visit: &mut F) -> Result<(), SpecError>
where
    F: FnMut(&mut Map<String, Value>) -> Result<(), SpecError>,
{
    match value {
        Value::Object(obj) => {
            for (key, child) in obj.iter_mut() {
                if !DATA_KEYS.contains(&key.as_str()) {
                    walk_schemas(child, visit)?;
                }
            }
            visit(obj)
        }
        Value::Array(items) => {
            for item in items {
                walk_schemas(item, visit)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nullable_becomes_null_type_and_enum_member() {
        let mut doc = json!({"s": {"type": "string", "nullable": true, "enum": ["a"]}});
        apply_openapi_30(&mut doc).unwrap();
        assert_eq!(doc["s"], json!({"type": ["string", "null"], "enum": ["a", null]}));
    }

    #[test]
    fn boolean_exclusive_minimum_is_rewritten() {
        let mut doc = json!({"s": {"type": "number", "minimum": 5, "exclusiveMinimum": true}});
        apply_openapi_30(&mut doc).unwrap();
        assert_eq!(doc["s"], json!({"type": "number", "exclusiveMinimum": 5}));
    }

    #[test]
    fn numeric_exclusive_minimum_is_rejected_in_30() {
        let mut doc = json!({"s": {"type": "number", "exclusiveMinimum": 5}});
        assert!(matches!(
            apply_openapi_30(&mut doc),
            Err(SpecError::SchemaCompileError(_))
        ));
    }

    #[test]
    fn examples_are_not_rewritten() {
        let mut doc = json!({"s": {"type": "string", "example": {"nullable": true}}});
        apply_openapi_30(&mut doc).unwrap();
        assert_eq!(doc["s"]["example"], json!({"nullable": true}));
    }

    #[test]
    fn read_only_is_removed_from_required() {
        let mut doc = json!({"components": {"schemas": {"Id": {"type": "string", "readOnly": true}}},
            "s": {"required": ["id", "name", "ref"], "properties": {
                "id": {"readOnly": true}, "name": {}, "ref": {"$ref": "#/components/schemas/Id"}
            }}});
        strip_read_only_required(&mut doc);
        assert_eq!(doc["s"]["required"], json!(["name"]));
    }
}
