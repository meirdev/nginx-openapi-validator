//! Parameter decoding and validation.
//!
//! Parameters arrive as strings serialized according to their OpenAPI
//! `style` and `explode`. Each location's validator decodes the raw text into
//! the JSON value the schema describes (scalar, array or object), coerces
//! scalars by declared type, then runs the compiled schema validator.

use serde_json::{Map, Value};

use crate::coerce::{self, deref};
use crate::compiled_spec::{CompiledParam, ParamStyle};
use crate::error::{ValidationError, ValidationErrorKind};
use crate::form;

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

/// Validate path parameters captured from the route template.
pub fn validate_path_params(
    captured: &[(&str, &str)],
    compiled_params: &[CompiledParam],
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let raw = captured
            .iter()
            .find(|(name, _)| *name == param.name)
            .map(|(_, v)| *v);
        match raw {
            Some(raw) => validate_value(decode_path(raw, param, root), param, "path", errors),
            None => report_missing(param, "path", errors),
        }
    }
}

/// Validate query parameters.
pub fn validate_query_params(
    query_pairs: &[(String, String)],
    compiled_params: &[CompiledParam],
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        match decode_query(query_pairs, param, root) {
            Ok(Some(values)) => {
                for value in values {
                    validate_value(value, param, "query", errors);
                }
            }
            Ok(None) => report_missing(param, "query", errors),
            Err(message) => errors.push(ValidationError {
                kind: ValidationErrorKind::InvalidParamValue,
                message: format!("Invalid value for query parameter '{}': {message}", param.name),
                path: format!("query.{}", param.name),
            }),
        }
    }
}

/// Validate header parameters (`simple` style).
pub fn validate_header_params(
    headers: &[(String, String)],
    compiled_params: &[CompiledParam],
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let raw = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&param.name))
            .map(|(_, v)| v.as_str());
        match raw {
            Some(raw) => {
                let value = decode_simple(raw, param.explode, param.schema.as_ref(), root);
                validate_value(value, param, "header", errors);
            }
            None => report_missing(param, "header", errors),
        }
    }
}

/// Validate cookie parameters (`form` style, one cookie per parameter).
pub fn validate_cookie_params(
    cookies: &[(String, String)],
    compiled_params: &[CompiledParam],
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let raw = cookies
            .iter()
            .find(|(k, _)| *k == param.name)
            .map(|(_, v)| v.as_str());
        match raw {
            Some(raw) => {
                // Cookie values containing commas or spaces are commonly
                // sent quoted (RFC 6265 cookie-value).
                let raw = raw
                    .strip_prefix('"')
                    .and_then(|r| r.strip_suffix('"'))
                    .unwrap_or(raw);
                let value = decode_delimited(
                    raw,
                    ',',
                    param.explode == Some(true),
                    param.schema.as_ref(),
                    root,
                );
                validate_value(value, param, "cookie", errors);
            }
            None => report_missing(param, "cookie", errors),
        }
    }
}

/// Whether a query-string key is carried by this parameter, given its style.
/// Used to decide which keys are "undeclared" in strict mode.
pub fn query_key_belongs_to(key: &str, param: &CompiledParam, root: &Value) -> bool {
    if key == param.name {
        return true;
    }
    match param.style {
        ParamStyle::DeepObject => key.starts_with(&format!("{}[", param.name)),
        ParamStyle::Form if param.explode != Some(false) => {
            object_properties(param.schema.as_ref(), root).is_some_and(|p| p.contains_key(key))
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Decoding by style
// ---------------------------------------------------------------------------

/// `simple` style (path and header): `a,b,c` for arrays, `k,v,k,v` for
/// objects, or `k=v,k=v` when exploded.
fn decode_simple(raw: &str, explode: Option<bool>, schema: Option<&Value>, root: &Value) -> Value {
    decode_delimited(raw, ',', explode == Some(true), schema, root)
}

fn decode_path(raw: &str, param: &CompiledParam, root: &Value) -> Value {
    let schema = param.schema.as_ref();
    match param.style {
        ParamStyle::Label => {
            // `.value`; exploded arrays and objects use `.` between items.
            let body = raw.strip_prefix('.').unwrap_or(raw);
            let exploded = param.explode == Some(true);
            decode_delimited(body, if exploded { '.' } else { ',' }, exploded, schema, root)
        }
        ParamStyle::Matrix => {
            // `;name=value`; exploded forms repeat `;name=v` or list `;k=v`.
            let body = raw.strip_prefix(';').unwrap_or(raw);
            if param.explode == Some(true) {
                let pairs: Vec<(&str, &str)> = body
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.split_once('=').unwrap_or((s, "")))
                    .collect();
                if coerce::is_object_schema(root, schema) {
                    object_from_pairs(&pairs, schema, root)
                } else if coerce::is_array_schema(root, schema) {
                    let items_schema = coerce::items_schema(root, schema);
                    Value::Array(
                        pairs
                            .iter()
                            .filter(|(k, _)| *k == param.name)
                            .map(|(_, v)| coerce::coerce(v, root, items_schema))
                            .collect(),
                    )
                } else {
                    let value = pairs
                        .iter()
                        .find(|(k, _)| *k == param.name)
                        .map(|(_, v)| *v)
                        .unwrap_or(body);
                    coerce::coerce(value, root, schema)
                }
            } else {
                let value = body
                    .strip_prefix(&format!("{}=", param.name))
                    .unwrap_or(body);
                decode_delimited(value, ',', false, schema, root)
            }
        }
        _ => decode_simple(raw, param.explode, schema, root),
    }
}

/// Decode a query parameter. `Ok(None)` means no key carries it; otherwise
/// every value to validate (scalars may repeat). `Err` reports a value that
/// contradicts the declared serialization style.
fn decode_query(
    pairs: &[(String, String)],
    param: &CompiledParam,
    root: &Value,
) -> Result<Option<Vec<Value>>, String> {
    let schema = param.schema.as_ref();

    if param.style == ParamStyle::DeepObject {
        let prefix = format!("{}[", param.name);
        let mut value = Value::Null;
        let mut found = false;
        for (key, raw) in pairs {
            if !key.starts_with(&prefix) {
                continue;
            }
            found = true;
            let (_, path) = form::split_bracket_path(key);
            let leaf_schema = form::navigate(root, schema, &path);
            form::insert_path(&mut value, &path, coerce::coerce(raw, root, leaf_schema));
        }
        return Ok(found.then_some(vec![value]));
    }

    let delimiter = match param.style {
        ParamStyle::SpaceDelimited => ' ',
        ParamStyle::PipeDelimited => '|',
        _ => ',',
    };
    let values: Vec<&str> = pairs
        .iter()
        .filter(|(k, _)| *k == param.name)
        .map(|(_, v)| v.as_str())
        .collect();

    if coerce::is_object_schema(root, schema) {
        // Exploded form objects spread their properties as separate keys.
        if param.style == ParamStyle::Form && param.explode != Some(false) {
            if let Some(properties) = object_properties(schema, root) {
                let mut object = Map::new();
                for (key, raw) in pairs {
                    if let Some(prop_schema) = properties.get(key) {
                        object.insert(
                            key.clone(),
                            coerce::coerce(raw, root, Some(deref(root, prop_schema))),
                        );
                    }
                }
                if !object.is_empty() {
                    return Ok(Some(vec![Value::Object(object)]));
                }
            }
        }
        let Some(first) = values.first() else {
            return Ok(None);
        };
        return Ok(Some(vec![decode_delimited(first, delimiter, false, schema, root)]));
    }

    if coerce::is_array_schema(root, schema) {
        if values.is_empty() {
            return Ok(None);
        }
        // With `explode: true` each key repetition is one item, so a
        // comma-separated list is a serialization error. When `explode` is
        // omitted, comma-separated values are accepted as well, matching the
        // reference implementation.
        let split = !(param.style == ParamStyle::Form && param.explode == Some(true));
        if !split {
            if let Some(bad) = values.iter().find(|v| v.contains(',')) {
                return Err(format!(
                    "parameter is declared with explode=true (one item per key) but '{bad}' is a comma-separated list"
                ));
            }
        }
        let items_schema = coerce::items_schema(root, schema);
        let items = values
            .iter()
            .flat_map(|v| {
                if split {
                    v.split(delimiter).collect::<Vec<_>>()
                } else {
                    vec![*v]
                }
            })
            .map(|item| coerce::coerce(item, root, items_schema))
            .collect();
        return Ok(Some(vec![Value::Array(items)]));
    }

    if values.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        values
            .iter()
            .map(|v| coerce::coerce(v, root, schema))
            .collect(),
    ))
}

/// Decode a single delimited value: arrays split on the delimiter, objects
/// alternate `k,v` pairs (or `k=v` items when `kv_exploded`), scalars coerce.
fn decode_delimited(
    raw: &str,
    delimiter: char,
    kv_exploded: bool,
    schema: Option<&Value>,
    root: &Value,
) -> Value {
    if coerce::is_object_schema(root, schema) {
        let parts: Vec<&str> = raw.split(delimiter).collect();
        let pairs: Vec<(&str, &str)> = if kv_exploded {
            if !parts.iter().all(|p| p.contains('=')) {
                // Not `k=v` items: leave it a string so the type check fails.
                return Value::String(raw.to_string());
            }
            parts
                .iter()
                .map(|p| p.split_once('=').unwrap_or((p, "")))
                .collect()
        } else {
            if parts.len() % 2 != 0 {
                // Alternating `k,v` needs an even count; otherwise this is
                // not an object serialization.
                return Value::String(raw.to_string());
            }
            parts.chunks(2).map(|c| (c[0], c[1])).collect()
        };
        object_from_pairs(&pairs, schema, root)
    } else if coerce::is_array_schema(root, schema) {
        let items_schema = coerce::items_schema(root, schema);
        Value::Array(
            raw.split(delimiter)
                .map(|item| coerce::coerce(item, root, items_schema))
                .collect(),
        )
    } else {
        coerce::coerce(raw, root, schema)
    }
}

fn object_from_pairs(pairs: &[(&str, &str)], schema: Option<&Value>, root: &Value) -> Value {
    let properties = object_properties(schema, root);
    let mut object = Map::new();
    for (key, raw) in pairs {
        let prop_schema = properties
            .and_then(|p| p.get(*key))
            .map(|s| deref(root, s));
        object.insert(key.to_string(), coerce::coerce(raw, root, prop_schema));
    }
    Value::Object(object)
}

fn object_properties<'a>(
    schema: Option<&'a Value>,
    root: &'a Value,
) -> Option<&'a Map<String, Value>> {
    schema
        .map(|s| deref(root, s))
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn report_missing(param: &CompiledParam, location: &str, errors: &mut Vec<ValidationError>) {
    if param.required {
        errors.push(ValidationError {
            kind: ValidationErrorKind::MissingRequiredParam,
            message: format!("Required {location} parameter '{}' is missing", param.name),
            path: format!("{location}.{}", param.name),
        });
    }
}

/// Validate an already-decoded value against the parameter's compiled schema.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiled_spec::ParamLocation;

    fn param(schema: &str, explode: Option<bool>) -> CompiledParam {
        styled(schema, explode, ParamStyle::Form)
    }

    fn styled(schema: &str, explode: Option<bool>, style: ParamStyle) -> CompiledParam {
        let schema: Value = serde_json::from_str(schema).unwrap();
        CompiledParam {
            name: "p".to_string(),
            location: ParamLocation::Query,
            required: true,
            style,
            explode,
            schema_validator: Some(jsonschema::validator_for(&schema).unwrap()),
            schema: Some(schema),
        }
    }

    fn query_errors(qs: &str, p: &CompiledParam) -> Vec<ValidationError> {
        let pairs = parse_query_string(qs);
        let mut errors = Vec::new();
        validate_query_params(&pairs, std::slice::from_ref(p), &Value::Null, &mut errors);
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
        assert!(parse_query_string("").is_empty());
    }

    #[test]
    fn test_string_param_keeps_json_looking_values() {
        let p = param(r#"{"type": "string"}"#, None);
        for v in ["123", "true", "null", "[1]", "{\"a\":1}", "\"quoted\""] {
            assert!(query_errors(&format!("p={v}"), &p).is_empty(), "value {v} must be a valid string");
        }
    }

    #[test]
    fn test_integer_param() {
        let p = param(r#"{"type": "integer", "minimum": 1}"#, None);
        assert!(query_errors("p=5", &p).is_empty());
        assert!(!query_errors("p=0", &p).is_empty());
        assert!(!query_errors("p=abc", &p).is_empty());
        assert!(!query_errors("p=1.5", &p).is_empty());
    }

    #[test]
    fn test_number_and_boolean_params() {
        let n = param(r#"{"type": "number"}"#, None);
        assert!(query_errors("p=1.5", &n).is_empty());
        assert!(!query_errors("p=x", &n).is_empty());

        let b = param(r#"{"type": "boolean"}"#, None);
        assert!(query_errors("p=true", &b).is_empty());
        assert!(!query_errors("p=yes", &b).is_empty());
    }

    #[test]
    fn test_array_param_exploded() {
        let p = param(r#"{"type": "array", "items": {"type": "integer"}, "minItems": 2}"#, Some(true));
        assert!(query_errors("p=1&p=2", &p).is_empty());
        assert!(!query_errors("p=1", &p).is_empty());
        assert!(!query_errors("p=1&p=x", &p).is_empty());
        // Explicit explode: commas are literal, so this is one bad item.
        assert!(!query_errors("p=1,2", &p).is_empty());
    }

    #[test]
    fn test_array_param_comma_separated() {
        let p = param(r#"{"type": "array", "items": {"type": "integer"}, "minItems": 2}"#, Some(false));
        assert!(query_errors("p=1,2", &p).is_empty());
        assert!(!query_errors("p=1", &p).is_empty());
        // Omitted explode accepts both conventions.
        let p = param(r#"{"type": "array", "items": {"type": "integer"}, "minItems": 2}"#, None);
        assert!(query_errors("p=1,2", &p).is_empty());
        assert!(query_errors("p=1&p=2", &p).is_empty());
    }

    #[test]
    fn test_delimited_styles() {
        let schema = r#"{"type": "array", "items": {"type": "integer"}, "minItems": 3}"#;
        assert!(query_errors("p=1|2|3", &styled(schema, None, ParamStyle::PipeDelimited)).is_empty());
        assert!(query_errors("p=1%202%203", &styled(schema, None, ParamStyle::SpaceDelimited)).is_empty());
        let obj = r#"{"type": "object", "required": ["a", "b"], "properties": {"a": {"type": "integer"}, "b": {"type": "boolean"}}}"#;
        assert!(query_errors("p=a|1|b|true", &styled(obj, None, ParamStyle::PipeDelimited)).is_empty());
    }

    #[test]
    fn test_form_objects() {
        let obj = r#"{"type": "object", "required": ["a", "b"], "properties": {"a": {"type": "integer"}, "b": {"type": "boolean"}}}"#;
        assert!(query_errors("a=1&b=true", &param(obj, None)).is_empty());
        assert!(query_errors("p=a,1,b,true", &param(obj, Some(false))).is_empty());
        assert!(!query_errors("a=x&b=true", &param(obj, None)).is_empty());
    }

    #[test]
    fn test_deep_object() {
        let obj = r#"{"type": "object", "required": ["a"], "properties": {"a": {"type": "integer"}, "n": {"type": "object", "properties": {"c": {"type": "boolean"}}}}}"#;
        let p = styled(obj, None, ParamStyle::DeepObject);
        assert!(query_errors("p[a]=1&p[n][c]=true", &p).is_empty());
        assert!(!query_errors("p[a]=x", &p).is_empty());
        assert_eq!(query_errors("other=1", &p)[0].kind, ValidationErrorKind::MissingRequiredParam);
        assert!(query_key_belongs_to("p[a]", &p, &Value::Null));
        assert!(!query_key_belongs_to("q", &p, &Value::Null));
    }

    #[test]
    fn test_nullable_type_list() {
        let p = param(r#"{"type": ["integer", "null"]}"#, None);
        assert!(query_errors("p=", &p).is_empty());
        assert!(query_errors("p=3", &p).is_empty());
    }

    fn path_errors(raw: &str, p: &CompiledParam) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        validate_path_params(&[("p", raw)], std::slice::from_ref(p), &Value::Null, &mut errors);
        errors
    }

    #[test]
    fn test_path_styles() {
        let int = r#"{"type": "integer"}"#;
        assert!(path_errors("42", &styled(int, None, ParamStyle::Simple)).is_empty());
        assert!(!path_errors("x", &styled(int, None, ParamStyle::Simple)).is_empty());
        assert!(path_errors(".42", &styled(int, None, ParamStyle::Label)).is_empty());
        assert!(path_errors(";p=42", &styled(int, None, ParamStyle::Matrix)).is_empty());

        let arr = r#"{"type": "array", "items": {"type": "integer"}, "minItems": 3}"#;
        assert!(path_errors("1,2,3", &styled(arr, None, ParamStyle::Simple)).is_empty());
        assert!(path_errors(".1,2,3", &styled(arr, None, ParamStyle::Label)).is_empty());
        assert!(path_errors(".1.2.3", &styled(arr, Some(true), ParamStyle::Label)).is_empty());
        assert!(path_errors(";p=1,2,3", &styled(arr, None, ParamStyle::Matrix)).is_empty());
        assert!(path_errors(";p=1;p=2;p=3", &styled(arr, Some(true), ParamStyle::Matrix)).is_empty());

        let obj = r#"{"type": "object", "required": ["a", "b"], "properties": {"a": {"type": "integer"}, "b": {"type": "boolean"}}}"#;
        assert!(path_errors("a,1,b,true", &styled(obj, None, ParamStyle::Simple)).is_empty());
        assert!(path_errors("a=1,b=true", &styled(obj, Some(true), ParamStyle::Simple)).is_empty());
        assert!(path_errors(".a,1,b,true", &styled(obj, None, ParamStyle::Label)).is_empty());
        assert!(path_errors(".a=1.b=true", &styled(obj, Some(true), ParamStyle::Label)).is_empty());
        assert!(path_errors(";p=a,1,b,true", &styled(obj, None, ParamStyle::Matrix)).is_empty());
        assert!(path_errors(";a=1;b=true", &styled(obj, Some(true), ParamStyle::Matrix)).is_empty());
        assert!(!path_errors(";a=x;b=true", &styled(obj, Some(true), ParamStyle::Matrix)).is_empty());
    }

    #[test]
    fn test_malformed_object_serializations_fail_type_check() {
        let obj = r#"{"type": "object", "properties": {"a": {"type": "integer"}}}"#;
        assert!(!path_errors("I am not an object", &styled(obj, None, ParamStyle::Simple)).is_empty());
        assert!(!path_errors("a,1,b", &styled(obj, None, ParamStyle::Simple)).is_empty());
        assert!(!path_errors("a=1,b", &styled(obj, Some(true), ParamStyle::Simple)).is_empty());
    }

    #[test]
    fn test_header_and_cookie_objects() {
        let obj = r#"{"type": "object", "required": ["a", "b"], "properties": {"a": {"type": "integer"}, "b": {"type": "boolean"}}}"#;
        let mut p = styled(obj, None, ParamStyle::Simple);
        p.name = "X-P".to_string();
        let headers = vec![("x-p".to_string(), "a,1,b,true".to_string())];
        let mut errors = Vec::new();
        validate_header_params(&headers, std::slice::from_ref(&p), &Value::Null, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");

        let arr = r#"{"type": "array", "items": {"type": "boolean"}, "minItems": 2}"#;
        let p = styled(arr, None, ParamStyle::Form);
        let cookies = vec![("p".to_string(), "\"true,false\"".to_string())];
        let mut errors = Vec::new();
        validate_cookie_params(&cookies, std::slice::from_ref(&p), &Value::Null, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }
}
