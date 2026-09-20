#![allow(dead_code)]

use openapi_validator::error::{ValidationError, ValidationErrorKind, ValidationResult};
use openapi_validator::{validate_request, CompiledSpec, RequestData, ValidationConfig};
use serde_json::{json, Value};

pub fn document(version: &str, media_type: &str, media: Value, schemas: Value) -> Value {
    json!({
        "openapi": version,
        "info": {"title": "libopenapi-validator parity", "version": "1.0.0"},
        "paths": {"/test": {"post": {
            "requestBody": {"required": true, "content": {media_type: media}},
            "responses": {"200": {"description": "OK"}}
        }}},
        "components": {"schemas": schemas}
    })
}

pub fn compile(version: &str, schema: Value, schemas: Value) -> CompiledSpec {
    compile_media(
        version,
        "application/json",
        json!({"schema": schema}),
        schemas,
    )
}

pub fn compile_media(
    version: &str,
    media_type: &str,
    media: Value,
    schemas: Value,
) -> CompiledSpec {
    let doc = document(version, media_type, media, schemas);
    CompiledSpec::from_json(&doc.to_string())
        .unwrap_or_else(|error| panic!("reference schema must compile: {error}\n{doc}"))
}

pub fn validate_raw(spec: &CompiledSpec, media_type: &str, body: &str) -> ValidationResult {
    validate_request(
        spec,
        &RequestData {
            method: "POST".into(),
            path: "/test".into(),
            query_string: None,
            headers: vec![("Content-Type".into(), media_type.into())],
            body: Some(body.as_bytes().to_vec()),
        },
        &ValidationConfig::default(),
    )
}

pub fn validate(spec: &CompiledSpec, body: Value) -> ValidationResult {
    validate_raw(spec, "application/json", &body.to_string())
}

#[track_caller]
pub fn assert_valid(result: ValidationResult) {
    assert!(
        result.is_valid(),
        "expected a valid request, got {result:#?}"
    );
    assert_eq!(result.http_status(), None);
}

#[track_caller]
pub fn assert_invalid(result: ValidationResult, kind: ValidationErrorKind) -> Vec<ValidationError> {
    assert_eq!(result.http_status(), Some(400), "{result:#?}");
    match result {
        ValidationResult::Invalid(errors) => {
            assert!(
                !errors.is_empty(),
                "invalid results must explain the failure"
            );
            for error in &errors {
                assert_eq!(error.kind, kind, "{error:#?}");
                assert!(!error.message.is_empty(), "{error:#?}");
                assert!(error.path.starts_with("body"), "{error:#?}");
            }
            errors
        }
        ValidationResult::Valid => panic!("expected rejection"),
    }
}

#[track_caller]
pub fn assert_schema_invalid(result: ValidationResult) -> Vec<ValidationError> {
    assert_invalid(result, ValidationErrorKind::SchemaValidation)
}

#[track_caller]
pub fn assert_error_paths(result: ValidationResult, expected: &[&str]) {
    let errors = assert_schema_invalid(result);
    let mut paths: Vec<_> = errors.iter().map(|error| error.path.as_str()).collect();
    paths.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(paths, expected, "{errors:#?}");
}
