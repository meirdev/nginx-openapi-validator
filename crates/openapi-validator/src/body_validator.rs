use serde_json::Value;

use crate::compiled_spec::CompiledMediaType;
use crate::error::{ValidationError, ValidationErrorKind};
use crate::{form, xml};

/// How a request body is turned into a JSON value for schema validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Json,
    Form,
    Xml,
    /// A media type this crate cannot decode; only presence is checked.
    Opaque,
}

/// Validate a request body.
///
/// The body is decoded according to `kind` and checked against the matched
/// media type's schema. Opaque media types are only checked for presence.
pub fn validate_body(
    body: Option<&[u8]>,
    body_required: bool,
    media: Option<&CompiledMediaType>,
    kind: BodyKind,
    root: &Value,
    errors: &mut Vec<ValidationError>,
) {
    let raw = match body {
        None | Some(b"") if body_required => {
            errors.push(ValidationError {
                kind: ValidationErrorKind::MissingRequiredBody,
                message: "Request body is required but missing".to_string(),
                path: "body".to_string(),
            });
            return;
        }
        None => return,
        // An explicitly present but empty XML body is not a document.
        Some(b"") if kind == BodyKind::Xml => {
            errors.push(ValidationError {
                kind: ValidationErrorKind::InvalidBody,
                message: "Request body is empty; expected an XML document".to_string(),
                path: "body".to_string(),
            });
            return;
        }
        Some(b"") => return,
        Some(raw) => raw,
    };

    let decoded = match kind {
        BodyKind::Json => serde_json::from_slice::<Value>(raw).map_err(|e| {
            vec![ValidationError {
                kind: ValidationErrorKind::InvalidBody,
                message: format!("Failed to parse request body as JSON: {e}"),
                path: "body".to_string(),
            }]
        }),
        BodyKind::Form => form::decode(raw, media, root),
        BodyKind::Xml => xml::decode(raw, media, root),
        BodyKind::Opaque => return,
    };

    let value = match decoded {
        Ok(value) => value,
        Err(mut decode_errors) => {
            errors.append(&mut decode_errors);
            return;
        }
    };

    if let Some(validator) = media.and_then(|m| m.schema_validator.as_ref()) {
        for err in validator.iter_errors(&value) {
            errors.push(ValidationError {
                kind: ValidationErrorKind::SchemaValidation,
                message: err.to_string(),
                path: format!("body{}", err.instance_path()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media(schema: Value) -> CompiledMediaType {
        CompiledMediaType {
            media_type: "application/json".parse().unwrap(),
            schema_validator: Some(jsonschema::validator_for(&schema).unwrap()),
            schema: Some(schema),
            encoding: None,
        }
    }

    fn run(body: Option<&[u8]>, required: bool, media: Option<&CompiledMediaType>, kind: BodyKind) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        validate_body(body, required, media, kind, &Value::Null, &mut errors);
        errors
    }

    #[test]
    fn test_valid_body() {
        let m = media(json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}));
        assert!(run(Some(br#"{"name": "Alice"}"#), true, Some(&m), BodyKind::Json).is_empty());
    }

    #[test]
    fn test_invalid_body_schema() {
        let m = media(json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}));
        let errors = run(Some(br#"{"age": 25}"#), true, Some(&m), BodyKind::Json);
        assert_eq!(errors[0].kind, ValidationErrorKind::SchemaValidation);
        assert_eq!(errors[0].path, "body");
    }

    #[test]
    fn test_missing_required_body() {
        let errors = run(None, true, None, BodyKind::Json);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::MissingRequiredBody);
    }

    #[test]
    fn test_missing_optional_body() {
        assert!(run(None, false, None, BodyKind::Json).is_empty());
    }

    #[test]
    fn test_opaque_body_is_not_parsed() {
        let m = media(json!({"type": "object"}));
        assert!(run(Some(b"\x00\x01binary"), true, Some(&m), BodyKind::Opaque).is_empty());
    }

    #[test]
    fn test_invalid_json() {
        let errors = run(Some(b"not json"), true, None, BodyKind::Json);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::InvalidBody);
    }

    #[test]
    fn test_form_body_is_decoded_and_validated() {
        let m = media(json!({"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}}));
        assert!(run(Some(b"n=5"), true, Some(&m), BodyKind::Form).is_empty());
        assert!(!run(Some(b"n=x"), true, Some(&m), BodyKind::Form).is_empty());
        assert!(!run(Some(b"other=1"), true, Some(&m), BodyKind::Form).is_empty());
    }
}
