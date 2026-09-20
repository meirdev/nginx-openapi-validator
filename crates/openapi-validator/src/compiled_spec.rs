use std::collections::HashMap;

use jsonschema::{Draft, Validator};
use matchit::Router;
use mime::Mime;
use serde_json::Value;

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
    /// Whether array values arrive as repeated keys (`a=1&a=2`) rather than
    /// a single comma-separated value (`a=1,2`). Defaults per OpenAPI: true
    /// for query and cookie parameters, false for path and header parameters.
    pub explode: bool,
    /// The resolved JSON Schema, used to coerce the raw string value into the
    /// declared type before validation. `None` when the parameter has no schema.
    pub schema: Option<Value>,
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

    let compiler = SchemaCompiler::new(spec)?;

    for (path_template, path_item) in paths {
        // Collect path-level parameters
        let path_level_params = path_item
            .parameters
            .iter()
            .map(|p| resolve_param(p, spec))
            .collect::<Result<Vec<_>, _>>()?;

        let mut operations = HashMap::new();

        for (method, operation) in path_item.methods() {
            // Merge path-level and operation-level parameters.
            // Operation-level overrides path-level by name+location.
            let op_params = operation
                .parameters(spec)
                .map_err(|e| SpecError::RefResolutionError(e.to_string()))?;

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
                if is_ignored_header_param(param) {
                    continue;
                }
                let compiled = compile_param(param, &compiler)?;
                match param.location {
                    oas3::spec::ParameterIn::Path => path_params.push(compiled),
                    oas3::spec::ParameterIn::Query => query_params.push(compiled),
                    oas3::spec::ParameterIn::Header => header_params.push(compiled),
                    oas3::spec::ParameterIn::Cookie => cookie_params.push(compiled),
                }
            }

            // Compile request body
            let request_body = compile_request_body(operation, spec, &compiler)?;

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

/// OpenAPI: header parameters named Accept, Content-Type or Authorization
/// are ignored; those headers are governed by other parts of the spec.
fn is_ignored_header_param(param: &oas3::spec::Parameter) -> bool {
    param.location == oas3::spec::ParameterIn::Header
        && ["accept", "content-type", "authorization"]
            .iter()
            .any(|h| param.name.eq_ignore_ascii_case(h))
}

const PARAMETER_REF_PREFIX: &str = "#/components/parameters/";

fn resolve_param(
    param_or_ref: &oas3::spec::ObjectOrReference<oas3::spec::Parameter>,
    spec: &oas3::Spec,
) -> Result<oas3::spec::Parameter, SpecError> {
    match param_or_ref {
        oas3::spec::ObjectOrReference::Object(p) => Ok(p.clone()),
        oas3::spec::ObjectOrReference::Ref { ref_path, .. } => {
            let unresolvable = || SpecError::RefResolutionError(ref_path.clone());
            let name = ref_path
                .strip_prefix(PARAMETER_REF_PREFIX)
                .ok_or_else(unresolvable)?;
            let resolved = spec
                .components
                .as_ref()
                .and_then(|c| c.parameters.get(name))
                .ok_or_else(unresolvable)?;
            resolve_param(resolved, spec)
        }
    }
}

fn compile_param(
    param: &oas3::spec::Parameter,
    compiler: &SchemaCompiler,
) -> Result<CompiledParam, SpecError> {
    let (schema, schema_validator) = match &param.schema {
        Some(schema) => {
            let (json, validator) = compiler.compile(schema)?;
            (Some(json), Some(validator))
        }
        None => (None, None),
    };

    // OpenAPI default: explode is true for form style (query, cookie) and
    // false for simple style (path, header).
    let explode = param.explode.unwrap_or(matches!(
        param.location,
        oas3::spec::ParameterIn::Query | oas3::spec::ParameterIn::Cookie
    ));

    Ok(CompiledParam {
        name: param.name.clone(),
        required: param.required.unwrap_or(false),
        explode,
        schema,
        schema_validator,
    })
}

/// Compiles OpenAPI schemas into JSON Schema validators.
///
/// Schemas in an OpenAPI document reference each other through
/// `#/components/schemas/...` pointers that are relative to the whole document,
/// not to the schema being compiled. To make those pointers resolvable, the
/// spec's `components.schemas` are embedded into every compiled schema document
/// under a `components` key, which JSON Schema treats as an unknown keyword.
struct SchemaCompiler<'s> {
    spec: &'s oas3::Spec,
    components: Option<Value>,
}

impl<'s> SchemaCompiler<'s> {
    fn new(spec: &'s oas3::Spec) -> Result<Self, SpecError> {
        let components = match &spec.components {
            Some(c) if !c.schemas.is_empty() => {
                let schemas = serde_json::to_value(&c.schemas)
                    .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;
                Some(serde_json::json!({ "schemas": schemas }))
            }
            _ => None,
        };
        Ok(Self { spec, components })
    }

    /// Resolve a top-level `$ref` (if any) and compile the schema.
    ///
    /// Returns the resolved schema JSON alongside the validator.
    fn compile(&self, schema: &oas3::spec::Schema) -> Result<(Value, Validator), SpecError> {
        let resolved = schema
            .resolve(self.spec)
            .map_err(|e| SpecError::RefResolutionError(e.to_string()))?;
        let schema_json = serde_json::to_value(&resolved)
            .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;

        let mut document = schema_json.clone();
        if let (Value::Object(map), Some(components)) = (&mut document, &self.components) {
            map.entry("components").or_insert_with(|| components.clone());
        }

        let validator = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .build(&document)
            .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;
        Ok((schema_json, validator))
    }
}

fn compile_request_body(
    operation: &oas3::spec::Operation,
    spec: &oas3::Spec,
    compiler: &SchemaCompiler,
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
            Some(schema) => Some(compiler.compile(schema)?.1),
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
    fn test_nested_component_refs_resolve() {
        let json = r##"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"},
          "components": {"schemas": {
            "Owner": {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}},
            "Pet": {"type": "object", "properties": {"owner": {"$ref": "#/components/schemas/Owner"}}}
          }},
          "paths": {"/pets": {"post": {
            "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}},
            "responses": {"201": {"description": "ok"}}}}}}"##;
        let spec = CompiledSpec::from_json(json).expect("nested $ref must compile");
        let m = spec.match_route("/pets").unwrap();
        let op = m.check_method("POST").unwrap();
        let body = op.request_body.as_ref().unwrap();
        let validator = body.content[0].schema_validator.as_ref().unwrap();
        assert!(validator.is_valid(&serde_json::json!({"owner": {"name": "x"}})));
        assert!(!validator.is_valid(&serde_json::json!({"owner": {}})));
    }

    #[test]
    fn test_reserved_header_params_are_ignored() {
        let json = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {"/x": {"get": {
            "parameters": [
                {"name": "Authorization", "in": "header", "required": true, "schema": {"type": "string"}},
                {"name": "X-Trace", "in": "header", "schema": {"type": "string"}}
            ],
            "responses": {"200": {"description": "ok"}}}}}}"#;
        let spec = CompiledSpec::from_json(json).unwrap();
        let op = spec.match_route("/x").unwrap().check_method("GET").unwrap();
        let names: Vec<&str> = op.header_params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["X-Trace"]);
    }

    #[test]
    fn test_unresolvable_path_level_param_ref_is_an_error() {
        let json = r##"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {"/x": {
            "parameters": [{"$ref": "#/components/parameters/Missing"}],
            "get": {"responses": {"200": {"description": "ok"}}}}}}"##;
        assert!(matches!(
            CompiledSpec::from_json(json),
            Err(SpecError::RefResolutionError(_))
        ));
    }

    #[test]
    fn test_from_json_parse_error() {
        assert!(matches!(
            CompiledSpec::from_json("not json"),
            Err(SpecError::ParseError(_))
        ));
    }
}
