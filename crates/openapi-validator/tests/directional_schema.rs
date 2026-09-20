// Adapted from libopenapi-validator/schema_validation/directional_schema_test.go
// and schema_resources_test.go (request-body cases only).
// Copyright 2023-2026 Princess Beef Heavy Industries, LLC / Dave Shanley
// SPDX-License-Identifier: MIT

mod common;

use common::*;
use serde_json::json;

#[test]
fn request_removes_read_only_required_properties() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "required": ["id", "name", "password"], "properties": {
            "id": {"type": "string", "readOnly": true},
            "name": {"type": "string"},
            "password": {"type": "string", "writeOnly": true}
        }}),
        json!({}),
    );
    assert_valid(validate(
        &spec,
        json!({"name": "Alice", "password": "secret"}),
    ));
}

#[test]
fn request_keeps_write_only_required_properties() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "required": ["id", "name", "password"], "properties": {
            "id": {"type": "string", "readOnly": true},
            "name": {"type": "string"},
            "password": {"type": "string", "writeOnly": true}
        }}),
        json!({}),
    );
    assert_valid(validate(
        &spec,
        json!({"id": "1", "name": "Alice", "password": "secret"}),
    ));
    assert_error_paths(
        validate(&spec, json!({"id": "1", "name": "Alice"})),
        &["body"],
    );
}

#[test]
fn removes_empty_required() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "required": ["id"], "properties": {
            "id": {"type": "string", "readOnly": true}
        }}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({})));
}

#[test]
fn prunes_prefix_items() {
    let spec = compile(
        "3.1.0",
        json!({"type": "array", "prefixItems": [{
            "type": "object", "required": ["id"], "properties": {"id": {"type": "string", "readOnly": true}}
        }]}),
        json!({}),
    );
    assert_valid(validate(&spec, json!([{}])));
}

fn referenced_product() -> openapi_validator::CompiledSpec {
    compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/Product"}),
        json!({
            "Product": {"type": "object", "required": ["id", "name", "secret"], "properties": {
                "id": {"type": "string", "readOnly": true},
                "name": {"type": "string"},
                "secret": {"type": "string", "writeOnly": true}
            }}
        }),
    )
}

#[test]
fn directional_required_across_references_valid_request() {
    assert_valid(validate(
        &referenced_product(),
        json!({"name": "Desk", "secret": "internal"}),
    ));
}

#[test]
fn directional_required_across_references_missing_secret() {
    // Includes the readOnly id to isolate the writeOnly requirement.
    assert_error_paths(
        validate(&referenced_product(), json!({"id": "p1", "name": "Desk"})),
        &["body"],
    );
}
