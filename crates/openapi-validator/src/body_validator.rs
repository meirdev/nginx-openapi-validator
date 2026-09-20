use jsonschema::Validator;

use crate::error::{ValidationError, ValidationErrorKind};

/// Validate a JSON request body against a compiled schema.
pub fn validate_body(
    body: Option<&[u8]>,
    body_required: bool,
    schema_validator: Option<&Validator>,
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
            &mut errors,
        );
        assert!(!errors.is_empty());
    }

    #[test]
    fn test_missing_required_body() {
        let mut errors = Vec::new();
        validate_body(None, true, None, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::MissingRequiredBody);
    }

    #[test]
    fn test_missing_optional_body() {
        let mut errors = Vec::new();
        validate_body(None, false, None, &mut errors);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_invalid_json() {
        let mut errors = Vec::new();
        validate_body(Some(b"not json"), true, None, &mut errors);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ValidationErrorKind::InvalidBody);
    }
}
