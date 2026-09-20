use serde_json::Value;

use crate::coerce;
use crate::compiled_spec::CompiledParam;
use crate::error::{ValidationError, ValidationErrorKind};

/// Parse a query string into key-value pairs.
///
/// Uses `application/x-www-form-urlencoded` rules, so `+` decodes to a space.
/// That matches what most OpenAPI tooling and web frameworks do with query
/// strings, even though RFC 3986 itself gives `+` no special meaning.
pub fn parse_query_string(query: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Validate path parameters against their compiled schemas.
pub fn validate_path_params(
    captured: &[(&str, &str)],
    compiled_params: &[CompiledParam],
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let value = captured.iter().find(|(name, _)| *name == param.name);
        validate_single(value.map(|(_, v)| *v), param, "path", root, errors);
    }
}

/// Validate query (or cookie) parameters against their compiled schemas.
///
/// Repeated keys are collected; for array parameters with `explode: true`
/// (the OpenAPI default for query and cookie) each repetition is one item.
pub fn validate_query_params(
    query_pairs: &[(String, String)],
    compiled_params: &[CompiledParam],
    location: &str,
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let values: Vec<&str> = query_pairs
            .iter()
            .filter(|(k, _)| k == &param.name)
            .map(|(_, v)| v.as_str())
            .collect();

        if values.is_empty() {
            report_missing(param, location, errors);
            continue;
        }

        if is_array_param(param, root) {
            let items: Vec<&str> = if param.explode {
                values
            } else {
                values[0].split(',').collect()
            };
            validate_value(coerce_array(&items, param, root), param, location, errors);
        } else {
            for val in values {
                validate_value(coerce_scalar(val, param, root), param, location, errors);
            }
        }
    }
}

/// Validate header parameters against their compiled schemas.
pub fn validate_header_params(
    headers: &[(String, String)],
    compiled_params: &[CompiledParam],
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let value = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&param.name))
            .map(|(_, v)| v.as_str());
        validate_single(value, param, "header", root, errors);
    }
}

/// Validate a parameter that carries at most one raw value (path, header).
/// Array parameters use the `simple` style: comma-separated items.
fn validate_single(
    value: Option<&str>,
    param: &CompiledParam,
    location: &str,
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    let Some(raw) = value else {
        report_missing(param, location, errors);
        return;
    };

    let json_value = if is_array_param(param, root) {
        let items: Vec<&str> = raw.split(',').collect();
        coerce_array(&items, param, root)
    } else {
        coerce_scalar(raw, param, root)
    };
    validate_value(json_value, param, location, errors);
}

fn report_missing(param: &CompiledParam, location: &str, errors: &mut Vec<ValidationError>) {
    if param.required {
        errors.push(ValidationError {
            kind: ValidationErrorKind::MissingRequiredParam,
            message: format!("Required {location} parameter '{}' is missing", param.name),
            path: format!("{location}.{}", param.name),
        });
    }
}

/// Validate an already-coerced value against the parameter's compiled schema.
fn validate_value(
    value: Value,
    param: &CompiledParam,
    location: &str,
    errors: &mut Vec<ValidationError>,
) {
    let Some(validator) = &param.schema_validator else {
        return;
    };

    let validation_errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    if !validation_errors.is_empty() {
        errors.push(ValidationError {
            kind: ValidationErrorKind::InvalidParamValue,
            message: format!(
                "Invalid value for {location} parameter '{}': {}",
                param.name,
                validation_errors.join("; ")
            ),
            path: format!("{location}.{}", param.name),
        });
    }
}

// ---------------------------------------------------------------------------
// Type coercion (see `coerce` for the rules)
// ---------------------------------------------------------------------------

fn is_array_param(param: &CompiledParam, root: &Value) -> bool {
    coerce::is_array_schema(root, param.schema.as_ref())
}

fn coerce_scalar(raw: &str, param: &CompiledParam, root: &Value) -> Value {
    coerce::coerce(raw, root, param.schema.as_ref())
}

fn coerce_array(items: &[&str], param: &CompiledParam, root: &Value) -> Value {
    let items_schema = coerce::items_schema(root, param.schema.as_ref());
    Value::Array(
        items
            .iter()
            .map(|item| coerce::coerce(item, root, items_schema))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(schema: &str, explode: bool) -> CompiledParam {
        let schema: Value = serde_json::from_str(schema).unwrap();
        CompiledParam {
            name: "p".to_string(),
            required: true,
            explode,
            schema_validator: Some(jsonschema::validator_for(&schema).unwrap()),
            schema: Some(schema),
        }
    }

    fn query_errors(qs: &str, p: &CompiledParam) -> Vec<ValidationError> {
        let pairs = parse_query_string(qs);
        let mut errors = Vec::new();
        validate_query_params(&pairs, std::slice::from_ref(p), "query", &Value::Null, &mut errors);
        errors
    }

    #[test]
    fn test_parse_query_string() {
        let pairs = parse_query_string("page=1&limit=10&q=hello+world");
        assert_eq!(
            pairs,
            vec![
                ("page".to_string(), "1".to_string()),
                ("limit".to_string(), "10".to_string()),
                ("q".to_string(), "hello world".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_query_string_encoded() {
        let pairs = parse_query_string("name=%E4%B8%AD%E6%96%87");
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "name");
    }

    #[test]
    fn test_parse_empty_query() {
        let pairs = parse_query_string("");
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_string_param_keeps_json_looking_values() {
        let p = param(r#"{"type": "string"}"#, true);
        for v in ["123", "true", "null", "[1]", "{\"a\":1}", "\"quoted\""] {
            assert!(query_errors(&format!("p={v}"), &p).is_empty(), "value {v} must be a valid string");
        }
    }

    #[test]
    fn test_integer_param() {
        let p = param(r#"{"type": "integer", "minimum": 1}"#, true);
        assert!(query_errors("p=5", &p).is_empty());
        assert!(!query_errors("p=0", &p).is_empty());
        assert!(!query_errors("p=abc", &p).is_empty());
        assert!(!query_errors("p=1.5", &p).is_empty());
    }

    #[test]
    fn test_number_and_boolean_params() {
        let n = param(r#"{"type": "number"}"#, true);
        assert!(query_errors("p=1.5", &n).is_empty());
        assert!(!query_errors("p=x", &n).is_empty());

        let b = param(r#"{"type": "boolean"}"#, true);
        assert!(query_errors("p=true", &b).is_empty());
        assert!(!query_errors("p=yes", &b).is_empty());
    }

    #[test]
    fn test_array_param_exploded() {
        let p = param(r#"{"type": "array", "items": {"type": "integer"}, "minItems": 2}"#, true);
        assert!(query_errors("p=1&p=2", &p).is_empty());
        assert!(!query_errors("p=1", &p).is_empty());
        assert!(!query_errors("p=1&p=x", &p).is_empty());
    }

    #[test]
    fn test_array_param_comma_separated() {
        let p = param(r#"{"type": "array", "items": {"type": "integer"}, "minItems": 2}"#, false);
        assert!(query_errors("p=1,2", &p).is_empty());
        assert!(!query_errors("p=1", &p).is_empty());
    }

    #[test]
    fn test_nullable_type_list() {
        let p = param(r#"{"type": ["integer", "null"]}"#, true);
        assert!(query_errors("p=", &p).is_empty());
        assert!(query_errors("p=3", &p).is_empty());
    }

    #[test]
    fn test_path_param_coercion() {
        let p = param(r#"{"type": "integer"}"#, false);
        let mut errors = Vec::new();
        validate_path_params(&[("p", "42")], std::slice::from_ref(&p), &Value::Null, &mut errors);
        assert!(errors.is_empty());
        validate_path_params(&[("p", "x")], std::slice::from_ref(&p), &Value::Null, &mut errors);
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn test_header_param_case_insensitive() {
        let p = param(r#"{"type": "string", "minLength": 1}"#, false);
        let headers = vec![("x-p".to_string(), "v".to_string())];
        let mut errors = Vec::new();
        let mut p = p;
        p.name = "X-P".to_string();
        validate_header_params(&headers, std::slice::from_ref(&p), &Value::Null, &mut errors);
        assert!(errors.is_empty());
    }
}
