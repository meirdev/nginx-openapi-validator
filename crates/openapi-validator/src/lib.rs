pub mod body_validator;
pub mod coerce;
pub mod compiled_spec;
pub mod content_type;
pub mod dialect;
pub mod error;
pub mod form;
pub mod param_validator;
pub mod xml;

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
    // 2. Method check
    let operation = match matched.check_method(&request.method) {
        Ok(op) => op,
        Err(err) => {
            if parts.method {
                return ValidationResult::Invalid(vec![err]);
            }
            return ValidationResult::Valid;
        }
    };

    // 3. Path parameter validation
    let root = spec.document();

    if parts.path_params {
        param_validator::validate_path_params(
            &matched.params,
            &operation.path_params,
            root,
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
            root,
            &mut errors,
        );

        // Disallow additional query params not in spec
        if parts.disallow_additional_query_params {
            for (key, _) in &query_pairs {
                if !operation
                    .query_params
                    .iter()
                    .any(|p| param_validator::query_key_belongs_to(key, p, root))
                {
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
            root,
            &mut errors,
        );
    }

    // 6. Cookie parameter validation
    if parts.cookie_params {
        let cookie_pairs = extract_cookies(&request.headers);
        param_validator::validate_cookie_params(
            &cookie_pairs,
            &operation.cookie_params,
            root,
            &mut errors,
        );
    }

    // 7 & 8. Content-Type and Body validation
    if let Some(ref req_body) = operation.request_body {
        // A request that carries no body has nothing for Content-Type to
        // describe, so the header is only demanded when a body is present or
        // required.
        let has_body = request.body.as_deref().is_some_and(|b| !b.is_empty());

        if parts.content_type && (has_body || req_body.required) {
            let expected: Vec<&mime::Mime> = req_body.media_types().collect();
            content_type::validate_content_type(request.content_type(), &expected, &mut errors);
        }

        if parts.body {
            let request_media_type = request.content_type().and_then(content_type::parse_media_type);

            // The declared media type that accepts the request's Content-Type.
            let media = request_media_type
                .as_ref()
                .and_then(|ct| req_body.find_media_type(ct));

            // Without a Content-Type, JSON is assumed since that is what the
            // schema describes.
            let kind = request_media_type
                .as_ref()
                .map_or(body_validator::BodyKind::Json, content_type::body_kind);

            body_validator::validate_body(
                request.body.as_deref(),
                req_body.required,
                media,
                kind,
                root,
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
        CompiledSpec::from_json(sample_spec_json()).unwrap()
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
    fn test_body_schema_applies_regardless_of_content_type_case() {
        let spec = compile_test_spec();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "POST".to_string(),
            path: "/users".to_string(),
            query_string: None,
            headers: vec![("Content-Type".to_string(), "Application/JSON; charset=utf-8".to_string())],
            body: Some(br#"{"email": "alice@example.com"}"#.to_vec()),
        };
        let result = validate_request(&spec, &req, &config);
        assert!(!result.is_valid(), "schema must be enforced when media type differs only by case");
        assert_eq!(result.http_status(), Some(400));
    }

    #[test]
    fn test_string_query_param_accepts_numeric_value() {
        let spec = CompiledSpec::from_json(r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {"/q": {"get": {
            "parameters": [{"name": "name", "in": "query", "required": true, "schema": {"type": "string"}}],
            "responses": {"200": {"description": "ok"}}}}}}"#).unwrap();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "GET".to_string(),
            path: "/q".to_string(),
            query_string: Some("name=123".to_string()),
            headers: vec![],
            body: None,
        };
        assert!(validate_request(&spec, &req, &config).is_valid());
    }

    #[test]
    fn test_form_body_is_not_parsed_as_json() {
        let spec = CompiledSpec::from_json(r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {"/f": {"post": {
            "requestBody": {"required": true, "content": {"application/x-www-form-urlencoded": {"schema": {"type": "object"}}}},
            "responses": {"200": {"description": "ok"}}}}}}"#).unwrap();
        let config = ValidationConfig::default();
        let req = RequestData {
            method: "POST".to_string(),
            path: "/f".to_string(),
            query_string: None,
            headers: vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())],
            body: Some(b"a=1&b=2".to_vec()),
        };
        assert!(validate_request(&spec, &req, &config).is_valid());
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
