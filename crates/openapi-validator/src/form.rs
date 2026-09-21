//! Decoding of `application/x-www-form-urlencoded` bodies into JSON, driven
//! by the media type's schema and `encoding` object.

use serde_json::{Map, Value};

use crate::coerce::{self, deref};
use crate::compiled_spec::CompiledMediaType;
use crate::error::{ValidationError, ValidationErrorKind};

/// Deepest bracket nesting accepted in a field name such as `a[b][c]`.
/// Bounds recursion in [`insert_path`] and [`navigate`].
pub(crate) const MAX_NESTING: usize = 32;

/// RFC 3986 reserved characters, which a field may only contain unencoded
/// when its encoding sets `allowReserved: true`.
const RESERVED: &[char] = &[
    ':', '/', '?', '#', '[', ']', '@', '!', '$', '&', '\'', '(', ')', '*', '+', ',', ';', '=',
];

/// Decode a form body into a JSON object suitable for schema validation.
pub fn decode(
    raw: &[u8],
    media: Option<&CompiledMediaType>,
    root: &Value,
) -> Result<Value, Vec<ValidationError>> {
    let text = std::str::from_utf8(raw).map_err(|_| {
        vec![invalid_body(
            "body",
            "Form body is not valid UTF-8".to_string(),
        )]
    })?;

    let schema = media.and_then(|m| m.schema.as_ref()).map(|s| deref(root, s));
    let encoding = media.and_then(|m| m.encoding.as_ref());

    // Group raw (still encoded) values by top-level field name.
    let mut fields: Vec<(String, Vec<(Vec<String>, String)>)> = Vec::new();
    for pair in text.split('&').filter(|p| !p.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(raw_key)
            .map_err(|e| vec![invalid_body("body", format!("Malformed URL encoding in field name '{raw_key}': {e}"))])?;
        let (top, path) = split_bracket_path(&key);
        if path.len() > MAX_NESTING {
            return Err(vec![invalid_body(
                "body",
                format!("Field '{top}' is nested deeper than {MAX_NESTING} levels"),
            )]);
        }
        match fields.iter_mut().find(|(name, _)| *name == top) {
            Some((_, values)) => values.push((path, raw_value.to_string())),
            None => fields.push((top, vec![(path, raw_value.to_string())])),
        }
    }

    let mut errors = Vec::new();
    let mut object = Map::new();

    for (name, occurrences) in fields {
        let field_schema = coerce::property_schema(root, schema, &name);
        let field_encoding = encoding.and_then(|e| e.get(&name));
        let field_path = format!("body/{name}");

        match decode_field(&name, &occurrences, field_schema, field_encoding, root) {
            Ok(value) => {
                object.insert(name, value);
            }
            Err(e) => errors.push(match e {
                FieldError::Encoding(msg) => invalid_body(&field_path, msg),
                FieldError::Reserved => ValidationError {
                    kind: ValidationErrorKind::SchemaValidation,
                    message: format!(
                        "Field '{name}' contains reserved characters but its encoding does not set allowReserved"
                    ),
                    path: field_path,
                },
            }),
        }
    }

    if errors.is_empty() {
        Ok(Value::Object(object))
    } else {
        Err(errors)
    }
}

enum FieldError {
    Encoding(String),
    Reserved,
}

fn decode_field(
    name: &str,
    occurrences: &[(Vec<String>, String)],
    schema: Option<&Value>,
    encoding: Option<&Value>,
    root: &Value,
) -> Result<Value, FieldError> {
    let content_type = encoding
        .and_then(|e| e.get("contentType"))
        .and_then(Value::as_str);
    let allow_reserved = encoding
        .and_then(|e| e.get("allowReserved"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let style = encoding
        .and_then(|e| e.get("style"))
        .and_then(Value::as_str)
        .unwrap_or("form");
    let explode = encoding
        .and_then(|e| e.get("explode"))
        .and_then(Value::as_bool)
        .unwrap_or(style == "form");

    // A field with its own content type is an embedded document.
    if let Some(content_type) = content_type {
        let raw = &occurrences.last().map(|(_, v)| v.as_str()).unwrap_or("");
        let decoded = percent_decode(raw).map_err(FieldError::Encoding)?;
        return if is_json_content_type(content_type) {
            serde_json::from_str(&decoded)
                .map_err(|e| FieldError::Encoding(format!("Field '{name}' is not valid JSON: {e}")))
        } else {
            Ok(Value::String(decoded))
        };
    }

    // Bracketed keys (`content[0][name]`) build a nested structure.
    if occurrences.iter().any(|(path, _)| !path.is_empty()) {
        let mut value = Value::Null;
        for (path, raw) in occurrences {
            let leaf_schema = navigate(root, schema, path);
            let item = decode_scalar(raw, leaf_schema, allow_reserved, root)?;
            insert_path(&mut value, path, item).map_err(FieldError::Encoding)?;
        }
        return Ok(value);
    }

    let is_array = coerce::is_array_schema(root, schema);
    let items_schema = coerce::items_schema(root, schema);

    if is_array {
        // The delimiter separating items, if the style packs several items
        // into one value. It is exempt from the reserved-character rule.
        let delimiter = match style {
            "pipeDelimited" => Some('|'),
            "spaceDelimited" => Some(' '),
            _ if !explode => Some(','),
            _ => None,
        };
        let mut items = Vec::new();
        for (_, raw) in occurrences {
            let decoded = decode_string(raw, allow_reserved, delimiter)?;
            match delimiter {
                Some(d) => {
                    for piece in decoded.split(d) {
                        items.push(coerce::coerce(piece, root, items_schema));
                    }
                }
                None => items.push(coerce::coerce(&decoded, root, items_schema)),
            }
        }
        return Ok(Value::Array(items));
    }

    let mut values = Vec::with_capacity(occurrences.len());
    for (_, raw) in occurrences {
        values.push(decode_scalar(raw, schema, allow_reserved, root)?);
    }
    Ok(if values.len() == 1 {
        values.pop().unwrap()
    } else {
        // Repeated scalar field: hand the validator an array so the type
        // mismatch is reported.
        Value::Array(values)
    })
}

fn decode_scalar(
    raw: &str,
    schema: Option<&Value>,
    allow_reserved: bool,
    root: &Value,
) -> Result<Value, FieldError> {
    let decoded = decode_string(raw, allow_reserved, None)?;
    Ok(coerce::coerce(&decoded, root, schema))
}

/// Percent-decode a raw field value after checking the reserved-character
/// rule on its still-encoded form. `delimiter` is a style delimiter that is
/// allowed even though it is a reserved character.
fn decode_string(
    raw: &str,
    allow_reserved: bool,
    delimiter: Option<char>,
) -> Result<String, FieldError> {
    if !allow_reserved
        && raw
            .chars()
            .any(|c| RESERVED.contains(&c) && Some(c) != delimiter)
    {
        return Err(FieldError::Reserved);
    }
    percent_decode(raw).map_err(FieldError::Encoding)
}

fn is_json_content_type(content_type: &str) -> bool {
    content_type
        .parse::<mime::Mime>()
        .map(|m| crate::content_type::is_json(&m))
        .unwrap_or(false)
}

/// Walk a schema along a bracket path: numeric segments select `items`,
/// others select `properties`.
pub(crate) fn navigate<'a>(root: &'a Value, schema: Option<&'a Value>, path: &[String]) -> Option<&'a Value> {
    let mut current = schema;
    for segment in path {
        current = if segment.is_empty() || segment.chars().all(|c| c.is_ascii_digit()) {
            coerce::items_schema(root, current)
        } else {
            coerce::property_schema(root, current, segment)
        };
        current?;
    }
    current
}

/// `content[0][name]` → (`content`, [`0`, `name`]).
pub(crate) fn split_bracket_path(key: &str) -> (String, Vec<String>) {
    let Some(open) = key.find('[') else {
        return (key.to_string(), Vec::new());
    };
    let top = key[..open].to_string();
    let mut path = Vec::new();
    let mut rest = &key[open..];
    while let Some(stripped) = rest.strip_prefix('[') {
        match stripped.find(']') {
            Some(close) => {
                path.push(stripped[..close].to_string());
                rest = &stripped[close + 1..];
            }
            None => {
                path.push(stripped.to_string());
                break;
            }
        }
    }
    (top, path)
}

/// Insert `leaf` at `path` inside `target`, creating arrays for numeric or
/// empty segments and objects otherwise.
///
/// Array indexes must be dense: an index may address an existing item or
/// append one, but never skip ahead. Otherwise a key like `a[99999]` could
/// make the decoder allocate arbitrarily large arrays.
pub(crate) fn insert_path(target: &mut Value, path: &[String], leaf: Value) -> Result<(), String> {
    if path.len() > MAX_NESTING {
        return Err(format!("nesting deeper than {MAX_NESTING} levels"));
    }
    let Some((segment, rest)) = path.split_first() else {
        *target = leaf;
        return Ok(());
    };

    if segment.is_empty() || segment.chars().all(|c| c.is_ascii_digit()) {
        if !target.is_array() {
            *target = Value::Array(Vec::new());
        }
        let items = target.as_array_mut().unwrap();
        let index = if segment.is_empty() {
            items.len()
        } else {
            segment
                .parse::<usize>()
                .map_err(|_| format!("invalid array index '{segment}'"))?
        };
        if index > items.len() {
            return Err(format!(
                "array index {index} skips ahead of the {} items received so far",
                items.len()
            ));
        }
        if index == items.len() {
            items.push(Value::Null);
        }
        insert_path(&mut items[index], rest, leaf)
    } else {
        if !target.is_object() {
            *target = Value::Object(Map::new());
        }
        let entry = target
            .as_object_mut()
            .unwrap()
            .entry(segment.clone())
            .or_insert(Value::Null);
        insert_path(entry, rest, leaf)
    }
}

/// Strict percent-decoding: every `%` must be followed by two hex digits and
/// the result must be UTF-8. `+` decodes to a space.
fn percent_decode(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or_else(|| format!("invalid percent-escape at byte {i}"))?;
                out.push(hex);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "decoded value is not valid UTF-8".to_string())
}

fn invalid_body(path: &str, message: String) -> ValidationError {
    ValidationError {
        kind: ValidationErrorKind::InvalidBody,
        message,
        path: path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bracket_paths() {
        assert_eq!(split_bracket_path("a"), ("a".into(), vec![]));
        assert_eq!(
            split_bracket_path("content[0][name]"),
            ("content".into(), vec!["0".into(), "name".into()])
        );
        assert_eq!(split_bracket_path("x[]"), ("x".into(), vec!["".into()]));
    }

    #[test]
    fn nested_insert() {
        let mut v = Value::Null;
        insert_path(&mut v, &["0".into(), "name".into()], json!(true)).unwrap();
        insert_path(&mut v, &["0".into(), "age".into()], json!(4)).unwrap();
        insert_path(&mut v, &["1".into(), "name".into()], json!(false)).unwrap();
        insert_path(&mut v, &["".into()], json!("appended")).unwrap();
        assert_eq!(v, json!([{"name": true, "age": 4}, {"name": false}, "appended"]));
    }

    #[test]
    fn sparse_index_and_deep_nesting_are_rejected() {
        let mut v = Value::Null;
        assert!(insert_path(&mut v, &["99999".into()], json!(1)).is_err());
        let deep: Vec<String> = (0..=MAX_NESTING).map(|i| i.to_string()).collect();
        assert!(insert_path(&mut v, &deep, json!(1)).is_err());

        let sparse = decode(b"a[5]=1", None, &Value::Null).unwrap_err();
        assert_eq!(sparse[0].kind, ValidationErrorKind::InvalidBody);
        let key = format!("a{}=1", "[b]".repeat(MAX_NESTING + 1));
        assert!(decode(key.as_bytes(), None, &Value::Null).is_err());
    }

    #[test]
    fn strict_percent_decoding() {
        assert_eq!(percent_decode("a+b%20c").unwrap(), "a b c");
        assert!(percent_decode("%zz").is_err());
        assert!(percent_decode("%2").is_err());
    }

    #[test]
    fn decode_without_schema_keeps_strings_and_repeats() {
        let v = decode(b"a=1&b=x&b=y", None, &Value::Null).unwrap();
        assert_eq!(v, json!({"a": 1, "b": ["x", "y"]}));
    }
}
