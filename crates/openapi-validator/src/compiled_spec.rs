use std::collections::HashMap;

use jsonschema::Validator;

use crate::error::{SpecError, ValidationError, ValidationErrorKind};
use crate::path_matcher::PathTree;

/// Pre-compiled OpenAPI spec optimized for per-request validation.
pub struct CompiledSpec {
    /// Trie-based path matcher for O(n) lookup.
    pub path_tree: PathTree,
    /// Operations keyed by (template, uppercase method).
    pub operations: HashMap<String, HashMap<String, CompiledOperation>>,
}

impl Default for CompiledSpec {
    fn default() -> Self {
        Self {
            path_tree: PathTree::default(),
            operations: HashMap::new(),
        }
    }
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
    /// Keyed by media type (e.g., "application/json")
    pub content: HashMap<String, CompiledMediaType>,
}

pub struct CompiledMediaType {
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
    pub template: &'a str,
    pub params: Vec<(&'a str, &'a str)>,
}

impl CompiledSpec {
    /// Find a route matching the given path.
    pub fn match_route<'a>(&'a self, request_path: &'a str) -> Option<MatchedRoute<'a>> {
        let m = self.path_tree.match_path(request_path)?;
        Some(MatchedRoute {
            template: m.template,
            params: m.params,
        })
    }

    /// Check if a method is allowed for a matched route.
    pub fn check_method<'a>(
        &'a self,
        template: &str,
        method: &str,
    ) -> Result<&'a CompiledOperation, ValidationError> {
        let methods = self.operations.get(template).ok_or_else(|| ValidationError {
            kind: ValidationErrorKind::PathNotFound,
            message: format!("No operations found for path '{template}'"),
            path: "path".to_string(),
        })?;

        methods
            .get(&method.to_ascii_uppercase())
            .ok_or_else(|| {
                let allowed: Vec<&String> = methods.keys().collect();
                ValidationError {
                    kind: ValidationErrorKind::MethodNotAllowed,
                    message: format!(
                        "Method '{method}' is not allowed for path '{template}'. Allowed: {allowed:?}",
                    ),
                    path: "method".to_string(),
                }
            })
    }
}

/// Compile an oas3::Spec into a CompiledSpec for fast per-request validation.
pub fn compile_spec(spec: &oas3::Spec) -> Result<CompiledSpec, SpecError> {
    let paths = match &spec.paths {
        Some(paths) => paths,
        None => return Ok(CompiledSpec::default()),
    };

    let mut path_tree = PathTree::default();
    let mut operations: HashMap<String, HashMap<String, CompiledOperation>> = HashMap::new();

    for (path_template, path_item) in paths {
        path_tree.insert(path_template)?;

        // Collect path-level parameters
        let path_level_params: Vec<oas3::spec::Parameter> = path_item
            .parameters
            .iter()
            .filter_map(|p| resolve_param(p, spec))
            .collect();

        let mut route_operations = HashMap::new();

        for (method, operation) in path_item.methods() {
            let method_str = format!("{:?}", method).to_ascii_uppercase();

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

            route_operations.insert(
                method_str,
                CompiledOperation {
                    path_params,
                    query_params,
                    header_params,
                    cookie_params,
                    request_body,
                },
            );
        }

        operations.insert(path_template.clone(), route_operations);
    }

    Ok(CompiledSpec {
        path_tree,
        operations,
    })
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
        Some(schema_or_ref) => {
            let schema_obj = resolve_schema(schema_or_ref, spec)?;
            let schema_json = serde_json::to_value(&schema_obj)
                .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;
            Some(
                jsonschema::validator_for(&schema_json)
                    .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?,
            )
        }
        None => None,
    };

    Ok(CompiledParam {
        name: param.name.clone(),
        required: param.required.unwrap_or(false),
        schema_validator,
    })
}

fn resolve_schema(
    schema_or_ref: &oas3::spec::ObjectOrReference<oas3::spec::ObjectSchema>,
    spec: &oas3::Spec,
) -> Result<oas3::spec::ObjectSchema, SpecError> {
    match schema_or_ref {
        oas3::spec::ObjectOrReference::Object(s) => Ok(s.clone()),
        oas3::spec::ObjectOrReference::Ref { ref_path, .. } => {
            let name = ref_path
                .rsplit('/')
                .next()
                .ok_or_else(|| SpecError::RefResolutionError(ref_path.clone()))?;
            let components = spec
                .components
                .as_ref()
                .ok_or_else(|| SpecError::RefResolutionError(ref_path.clone()))?;
            let resolved = components
                .schemas
                .get(name)
                .ok_or_else(|| SpecError::RefResolutionError(ref_path.clone()))?;
            resolve_schema(resolved, spec)
        }
    }
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
    let mut content = HashMap::new();

    for (media_type_str, media_type) in &body.content {
        let schema_validator = match &media_type.schema {
            Some(schema_or_ref) => {
                let schema_obj = resolve_schema(schema_or_ref, spec)?;
                let schema_json = serde_json::to_value(&schema_obj)
                    .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?;
                Some(
                    jsonschema::validator_for(&schema_json)
                        .map_err(|e| SpecError::SchemaCompileError(e.to_string()))?,
                )
            }
            None => None,
        };

        content.insert(
            media_type_str.clone(),
            CompiledMediaType { schema_validator },
        );
    }

    Ok(Some(CompiledRequestBody { required, content }))
}
