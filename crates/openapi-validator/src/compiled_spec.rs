use std::collections::HashMap;
use std::sync::Arc;

use jsonschema::{Draft, Registry, Validator};
use matchit::Router;
use mime::Mime;
use serde_json::{json, Value};

use crate::coerce::resolve_pointer;
use crate::content_type::media_type_matches;
use crate::dialect;
use crate::error::{SpecError, ValidationError, ValidationErrorKind};

/// URI under which the OpenAPI document is registered with the schema
/// engine, so every `#/components/...` reference resolves against it.
const SPEC_URI: &str = "urn:openapi-validator:spec";

/// Pre-compiled OpenAPI spec optimized for per-request validation.
pub struct CompiledSpec {
    /// Radix-tree router from request path to the route's compiled operations.
    router: Router<CompiledRoute>,
    /// The OpenAPI document after dialect rewrites, used to follow `$ref`s at
    /// request time (e.g. to learn a parameter's declared type).
    document: Arc<Value>,
}

impl Default for CompiledSpec {
    fn default() -> Self {
        Self {
            router: Router::new(),
            document: Arc::new(Value::Null),
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

/// Where a parameter is carried in the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
    Cookie,
}

/// OpenAPI parameter serialization style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamStyle {
    Form,
    Simple,
    Label,
    Matrix,
    SpaceDelimited,
    PipeDelimited,
    DeepObject,
}

impl ParamStyle {
    fn parse(name: Option<&str>, location: ParamLocation) -> Result<Self, SpecError> {
        Ok(match name {
            None => match location {
                ParamLocation::Query | ParamLocation::Cookie => Self::Form,
                ParamLocation::Path | ParamLocation::Header => Self::Simple,
            },
            Some("form") => Self::Form,
            Some("simple") => Self::Simple,
            Some("label") => Self::Label,
            Some("matrix") => Self::Matrix,
            Some("spaceDelimited") => Self::SpaceDelimited,
            Some("pipeDelimited") => Self::PipeDelimited,
            Some("deepObject") => Self::DeepObject,
            Some(other) => {
                return Err(SpecError::ParseError(format!(
                    "unknown parameter style '{other}'"
                )))
            }
        })
    }
}

pub struct CompiledParam {
    pub name: String,
    pub location: ParamLocation,
    pub required: bool,
    pub style: ParamStyle,
    /// The `explode` flag as written in the document. `None` means it was
    /// omitted; decoders apply the OpenAPI default for the style, with the
    /// leniency the reference implementation shows for omitted values.
    pub explode: Option<bool>,
    /// The parameter's schema as written in the document (may be a `$ref`),
    /// used to coerce the raw string value into the declared type.
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
    /// The body schema as written in the document (may be a `$ref`). Form and
    /// XML decoders use it to map fields and coerce scalar values.
    pub schema: Option<Value>,
    pub schema_validator: Option<Validator>,
    /// The media type's `encoding` object, if any (form bodies only).
    pub encoding: Option<Value>,
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

impl ValidationParts {
    /// Names accepted by [`ValidationParts::parse`], besides `all`.
    pub const NAMES: [&'static str; 9] = [
        "path",
        "method",
        "path_params",
        "query_params",
        "header_params",
        "cookie_params",
        "content_type",
        "body",
        "disallow_additional_query_params",
    ];

    /// Every check disabled.
    pub fn none() -> Self {
        Self {
            path: false,
            method: false,
            path_params: false,
            query_params: false,
            header_params: false,
            cookie_params: false,
            content_type: false,
            body: false,
            disallow_additional_query_params: false,
        }
    }

    /// Parse a comma-separated list of part names, as given to the
    /// `openapi_validate_parts` directive. Everything starts off, then the
    /// listed parts are enabled; `all` enables the default set. Names are
    /// case-insensitive and surrounding whitespace is ignored.
    ///
    /// On failure, returns the first unknown name.
    pub fn parse(list: &str) -> Result<Self, String> {
        let mut parts = Self::none();
        for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "path" => parts.path = true,
                "method" => parts.method = true,
                "path_params" => parts.path_params = true,
                "query_params" => parts.query_params = true,
                "header_params" => parts.header_params = true,
                "cookie_params" => parts.cookie_params = true,
                "content_type" => parts.content_type = true,
                "body" => parts.body = true,
                "disallow_additional_query_params" => {
                    parts.disallow_additional_query_params = true;
                }
                "all" => parts = Self::default(),
                _ => return Err(part.to_string()),
            }
        }
        Ok(parts)
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

impl std::str::FromStr for EnforcementMode {
    type Err = ();

    /// Parse `block` or `audit` (case-insensitive), as given to the
    /// `openapi_validate_mode` directive.
    fn from_str(s: &str) -> Result<Self, ()> {
        if s.eq_ignore_ascii_case("block") {
            Ok(Self::Block)
        } else if s.eq_ignore_ascii_case("audit") {
            Ok(Self::Audit)
        } else {
            Err(())
        }
    }
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
    ///
    /// A HEAD request falls back to the GET operation when the spec does not
    /// declare HEAD, since servers answer HEAD from the GET handler.
    pub fn check_method(&self, method: &str) -> Result<&'a CompiledOperation, ValidationError> {
        let method_upper = method.to_ascii_uppercase();
        self.route
            .operations
            .get(&method_upper)
            .or_else(|| (method_upper == "HEAD").then(|| self.route.operations.get("GET")).flatten())
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
    /// Parse an OpenAPI 3.0.x or 3.1.x document from JSON text and compile it.
    pub fn from_json(json: &str) -> Result<Self, SpecError> {
        let doc: Value =
            serde_json::from_str(json).map_err(|e| SpecError::ParseError(e.to_string()))?;
        compile_document(doc)
    }

    /// The OpenAPI document after dialect rewrites.
    pub fn document(&self) -> &Value {
        &self.document
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

// ---------------------------------------------------------------------------
// Document navigation
// ---------------------------------------------------------------------------

/// Escape a key for use in a JSON pointer (RFC 6901).
fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// Percent-encode a JSON pointer for use as a URI fragment (RFC 3986), so
/// characters such as `{` in path templates do not break the reference.
fn fragment_encode(pointer: &str) -> String {
    const KEEP: &[u8] = b"-._~!$&'()*+,;=:@/?";
    let mut out = String::with_capacity(pointer.len());
    for byte in pointer.bytes() {
        if byte.is_ascii_alphanumeric() || KEEP.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

struct Document {
    root: Arc<Value>,
    registry: Registry<'static>,
}

impl Document {
    fn node(&self) -> (String, &Value) {
        (String::new(), &self.root)
    }

    /// Child of `parent` under `key`, with its pointer.
    fn child<'v>(
        &self,
        parent_pointer: &str,
        parent: &'v Value,
        key: &str,
    ) -> Option<(String, &'v Value)> {
        let value = parent.get(key)?;
        Some((
            format!("{parent_pointer}/{}", escape_pointer_token(key)),
            value,
        ))
    }

    /// Follow a `$ref` (if present) to its target, returning the target's pointer.
    fn deref<'a>(&'a self, pointer: String, value: &'a Value) -> Result<(String, &'a Value), SpecError> {
        let mut current = (pointer, value);
        for _ in 0..32 {
            let Some(reference) = current.1.get("$ref").and_then(Value::as_str) else {
                return Ok(current);
            };
            let target = resolve_pointer(&self.root, reference)
                .ok_or_else(|| SpecError::RefResolutionError(reference.to_string()))?;
            current = (reference.trim_start_matches('#').to_string(), target);
        }
        Err(SpecError::RefResolutionError(format!(
            "reference chain too deep at {}",
            current.0
        )))
    }

    /// Compile the schema at `pointer` into a validator.
    fn compile_schema(&self, pointer: &str) -> Result<Validator, SpecError> {
        let schema = json!({ "$ref": format!("{SPEC_URI}#{}", fragment_encode(pointer)) });
        jsonschema::options()
            .with_draft(Draft::Draft202012)
            .with_registry(&self.registry)
            .build(&schema)
            .map_err(|e| SpecError::SchemaCompileError(format!("at {pointer}: {e}")))
    }
}

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

fn compile_document(mut doc: Value) -> Result<CompiledSpec, SpecError> {
    if !doc.is_object() {
        return Err(SpecError::ParseError(
            "OpenAPI document must be a JSON object".to_string(),
        ));
    }
    let version = doc
        .get("openapi")
        .and_then(Value::as_str)
        .ok_or_else(|| SpecError::ParseError("missing 'openapi' version field".to_string()))?
        .to_string();
    if !version.starts_with("3.") {
        return Err(SpecError::ParseError(format!(
            "unsupported OpenAPI version '{version}'; only 3.0.x and 3.1.x are supported"
        )));
    }

    if dialect::is_openapi_30(&version) {
        dialect::apply_openapi_30(&mut doc)?;
    }
    dialect::strip_read_only_required(&mut doc);

    let registry = Registry::new()
        .add(SPEC_URI, doc.clone())
        .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?
        .prepare()
        .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;
    let document = Document {
        root: Arc::new(doc),
        registry,
    };

    // Routes keyed by normalized template, so `/users` and `/users/` merge
    // into one route instead of colliding in the router.
    let mut routes: Vec<(String, CompiledRoute)> = Vec::new();
    let (root_pointer, root) = document.node();
    let Some((paths_pointer, paths)) = document.child(&root_pointer, root, "paths") else {
        return Ok(CompiledSpec {
            router: Router::new(),
            document: document.root,
        });
    };
    let Some(paths) = paths.as_object() else {
        return Err(SpecError::ParseError("'paths' must be an object".to_string()));
    };

    for (path_template, item) in paths {
        let item_pointer = format!("{paths_pointer}/{}", escape_pointer_token(path_template));
        let (item_pointer, item) = document.deref(item_pointer, item)?;

        let path_level_params = collect_params(&document, &item_pointer, item)?;

        let mut operations = HashMap::new();
        for method in METHODS {
            let Some((op_pointer, op)) = document.child(&item_pointer, item, method) else {
                continue;
            };
            if !op.is_object() {
                continue;
            }

            // Operation-level parameters override path-level ones by name+location.
            let mut merged = path_level_params.clone();
            for param in collect_params(&document, &op_pointer, op)? {
                if let Some(existing) = merged
                    .iter_mut()
                    .find(|p| p.name == param.name && p.location == param.location)
                {
                    *existing = param;
                } else {
                    merged.push(param);
                }
            }

            let mut path_params = Vec::new();
            let mut query_params = Vec::new();
            let mut header_params = Vec::new();
            let mut cookie_params = Vec::new();
            for param in &merged {
                if param.location == "header" && is_ignored_header(&param.name) {
                    continue;
                }
                let compiled = compile_param(&document, param)?;
                match compiled.location {
                    ParamLocation::Path => path_params.push(compiled),
                    ParamLocation::Query => query_params.push(compiled),
                    ParamLocation::Header => header_params.push(compiled),
                    ParamLocation::Cookie => cookie_params.push(compiled),
                }
            }

            let request_body = compile_request_body(&document, &op_pointer, op)?;

            operations.insert(
                method.to_ascii_uppercase(),
                CompiledOperation {
                    path_params,
                    query_params,
                    header_params,
                    cookie_params,
                    request_body,
                },
            );
        }

        let normalized = normalize_path(path_template).to_string();
        match routes.iter_mut().find(|(key, _)| *key == normalized) {
            Some((_, existing)) => {
                for (method, op) in operations {
                    if existing.operations.contains_key(&method) {
                        return Err(SpecError::PathTemplateError(
                            path_template.clone(),
                            format!(
                                "{method} is already declared on '{}', which differs only by a trailing slash",
                                existing.template
                            ),
                        ));
                    }
                    existing.operations.insert(method, op);
                }
            }
            None => routes.push((
                normalized,
                CompiledRoute {
                    template: path_template.clone(),
                    operations,
                },
            )),
        }
    }

    let mut router = Router::new();
    for (normalized, route) in routes {
        let template = route.template.clone();
        router
            .insert(normalized, route)
            .map_err(|e| SpecError::PathTemplateError(template, e.to_string()))?;
    }

    Ok(CompiledSpec {
        router,
        document: document.root,
    })
}

/// A parameter object located in the document, with `$ref` already followed.
#[derive(Clone)]
struct ParamNode {
    pointer: String,
    name: String,
    location: String,
    value: Value,
}

fn collect_params(
    document: &Document,
    owner_pointer: &str,
    owner: &Value,
) -> Result<Vec<ParamNode>, SpecError> {
    let Some((list_pointer, list)) = document.child(owner_pointer, owner, "parameters") else {
        return Ok(Vec::new());
    };
    let Some(list) = list.as_array() else {
        return Err(SpecError::ParseError(format!(
            "'parameters' at {list_pointer} must be an array"
        )));
    };

    let mut params = Vec::with_capacity(list.len());
    for (index, entry) in list.iter().enumerate() {
        let (pointer, value) = document.deref(format!("{list_pointer}/{index}"), entry)?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| SpecError::ParseError(format!("parameter at {pointer} has no name")))?;
        let location = value
            .get("in")
            .and_then(Value::as_str)
            .ok_or_else(|| SpecError::ParseError(format!("parameter '{name}' has no 'in'")))?;
        params.push(ParamNode {
            pointer,
            name: name.to_string(),
            location: location.to_string(),
            value: value.clone(),
        });
    }
    Ok(params)
}

/// OpenAPI: header parameters named Accept, Content-Type or Authorization
/// are ignored; those headers are governed by other parts of the spec.
fn is_ignored_header(name: &str) -> bool {
    ["accept", "content-type", "authorization"]
        .iter()
        .any(|h| name.eq_ignore_ascii_case(h))
}

fn compile_param(document: &Document, param: &ParamNode) -> Result<CompiledParam, SpecError> {
    let (schema, schema_validator) = match document.child(&param.pointer, &param.value, "schema") {
        Some((pointer, schema)) => (
            Some(schema.clone()),
            Some(document.compile_schema(&pointer)?),
        ),
        None => (None, None),
    };

    let location = match param.location.as_str() {
        "path" => ParamLocation::Path,
        "query" => ParamLocation::Query,
        "header" => ParamLocation::Header,
        "cookie" => ParamLocation::Cookie,
        other => {
            return Err(SpecError::ParseError(format!(
                "parameter '{}' has unknown location '{other}'",
                param.name
            )))
        }
    };
    let style = ParamStyle::parse(param.value.get("style").and_then(Value::as_str), location)?;
    let explode = param.value.get("explode").and_then(Value::as_bool);

    let required = param
        .value
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(location == ParamLocation::Path);

    Ok(CompiledParam {
        name: param.name.clone(),
        location,
        required,
        style,
        explode,
        schema,
        schema_validator,
    })
}

fn compile_request_body(
    document: &Document,
    op_pointer: &str,
    op: &Value,
) -> Result<Option<CompiledRequestBody>, SpecError> {
    let Some((body_pointer, body)) = document.child(op_pointer, op, "requestBody") else {
        return Ok(None);
    };
    let (body_pointer, body) = document.deref(body_pointer, body)?;

    let required = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut content = Vec::new();
    if let Some((content_pointer, content_obj)) = document.child(&body_pointer, body, "content") {
        let Some(entries) = content_obj.as_object() else {
            return Err(SpecError::ParseError(format!(
                "'content' at {content_pointer} must be an object"
            )));
        };
        for (media_type_str, media) in entries {
            let media_type: Mime = media_type_str.parse().map_err(|e| {
                SpecError::ParseError(format!(
                    "invalid request body media type '{media_type_str}': {e}"
                ))
            })?;
            let media_pointer =
                format!("{content_pointer}/{}", escape_pointer_token(media_type_str));

            let (schema, schema_validator) = match document.child(&media_pointer, media, "schema")
            {
                Some((pointer, schema)) => (
                    Some(schema.clone()),
                    Some(document.compile_schema(&pointer)?),
                ),
                None => (None, None),
            };

            content.push(CompiledMediaType {
                media_type,
                schema,
                schema_validator,
                encoding: media.get("encoding").cloned(),
            });
        }
    }

    Ok(Some(CompiledRequestBody { required, content }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_parse_enables_only_listed_parts() {
        let p = ValidationParts::parse(" Path, query_params ,,").unwrap();
        assert!(p.path && p.query_params);
        assert!(!p.method && !p.body && !p.cookie_params);
    }

    #[test]
    fn parts_parse_all_then_extras() {
        let p = ValidationParts::parse("all,cookie_params").unwrap();
        assert!(p.path && p.body && p.cookie_params);
        assert!(!p.disallow_additional_query_params);
    }

    #[test]
    fn parts_parse_reports_unknown_name() {
        assert_eq!(ValidationParts::parse("path,bogus").unwrap_err(), "bogus");
    }

    #[test]
    fn enforcement_mode_from_str() {
        assert_eq!("AUDIT".parse(), Ok(EnforcementMode::Audit));
        assert_eq!("block".parse(), Ok(EnforcementMode::Block));
        assert_eq!("warn".parse::<EnforcementMode>(), Err(()));
    }

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
    fn test_slash_variants_merge_or_conflict() {
        let json = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {
            "/users": {"get": {"responses": {"200": {"description": "ok"}}}},
            "/users/": {"post": {"responses": {"200": {"description": "ok"}}}}
        }}"#;
        let spec = CompiledSpec::from_json(json).unwrap();
        let m = spec.match_route("/users").unwrap();
        assert!(m.check_method("GET").is_ok());
        assert!(m.check_method("POST").is_ok());

        let json = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {
            "/users": {"get": {"responses": {"200": {"description": "ok"}}}},
            "/users/": {"get": {"responses": {"200": {"description": "ok"}}}}
        }}"#;
        assert!(matches!(CompiledSpec::from_json(json), Err(SpecError::PathTemplateError(..))));
    }

    #[test]
    fn test_head_falls_back_to_get() {
        let spec = build_spec(&["/users"]);
        let m = spec.match_route("/users").unwrap();
        assert!(m.check_method("HEAD").is_ok());
        assert!(m.check_method("head").is_ok());
        assert!(m.check_method("OPTIONS").is_err());
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
        assert!(validator.is_valid(&json!({"owner": {"name": "x"}})));
        assert!(!validator.is_valid(&json!({"owner": {}})));
    }

    #[test]
    fn test_component_request_body_and_parameter_refs() {
        let json = r##"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"},
          "components": {
            "parameters": {"Page": {"name": "page", "in": "query", "schema": {"type": "integer"}}},
            "requestBodies": {"Pet": {"required": true, "content": {"application/json": {"schema": {"type": "object", "required": ["name"]}}}}}
          },
          "paths": {"/pets": {"post": {
            "parameters": [{"$ref": "#/components/parameters/Page"}],
            "requestBody": {"$ref": "#/components/requestBodies/Pet"},
            "responses": {"201": {"description": "ok"}}}}}}"##;
        let spec = CompiledSpec::from_json(json).unwrap();
        let op = spec.match_route("/pets").unwrap().check_method("POST").unwrap();
        assert_eq!(op.query_params[0].name, "page");
        let body = op.request_body.as_ref().unwrap();
        assert!(body.required);
        let validator = body.content[0].schema_validator.as_ref().unwrap();
        assert!(!validator.is_valid(&json!({})));
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
    fn test_unknown_keywords_survive_compilation() {
        // Keywords oas-typed parsers tend to drop must still be enforced.
        let json = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "paths": {"/x": {"post": {
            "requestBody": {"content": {"application/json": {"schema": {
                "type": "object", "not": {"required": ["forbidden"]},
                "patternProperties": {"^n_": {"type": "integer"}}
            }}}},
            "responses": {"200": {"description": "ok"}}}}}}"#;
        let spec = CompiledSpec::from_json(json).unwrap();
        let op = spec.match_route("/x").unwrap().check_method("POST").unwrap();
        let v = op.request_body.as_ref().unwrap().content[0].schema_validator.as_ref().unwrap();
        assert!(!v.is_valid(&json!({"forbidden": 1})));
        assert!(!v.is_valid(&json!({"n_a": "x"})));
        assert!(v.is_valid(&json!({"n_a": 1})));
    }

    #[test]
    fn test_from_json_parse_error() {
        assert!(matches!(
            CompiledSpec::from_json("not json"),
            Err(SpecError::ParseError(_))
        ));
    }
}
