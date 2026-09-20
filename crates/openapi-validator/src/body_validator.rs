use jsonschema::Validator;

use crate::error::{ValidationError, ValidationErrorKind};

/// Validate a request body.
///
/// The body is parsed and checked against the schema only when `parse_as_json`
/// is set, i.e. the request's media type is JSON. Other media types are only
/// checked for presence, since form and multipart bodies have no JSON
/// representation to validate.
pub fn validate_body(
    body: Option<&[u8]>,
    body_required: bool,
    schema_validator: Option<&Validator>,
    parse_as_json: bool,
    errors: &mut Vec<ValidationError>,
) {
    match body {
        None | Some(b"") => {
            if body_required {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::MissingRequiredBody,
                    message: "Request body is required but missing".to_string(),
                    path: "body".to_string(),
                });
            }
        }
        Some(_) if !parse_as_json => {}
        Some(raw) => {
            let json_value: serde_json::Value = match serde_json::from_slice(raw) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(ValidationError {
                        kind: ValidationErrorKind::InvalidBody,
                        message: format!("Failed to parse request body as JSON: {e}"),
                        path: "body".to_string(),
                    });
                    return;
                }
            };

            if let Some(validator) = schema_validator {
                for err in validator.iter_errors(&json_value) {
                    errors.push(ValidationError {
                        kind: ValidationErrorKind::SchemaValidation,
                        message: err.to_string(),
                        path: format!("body{}", err.instance_path()),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_validator(schema_json: &str) -> Validator {
        let schema: serde_json::Value = serde_json::from_str(schema_json).unwrap();
        jsonschema::validator_for(&schema).unwrap()
    }

    #[test]
    fn test_valid_body() {
        let validator = make_validator(
            r#"{"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}"#,
        );
        let mut errors = Vec::new();
        validate_body(
            Some(br#"{"name": "Alice"}"#),
            true,
            Some(&validator),
            true,
            &mut errors,
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn test_invalid_body_schema() {
        let validator = make_validator(
            r#"{"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}"#,
        );
        let mut errors = Vec::new();
        validate_body(
            Some(br#"{"age": 25}"#),
            true,
            Some(&validator),
            true,
            &mut errors,
        );
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_missing_required_body() {
        let mut errors = Vec::new();
        validate_body(None, true, None, true, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::MissingRequiredBody);
    }

    #[test]
    fn test_missing_optional_body() {
        let mut errors = Vec::new();
        validate_body(None, false, None, true, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_non_json_body_is_not_parsed() {
        let validator = make_validator(r#"{"type": "object"}"#);
        let mut errors = Vec::new();
        validate_body(Some(b"a=1&b=2"), true, Some(&validator), false, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_invalid_json() {
        let mut errors = Vec::new();
        validate_body(Some(b"not json"), true, None, true, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::InvalidBody);
    }
}
