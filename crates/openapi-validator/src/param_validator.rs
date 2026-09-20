use crate::compiled_spec::CompiledParam;
use crate::error::{ValidationError, ValidationErrorKind};

/// Parse a query string into key-value pairs.
pub fn parse_query_string(query: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Validate path parameters against their compiled schemas.
pub fn validate_path_params(
    captured: &[(&str, &str)],
    compiled_params: &[CompiledParam],
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let value = captured.iter().find(|(name, _)| *name == param.name);

        match value {
            Some((_, val)) => {
                validate_param_value(&param.name, val, param, "path", errors);
            }
            None if param.required => {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::MissingRequiredParam,
                    message: format!("Required path parameter '{}' is missing", param.name),
                    path: format!("path.{}", param.name),
                });
            }
            _ => {}
        }
    }
}

/// Validate query parameters against their compiled schemas.
pub fn validate_query_params(
    query_pairs: &[(String, String)],
    compiled_params: &[CompiledParam],
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let values: Vec<&str> = query_pairs
            .iter()
            .filter(|(k, _)| k == &param.name)
            .map(|(_, v)| v.as_str())
            .collect();

        if values.is_empty() {
            if param.required {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::MissingRequiredParam,
                    message: format!("Required query parameter '{}' is missing", param.name),
                    path: format!("query.{}", param.name),
                });
            }
            continue;
        }

        for val in &values {
            validate_param_value(&param.name, val, param, "query", errors);
        }
    }
}

/// Validate header parameters against their compiled schemas.
pub fn validate_header_params(
    headers: &[(String, String)],
    compiled_params: &[CompiledParam],
    errors: &mut Vec<ValidationError>,
) {
    for param in compiled_params {
        let lower_name = param.name.to_ascii_lowercase();
        let value = headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower_name)
            .map(|(_, v)| v.as_str());

        match value {
            Some(val) => {
                validate_param_value(&param.name, val, param, "header", errors);
            }
            None if param.required => {
                errors.push(ValidationError {
                    kind: ValidationErrorKind::MissingRequiredParam,
                    message: format!("Required header parameter '{}' is missing", param.name),
                    path: format!("header.{}", param.name),
                });
            }
            _ => {}
        }
    }
}

/// Validate a single parameter value against its compiled JSON schema.
fn validate_param_value(
    name: &str,
    value: &str,
    param: &CompiledParam,
    location: &str,
    errors: &mut Vec<ValidationError>,
) {
    let Some(validator) = &param.schema_validator else {
        return;
    };

    // Try to parse the value as JSON for schema validation.
    // If it doesn't parse as JSON, treat it as a string.
    let json_value = serde_json::from_str(value)
        .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));

    let validation_errors: Vec<String> = validator
        .iter_errors(&json_value)
        .map(|e| e.to_string())
        .collect();
    if !validation_errors.is_empty() {
        errors.push(ValidationError {
            kind: ValidationErrorKind::InvalidParamValue,
            message: format!(
                "Invalid value for {location} parameter '{name}': {}",
                validation_errors.join("; ")
            ),
            path: format!("{location}.{name}"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
