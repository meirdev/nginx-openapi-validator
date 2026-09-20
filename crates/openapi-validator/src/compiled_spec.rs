use std::collections::HashMap;

use jsonschema::Validator;
use matchit::Router;
use mime::Mime;

use crate::content_type::media_type_matches;

use crate::error::{SpecError, ValidationError, ValidationErrorKind};

/// Pre-compiled OpenAPI spec optimized for per-request validation.
pub struct CompiledSpec {
    /// Radix-tree router from request path to the route's compiled operations.
    router: Router<CompiledRoute>,
}

impl Default for CompiledSpec {
    fn default() -> Self {
        Self {
            router: Router::new(),
        }
    }
}

/// Everything compiled for a single OpenAPI path template.
pub struct CompiledRoute {
    /// The original OpenAPI path template, e.g. `/users/{id}`.
    pub template: String,
    /// Operations keyed by uppercase HTTP method.
    pub operations: HashMap<String, CompiledOperation>,
}

pub struct CompiledOperation {
    pub path_params: Vec<CompiledParam>,
    pub query_params: Vec<CompiledParam>,
    pub header_params: Vec<CompiledParam>,
    pub cookie_params: Vec<CompiledParam>,
    pub request_body: Option<CompiledRequestBody>,
}

pub struct CompiledParam {
    pub name: String,
    pub required: bool,
    pub schema_validator: Option<Validator>,
}

pub struct CompiledRequestBody {
    pub required: bool,
    /// Media types accepted by this body, in spec order.
    pub content: Vec<CompiledMediaType>,
}

impl CompiledRequestBody {
    /// The media types declared in the spec for this body.
    pub fn media_types(&self) -> impl Iterator<Item = &Mime> {
        self.content.iter().map(|m| &m.media_type)
    }

    /// Find the first declared media type that accepts `actual`.
    ///
    /// Wildcards in the spec (`application/*`, `*/*`) are honoured and the
    /// comparison is case-insensitive.
    pub fn find_media_type(&self, actual: &Mime) -> Option<&CompiledMediaType> {
        self.content
            .iter()
            .find(|m| media_type_matches(&m.media_type, actual))
    }
}

pub struct CompiledMediaType {
    /// Media type as declared in the spec, e.g. `application/json` or `application/*`.
    pub media_type: Mime,
    pub schema_validator: Option<Validator>,
}

/// What parts of the request to validate. Each flag can be toggled independently.
#[derive(Debug, Clone)]
pub struct ValidationParts {
    pub path: bool,
    pub method: bool,
    pub path_params: bool,
    pub query_params: bool,
    pub header_params: bool,
    pub cookie_params: bool,
    pub content_type: bool,
    pub body: bool,
    /// If true, reject requests with query parameters not defined in the spec.
    pub disallow_additional_query_params: bool,
}

impl Default for ValidationParts {
    fn default() -> Self {
        Self {
            path: true,
            method: true,
            path_params: true,
            query_params: true,
            header_params: true,
            cookie_params: false,
            content_type: true,
            body: true,
            disallow_additional_query_params: false,
        }
    }
}

/// Enforcement mode for validation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnforcementMode {
    /// Return 4xx error and block the request.
    #[default]
    Block,
    /// Log the error but pass the request through.
    Audit,
}

/// Full validation configuration for a location.
#[derive(Debug, Clone)]
pub struct ValidationConfig {
    pub enabled: bool,
    pub enforcement: EnforcementMode,
    pub parts: ValidationParts,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enforcement: EnforcementMode::Block,
            parts: ValidationParts::default(),
        }
    }
}

/// Result of matching a request path against the compiled spec.
pub struct MatchedRoute<'a> {
    /// The OpenAPI path template that matched.
    pub template: &'a str,
    /// Captured path parameter name-value pairs, in template order.
    pub params: Vec<(&'a str, &'a str)>,
    route: &'a CompiledRoute,
}

impl<'a> MatchedRoute<'a> {
    /// Look up the operation for an HTTP method on this route.
    pub fn check_method(&self, method: &str) -> Result<&'a CompiledOperation, ValidationError> {
        self.route
            .operations
            .get(&method.to_ascii_uppercase())
            .ok_or_else(|| {
                let mut allowed: Vec<&str> =
                    self.route.operations.keys().map(String::as_str).collect();
                allowed.sort_unstable();
                ValidationError {
                    kind: ValidationErrorKind::MethodNotAllowed,
                    message: format!(
                        "Method '{method}' is not allowed for path '{}'. Allowed: {}",
                        self.template,
                        allowed.join(", ")
                    ),
                    path: "method".to_string(),
                }
            })
    }
}

impl CompiledSpec {
    /// Parse an OpenAPI 3.x document from JSON text and compile it.
    ///
    /// This is the entry point for callers that do not want to depend on the
    /// underlying OpenAPI parser crate.
    pub fn from_json(json: &str) -> Result<Self, SpecError> {
        let spec = oas3::from_json(json).map_err(|e| SpecError::ParseError(e.to_string()))?;
        compile_spec(&spec)
    }

    /// Find a route matching the given request path.
    ///
    /// The path must already be percent-decoded and must not contain the query
    /// string. A trailing slash is ignored, so `/users/` matches `/users`.
    pub fn match_route<'a>(&'a self, request_path: &'a str) -> Option<MatchedRoute<'a>> {
        let m = self.router.at(normalize_path(request_path)).ok()?;
        Some(MatchedRoute {
            template: m.value.template.as_str(),
            params: m.params.iter().collect(),
            route: m.value,
        })
    }
}

/// Strip trailing slashes so templates and request paths compare consistently.
fn normalize_path(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

/// Compile an oas3::Spec into a CompiledSpec for fast per-request validation.
pub fn compile_spec(spec: &oas3::Spec) -> Result<CompiledSpec, SpecError> {
    let mut router = Router::new();

    let Some(paths) = &spec.paths else {
        return Ok(CompiledSpec { router });
    };

    for (path_template, path_item) in paths {
        // Collect path-level parameters
        let path_level_params: Vec<oas3::spec::Parameter> = path_item
            .parameters
            .iter()
            .filter_map(|p| resolve_param(p, spec))
            .collect();

        let mut operations = HashMap::new();

        for (method, operation) in path_item.methods() {
            // Merge path-level and operation-level parameters.
            // Operation-level overrides path-level by name+location.
            let op_params = operation.parameters(spec).unwrap_or_default();

            let mut merged_params = path_level_params.clone();
            for op_param in op_params {
                if let Some(existing) = merged_params
                    .iter_mut()
                    .find(|p| p.name == op_param.name && p.location == op_param.location)
                {
                    *existing = op_param;
                } else {
                    merged_params.push(op_param);
                }
            }

            let mut path_params = Vec::new();
            let mut query_params = Vec::new();
            let mut header_params = Vec::new();
            let mut cookie_params = Vec::new();

            for param in &merged_params {
                let compiled = compile_param(param, spec)?;
                match param.location {
                    oas3::spec::ParameterIn::Path => path_params.push(compiled),
                    oas3::spec::ParameterIn::Query => query_params.push(compiled),
                    oas3::spec::ParameterIn::Header => header_params.push(compiled),
                    oas3::spec::ParameterIn::Cookie => cookie_params.push(compiled),
                }
            }

            // Compile request body
            let request_body = compile_request_body(operation, spec)?;

            operations.insert(
                method.as_str().to_ascii_uppercase(),
                CompiledOperation {
                    path_params,
                    query_params,
                    header_params,
                    cookie_params,
                    request_body,
                },
            );
        }

        router
            .insert(
                normalize_path(path_template),
                CompiledRoute {
                    template: path_template.clone(),
                    operations,
                },
            )
            .map_err(|e| SpecError::PathTemplateError(path_template.clone(), e.to_string()))?;
    }

    Ok(CompiledSpec { router })
}

fn resolve_param(
    param_or_ref: &oas3::spec::ObjectOrReference<oas3::spec::Parameter>,
    spec: &oas3::Spec,
) -> Option<oas3::spec::Parameter> {
    match param_or_ref {
        oas3::spec::ObjectOrReference::Object(p) => Some(p.clone()),
        oas3::spec::ObjectOrReference::Ref { ref_path, .. } => {
            let name = ref_path.rsplit('/').next()?;
            let components = spec.components.as_ref()?;
            let resolved = components.parameters.get(name)?;
            resolve_param(resolved, spec)
        }
    }
}

fn compile_param(
    param: &oas3::spec::Parameter,
    spec: &oas3::Spec,
) -> Result<CompiledParam, SpecError> {
    let schema_validator = match &param.schema {
        Some(schema) => Some(compile_schema(schema, spec)?),
        None => None,
    };

    Ok(CompiledParam {
        name: param.name.clone(),
        required: param.required.unwrap_or(false),
        schema_validator,
    })
}

/// Resolve a top-level `$ref` (if any) and compile the schema into a validator.
fn compile_schema(
    schema: &oas3::spec::Schema,
    spec: &oas3::Spec,
) -> Result<Validator, SpecError> {
    let resolved = schema
        .resolve(spec)
        .map_err(|e| SpecError::RefResolutionError(e.to_string()))?;
    let schema_json = serde_json::to_value(&resolved)
        .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;
    jsonschema::validator_for(&schema_json)
        .map_err(|e| SpecError::SchemaCompileError(e.to_string()))
}

fn compile_request_body(
    operation: &oas3::spec::Operation,
    spec: &oas3::Spec,
) -> Result<Option<CompiledRequestBody>, SpecError> {
    let body = match operation.request_body(spec) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(None),
        Err(e) => return Err(SpecError::RefResolutionError(e.to_string())),
    };

    let required = body.required.unwrap_or(false);
    let mut content = Vec::with_capacity(body.content.len());

    for (media_type_str, media_type) in &body.content {
        let parsed: Mime = media_type_str.parse().map_err(|e| {
            SpecError::ParseError(format!(
                "invalid request body media type '{media_type_str}': {e}"
            ))
        })?;

        let schema_validator = match &media_type.schema {
            Some(schema) => Some(compile_schema(schema, spec)?),
            None => None,
        };

        content.push(CompiledMediaType {
            media_type: parsed,
            schema_validator,
        });
    }

    Ok(Some(CompiledRequestBody { required, content }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a spec whose paths all expose a GET with no parameters.
    fn build_spec(templates: &[&str]) -> CompiledSpec {
        let paths: Vec<String> = templates
            .iter()
            .map(|t| format!(r#""{t}": {{"get": {{"responses": {{"200": {{"description": "ok"}}}}}}}}"#))
            .collect();
        let json = format!(
            r#"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}}, "paths": {{{}}}}}"#,
            paths.join(",")
        );
        CompiledSpec::from_json(&json).unwrap()
    }

    #[test]
    fn test_simple_static_path() {
        let spec = build_spec(&["/users"]);
        let m = spec.match_route("/users").unwrap();
        assert_eq!(m.template, "/users");
        assert!(m.params.is_empty());
    }

    #[test]
    fn test_no_match() {
        let spec = build_spec(&["/users"]);
        assert!(spec.match_route("/posts").is_none());
    }

    #[test]
    fn test_path_with_param() {
        let spec = build_spec(&["/users/{id}"]);
        let m = spec.match_route("/users/123").unwrap();
        assert_eq!(m.template, "/users/{id}");
        assert_eq!(m.params, vec![("id", "123")]);
    }

    #[test]
    fn test_multiple_params() {
        let spec = build_spec(&["/users/{userId}/posts/{postId}"]);
        let m = spec.match_route("/users/42/posts/99").unwrap();
        assert_eq!(m.template, "/users/{userId}/posts/{postId}");
        assert_eq!(m.params, vec![("userId", "42"), ("postId", "99")]);
    }

    #[test]
    fn test_no_match_extra_segments() {
        let spec = build_spec(&["/users/{id}"]);
        assert!(spec.match_route("/users/123/extra").is_none());
    }

    #[test]
    fn test_no_match_too_few_segments() {
        let spec = build_spec(&["/users/{id}"]);
        assert!(spec.match_route("/users").is_none());
    }

    #[test]
    fn test_static_over_param_priority() {
        let spec = build_spec(&["/users/me", "/users/{id}"]);
        let m = spec.match_route("/users/me").unwrap();
        assert_eq!(m.template, "/users/me");
        assert!(m.params.is_empty());

        let m = spec.match_route("/users/123").unwrap();
        assert_eq!(m.template, "/users/{id}");
        assert_eq!(m.params, vec![("id", "123")]);
    }

    #[test]
    fn test_different_param_names_at_same_position() {
        // Each template keeps its own parameter name.
        let spec = build_spec(&["/users/{id}", "/users/{userId}/posts"]);
        let m = spec.match_route("/users/5").unwrap();
        assert_eq!(m.params, vec![("id", "5")]);

        let m = spec.match_route("/users/5/posts").unwrap();
        assert_eq!(m.template, "/users/{userId}/posts");
        assert_eq!(m.params, vec![("userId", "5")]);
    }

    #[test]
    fn test_param_with_suffix() {
        let spec = build_spec(&["/files/{name}.json"]);
        let m = spec.match_route("/files/report.json").unwrap();
        assert_eq!(m.params, vec![("name", "report")]);
        assert!(spec.match_route("/files/report.xml").is_none());
    }

    #[test]
    fn test_duplicate_template_with_different_param_name_is_rejected() {
        // OpenAPI forbids templates that differ only by parameter name.
        let json = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {
            "/pets/{id}": {"get": {"responses": {"200": {"description": "ok"}}}},
            "/pets/{petId}": {"get": {"responses": {"200": {"description": "ok"}}}}
        }}"#;
        assert!(matches!(
            CompiledSpec::from_json(json),
            Err(SpecError::PathTemplateError(..))
        ));
    }

    #[test]
    fn test_multiple_routes() {
        let spec = build_spec(&["/users", "/users/{id}", "/posts", "/posts/{id}/comments"]);

        assert!(spec.match_route("/users").is_some());
        assert!(spec.match_route("/users/5").is_some());
        assert!(spec.match_route("/posts").is_some());
        assert!(spec.match_route("/posts/1/comments").is_some());
        assert!(spec.match_route("/other").is_none());
    }

    #[test]
    fn test_trailing_slash() {
        let spec = build_spec(&["/users", "/posts/"]);
        assert!(spec.match_route("/users/").is_some());
        assert!(spec.match_route("/posts").is_some());
        assert!(spec.match_route("/posts/").is_some());
    }

    #[test]
    fn test_param_rejects_empty_segment() {
        let spec = build_spec(&["/users/{id}/posts"]);
        assert!(spec.match_route("/users//posts").is_none());
    }

    #[test]
    fn test_root_path() {
        let spec = build_spec(&["/"]);
        assert!(spec.match_route("/").is_some());
        assert!(spec.match_route("").is_some());
    }

    #[test]
    fn test_method_check() {
        let spec = build_spec(&["/users"]);
        let m = spec.match_route("/users").unwrap();
        assert!(m.check_method("get").is_ok());
        let err = m.check_method("DELETE").err().expect("DELETE must be rejected");
        assert_eq!(err.kind, ValidationErrorKind::MethodNotAllowed);
        assert!(err.message.contains("Allowed: GET"));
    }

    #[test]
    fn test_from_json_parse_error() {
        assert!(matches!(
            CompiledSpec::from_json("not json"),
            Err(SpecError::ParseError(_))
        ));
    }
}
