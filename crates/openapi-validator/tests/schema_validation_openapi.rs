// Adapted from libopenapi-validator/schema_validation/validate_schema_openapi_test.go,
// validate_schema_test.go, and schema_resources_test.go.
// Copyright 2023-2026 Princess Beef Heavy Industries, LLC / Dave Shanley
// SPDX-License-Identifier: MIT

mod common;

use common::*;
use openapi_validator::CompiledSpec;
use serde_json::json;

#[test]
fn nullable_keyword_openapi30_success() {
    let spec = compile(
        "3.0.0",
        json!({"type": "object", "required": ["name"], "properties": {
            "name": {"type": "string", "nullable": true}
        }}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"name": null})));
    assert_schema_invalid(validate(&spec, json!({})));
}

#[test]
fn nullable_keyword_openapi31_fails() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "required": ["name"], "properties": {
            "name": {"type": "string", "nullable": true}
        }}),
        json!({}),
    );
    // Compare rejection, not Go's vocabulary-specific error wording.
    assert_error_paths(validate(&spec, json!({"name": null})), &["body/name"]);
}

#[test]
fn multiple_openapi_keywords() {
    let spec = compile(
        "3.0.0",
        json!({"type": "object", "properties": {
            "name": {"type": "string", "nullable": true, "example": "John Doe", "deprecated": true}
        }}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"name": null})));
}

#[test]
fn nullable_enum_openapi30() {
    let spec = compile(
        "3.0.0",
        json!({"type": "object", "required": ["name"], "properties": {
            "name": {"type": "string", "enum": ["mcbird", "mcbeef", "veggie", null], "nullable": true},
            "patties": {"type": "integer"}, "vegetarian": {"type": "boolean"}
        }}),
        json!({}),
    );
    assert_valid(validate(
        &spec,
        json!({"name": null, "patties": 2, "vegetarian": true}),
    ));
    assert_schema_invalid(validate(&spec, json!({"name": "not-on-the-menu"})));
}

#[test]
fn v3_0_numeric_exclusive_minimum_rejected() {
    // TestValidateSchema_v3_0_NumericExclusiveMinimum expects rejection of this
    // schema, even though amount=3 satisfies the Draft 2020-12 interpretation.
    let doc = document(
        "3.0.0",
        "application/json",
        json!({"schema": {
            "type": "object", "properties": {"amount": {"type": "number", "exclusiveMinimum": 0}}
        }}),
        json!({}),
    );
    match CompiledSpec::from_json(&doc.to_string()) {
        Err(
            openapi_validator::error::SpecError::ParseError(_)
            | openapi_validator::error::SpecError::SchemaCompileError(_),
        ) => {}
        Err(error) => panic!("unexpected compilation error: {error}"),
        Ok(spec) => {
            assert_schema_invalid(validate(&spec, json!({"amount": 3})));
        }
    }
}

#[test]
fn discriminator_keyword_valid() {
    let spec = compile(
        "3.0.0",
        json!({"type": "object", "discriminator": {
            "propertyName": "type", "mapping": {
                "dog": "#/components/schemas/Dog", "cat": "#/components/schemas/Cat"
            }
        }}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"type": "dog", "name": "Buddy"})));
}

#[test]
fn circular_reference() {
    let spec = compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/b"}),
        json!({
            "a": {"type": "string", "examples": [""]},
            "b": {"type": "object", "examples": [{"z": ""}], "properties": {
                "z": {"$ref": "#/components/schemas/a"}, "b": {"$ref": "#/components/schemas/b"}
            }},
            "c": {"type": "object", "examples": [{"b": {"z": ""}}], "properties": {
                "b": {"$ref": "#/components/schemas/b"}
            }}
        }),
    );
    assert_valid(validate(&spec, json!({"z": "", "b": {"z": ""}})));
    assert_error_paths(
        validate(&spec, json!({"z": 42, "b": {"z": ""}})),
        &["body/z"],
    );
    assert_error_paths(
        validate(&spec, json!({"z": "", "b": {"z": 42}})),
        &["body/b/z"],
    );
}

#[test]
fn simple_circular_reference() {
    let spec = compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/Node"}),
        json!({
            "Node": {"type": "object", "properties": {
                "value": {"type": "string"}, "next": {"$ref": "#/components/schemas/Node"}
            }}
        }),
    );
    assert_valid(validate(
        &spec,
        json!({"value": "test", "next": {"value": "nested"}}),
    ));
    assert_error_paths(
        validate(&spec, json!({"value": "test", "next": {"value": 42}})),
        &["body/next/value"],
    );
}

#[test]
fn circular_reference_through_array_items() {
    let spec = compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/Error"}),
        json!({
            "Error": {"type": "object", "required": ["code"], "properties": {
                "code": {"type": "string"},
                "details": {"type": "array", "items": {"$ref": "#/components/schemas/Error"}}
            }}
        }),
    );
    assert_valid(validate(
        &spec,
        json!({"code": "root", "details": [{"code": "child"}]}),
    ));
    assert_error_paths(
        validate(&spec, json!({"code": "root", "details": [{"code": 42}]})),
        &["body/details/0/code"],
    );
}

#[test]
fn local_recursive_reference_reports_instance_location() {
    // Rust exposes instance paths, rather than Go's YAML source line/column.
    let spec = compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/Node"}),
        json!({
            "Name": {"type": "string"},
            "Node": {"type": "object", "properties": {
                "name": {"$ref": "#/components/schemas/Name"}, "next": {"$ref": "#/components/schemas/Node"}
            }}
        }),
    );
    assert_error_paths(
        validate(&spec, json!({"name": 42, "next": {"name": "ok"}})),
        &["body/name"],
    );
}
