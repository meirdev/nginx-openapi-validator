pub mod body_validator;
pub mod compiled_spec;
pub mod content_type;
pub mod error;
pub mod param_validator;
pub mod path_matcher;

pub use compiled_spec::{CompiledSpec, ValidationConfig, ValidationParts};
use error::{ValidationError, ValidationErrorKind, ValidationResult};

/// A plain representation of an HTTP request, decoupled from any web framework.
pub struct RequestData {
    pub method: String,
    pub path: String,
    pub query_string: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl RequestData {
    pub fn content_type(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str())
    }
}

/// Validate a request against a compiled OpenAPI spec, respecting the given config.
pub fn validate_request(
    spec: &CompiledSpec,
    request: &RequestData,
    config: &ValidationConfig,
) -> ValidationResult {
    if !config.enabled {
        return ValidationResult::Valid;
    }

    let mut errors = Vec::new();
    let parts = &config.parts;

    // 1. Path matching
    let matched = match spec.match_route(&request.path) {
        Some(result) => result,
        None => {
            if parts.path {
                return ValidationResult::Invalid(vec![ValidationError {
                    kind: ValidationErrorKind::PathNotFound,
                    message: format!("No matching path found for '{}'", request.path),
                    path: "path".to_string(),
                }]);
            }
            return ValidationResult::Valid;
        }
    };
    let captured_params = matched.params;

    // 2. Method check
    let operation = match spec.check_method(matched.template, &request.method) {
        Ok(op) => op,
        Err(err) => {
            if parts.method {
                return ValidationResult::Invalid(vec![err]);
            }
            return ValidationResult::Valid;
        }
    };

    // 3. Path parameter validation
    if parts.path_params {
        param_validator::validate_path_params(
            &captured_params,
            &operation.path_params,
            &mut errors,
        );
    }

    // 4. Query parameter validation
    if parts.query_params {
        let query_pairs = request
            .query_string
            .as_deref()
            .map(param_validator::parse_query_string)
            .unwrap_or_default();

        param_validator::validate_query_params(
            &query_pairs,
            &operation.query_params,
            &mut errors,
        );

        // Disallow additional query params not in spec
        if parts.disallow_additional_query_params {
            for (key, _) in &query_pairs {
                if !operation.query_params.iter().any(|p| &p.name == key) {
                    errors.push(ValidationError {
                        kind: ValidationErrorKind::InvalidParamValue,
                        message: format!(
                            "Unknown query parameter '{}' is not defined in the spec",
                            key
                        ),
                        path: format!("query.{}", key),
                    });
                }
            }
        }
    }

    // 5. Header parameter validation
    if parts.header_params {
        param_validator::validate_header_params(
            &request.headers,
            &operation.header_params,
            &mut errors,
        );
    }

    // 6. Cookie parameter validation
    if parts.cookie_params {
        let cookie_pairs = extract_cookies(&request.headers);
        param_validator::validate_query_params(
            &cookie_pairs,
            &operation.cookie_params,
            &mut errors,
        );
    }

    // 7 & 8. Content-Type and Body validation
    if let Some(ref req_body) = operation.request_body {
        let expected_media_types: Vec<&str> = req_body.content.keys().map(|s| s.as_str()).collect();

        if parts.content_type {
            content_type::validate_content_type(
                request.content_type(),
                &expected_media_types,
                &mut errors,
            );
        }

        if parts.body {
            // Find the matching media type's schema validator
            let schema_validator = request
                .content_type()
                .and_then(|ct| {
                    let media_type = ct.split(';').next().unwrap_or(ct).trim();
                    req_body.content.get(media_type)
                })
                .and_then(|mt| mt.schema_validator.as_ref());

            body_validator::validate_body(
                request.body.as_deref(),
                req_body.required,
                schema_validator,
                &mut errors,
            );
        }
    }

    if errors.is_empty() {
        ValidationResult::Valid
    } else {
        ValidationResult::Invalid(errors)
    }
}

/// Extract cookies from the Cookie header into key-value pairs.
fn extract_cookies(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        .flat_map(|(_, v)| {
            v.split(';').map(|cookie| {
                let mut parts = cookie.trim().splitn(2, '=');
                let name = parts.next().unwrap_or("").to_string();
                let value = parts.next().unwrap_or("").to_string();
                (name, value)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec_json() -> &'static str {
        r#"{
  "openapi": "3.1.0",
  "info": { "title": "Test API", "version": "1.0" },
  "paths": {
    "/users": {
      "get": {
        "parameters": [
          { "name": "page", "in": "query", "required": true, "schema": { "type": "integer" } },
          { "name": "limit", "in": "query", "schema": { "type": "integer" } }
        ],
        "responses": { "200": { "description": "OK" } }
      },
      "post": {
        "requestBody": {
          "required": true,
          "content": {
            "application/json": {
              "schema": {
                "type": "object",
                "required": ["name"],
                "properties": {
                  "name": { "type": "string" },
                  "email": { "type": "string" }
                }
              }
            }
          }
        },
        "responses": { "201": { "description": "Created" } }
      }
    },
    "/users/{id}": {
      "get": {
        "parameters": [
          { "name": "id", "in": "path", "required": true, "schema": { "type": "integer" } }
        ],
        "responses": { "200": { "description": "OK" } }
      }
    }
  }
}"#
    }

    fn compile_test_spec() -> CompiledSpec {
        let spec = oas3::from_json(sample_spec_json()).unwrap();
        compiled_spec::compile_spec(&spec).unwrap()
    }

    #[test]
    fn test_valid_get_request() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "GET".to_string(),
            path: "/users".to_string(),
            query_string: Some("page=1&limit=10".to_string()),
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(result.is_valid());
    }

    #[test]
    fn test_path_not_found() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "GET".to_string(),
            path: "/nonexistent".to_string(),
            query_string: None,
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
        assert_eq!(result.http_status(), Some(404));
    }

    #[test]
    fn test_method_not_allowed() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "DELETE".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
        assert_eq!(result.http_status(), Some(405));
    }

    #[test]
    fn test_missing_required_query_param() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "GET".to_string(),
            path: "/users".to_string(),
            query_string: Some("limit=10".to_string()),
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
        assert_eq!(result.http_status(), Some(400));
    }

    #[test]
    fn test_valid_post_with_body() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "POST".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: Some(br#"{"name": "Alice"}"#.to_vec()),
        };
        let result = validate_request(&spec, &req, &config);
        assert!(result.is_valid());
    }

    #[test]
    fn test_post_missing_required_body() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "POST".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
    }

    #[test]
    fn test_post_invalid_body_schema() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "POST".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: Some(br#"{"email": "alice@example.com"}"#.to_vec()),
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
    }

    #[test]
    fn test_wrong_content_type() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "POST".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
            body: Some(b"not json".to_vec()),
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
        assert_eq!(result.http_status(), Some(415));
    }

    #[test]
    fn test_disabled_validation() {
        let spec = compile_test_spec();
        let config = ValidationConfig {
            enabled: false,
            ..Default::default()
        };
        let req = RequestData {
            method: "GET".to_string(),
            path: "/nonexistent".to_string(),
            query_string: None,
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(result.is_valid());
    }

    #[test]
    fn test_selective_validation_skip_query() {
        let spec = compile_test_spec();
        let config = ValidationConfig {
            parts: ValidationParts {
                query_params: false,
                ..Default::default()
            },
            ..Default::default()
        };
        // Missing required 'page' param, but query validation is off
        let req = RequestData {
            method: "GET".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(result.is_valid());
    }

    #[test]
    fn test_disallow_additional_query_params() {
        let spec = compile_test_spec();
        let config = ValidationConfig {
            parts: ValidationParts {
                disallow_additional_query_params: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let req = RequestData {
            method: "GET".to_string(),
            path: "/users".to_string(),
            query_string: Some("page=1&unknown=foo".to_string()),
            headers: vec![],
            body: None,
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid());
    }
}
