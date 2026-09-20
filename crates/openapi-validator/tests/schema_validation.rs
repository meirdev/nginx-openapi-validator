// Adapted from libopenapi-validator/schema_validation/validate_schema_test.go.
// Copyright 2023-2026 Princess Beef Heavy Industries, LLC / Dave Shanley
// SPDX-License-Identifier: MIT

mod common;

use common::*;
use openapi_validator::error::{SpecError, ValidationErrorKind};
use openapi_validator::CompiledSpec;
use serde_json::{json, Value};

fn burger_schema() -> Value {
    json!({"type": "object", "properties": {
        "name": {"type": "string"},
        "patties": {"type": "integer"},
        "vegetarian": {"type": "boolean"}
    }})
}

// TestValidateSchema_SimpleValid and TestValidateSchema_SimpleValid_String
// collapse to the same byte-oriented public request API here.
#[test]
fn simple_valid() {
    let spec = compile("3.1.0", burger_schema(), json!({}));
    assert_valid(validate(
        &spec,
        json!({"name": "Big Mac", "patties": 2, "vegetarian": true}),
    ));
}

#[test]
fn simple_invalid_reports_both_properties() {
    let spec = compile("3.1.0", burger_schema(), json!({}));
    assert_error_paths(
        validate(
            &spec,
            json!({"name": "Big Mac", "patties": "I am not a number", "vegetarian": 23}),
        ),
        &["body/patties", "body/vegetarian"],
    );
}

#[test]
fn simple_invalid_multiple_array_items() {
    let mut item = burger_schema();
    item["required"] = json!(["name"]);
    let spec = compile("3.1.0", json!({"type": "array", "items": item}), json!({}));
    assert_error_paths(
        validate(
            &spec,
            json!([
                {"patties": 1, "vegetarian": true},
                {"name": "Quarter Pounder", "patties": true, "vegetarian": false},
                {"name": "Big Mac", "patties": 2, "vegetarian": false}
            ]),
        ),
        &["body/0", "body/1/patties"],
    );
}

#[test]
fn bad_json_is_a_parse_error() {
    let spec = compile("3.1.0", burger_schema(), json!({}));
    let errors = assert_invalid(
        validate_raw(&spec, "application/json", r#"{"bad": "json",}"#),
        ValidationErrorKind::InvalidBody,
    );
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, "body");
}

fn reffy_spec() -> CompiledSpec {
    compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/One"}),
        json!({
            "Death": {"type": "object", "required": ["cakeOrDeath"], "properties": {
                "cakeOrDeath": {"type": "string", "enum": ["death"]}
            }},
            "Cake": {"type": "object", "required": ["cakeOrDeath"], "properties": {
                "cakeOrDeath": {"type": "string", "enum": ["cake please"]}
            }},
            "Four": {"type": "object", "oneOf": [
                {"$ref": "#/components/schemas/Cake"}, {"$ref": "#/components/schemas/Death"}
            ]},
            "Three": {"type": "object", "properties": {
                "name": {"type": "string"}, "four": {"$ref": "#/components/schemas/Four"}
            }},
            "Two": {"type": "object", "properties": {
                "name": {"type": "string"}, "three": {"$ref": "#/components/schemas/Three"}
            }},
            "One": {"type": "object", "properties": {
                "name": {"type": "string"}, "two": {"$ref": "#/components/schemas/Two"}
            }}
        }),
    )
}

#[test]
fn reffy_complex_valid() {
    let spec = reffy_spec();
    for choice in ["cake please", "death"] {
        assert_valid(validate(
            &spec,
            json!({"two": {"three": {"four": {"cakeOrDeath": choice}}}}),
        ));
    }
}

#[test]
fn reffy_complex_invalid() {
    let spec = reffy_spec();
    for choice in [
        "no more cake? so the choice is 'or death?'",
        "i'll have the chicken",
    ] {
        assert_error_paths(
            validate(
                &spec,
                json!({"two": {"three": {"four": {"cakeOrDeath": choice}}}}),
            ),
            &["body/two/three/four"],
        );
    }
}

#[test]
fn v3_1_numeric_exclusive_minimum() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "properties": {
            "amount": {"type": "number", "exclusiveMinimum": 0}
        }}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"amount": 3})));
    // Boundary controls in addition to the upstream positive example.
    for amount in [0, -1] {
        assert_error_paths(validate(&spec, json!({"amount": amount})), &["body/amount"]);
    }
}

fn dependent_schema() -> Value {
    json!({"type": "object", "properties": {"fishCake": {
        "type": "object", "properties": {"bones": {"type": "boolean"}},
        "dependentSchemas": {"fishCake": {
            "type": "object", "properties": {"cream": {"type": "number", "format": "double"}},
            "required": ["cream"]
        }}
    }}})
}

#[test]
fn v3_1_dependent_schemas_original_valid() {
    let spec = compile("3.1.0", dependent_schema(), json!({}));
    assert_valid(validate(
        &spec,
        json!({"fishCake": {"bones": true, "cream": 2.5}}),
    ));
}

#[test]
fn v3_1_dependent_schemas_original_not_valid() {
    let spec = compile("3.1.0", dependent_schema(), json!({}));
    // Upstream's negative payload is malformed JSON, not a dependency violation.
    assert_invalid(
        validate_raw(
            &spec,
            "application/json",
            r#"{
        "fishCake": {"bones": true,}
        "cream": 2.5
    }"#,
        ),
        ValidationErrorKind::InvalidBody,
    );
}

#[test]
fn v3_1_dependent_schemas_triggered() {
    let spec = compile("3.1.0", dependent_schema(), json!({}));
    // Exercise the actual dependency as well: it triggers inside fishCake.
    assert_error_paths(
        validate(&spec, json!({"fishCake": {"fishCake": {}, "bones": true}})),
        &["body/fishCake"],
    );
    assert_valid(validate(
        &spec,
        json!({"fishCake": {"fishCake": {}, "cream": 2.5}}),
    ));
}

#[test]
fn one_of_multiple_matches_issue520() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "oneOf": [
            {"properties": {"pim": {"type": "string"}}},
            {"properties": {"pam": {"type": "string"}}}
        ]}),
        json!({}),
    );
    // Neither branch requires its property, so both match this object.
    assert_error_paths(validate(&spec, json!({"pam": "nop"})), &["body"]);
}

#[test]
fn one_of_discriminant_valid() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "oneOf": [
            {"properties": {"type": {"const": "pim"}, "pim": {"type": "string"}}, "required": ["type", "pim"]},
            {"properties": {"type": {"const": "pam"}, "pam": {"type": "string"}}, "required": ["type", "pam"]}
        ]}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"type": "pam", "pam": "nop"})));
    assert_schema_invalid(validate(&spec, json!({"type": "pim", "pam": "nop"})));
}

#[test]
fn one_of_no_matches() {
    let spec = compile(
        "3.1.0",
        json!({"type": "object", "oneOf": [
            {"properties": {"foo": {"type": "string"}}, "required": ["foo"]},
            {"properties": {"bar": {"type": "integer"}}, "required": ["bar"]}
        ]}),
        json!({}),
    );
    assert_error_paths(validate(&spec, json!({"baz": "invalid"})), &["body"]);
}

#[test]
fn one_of_simple_types() {
    let spec = compile(
        "3.1.0",
        json!({"oneOf": [{"type": "string"}, {"type": "integer"}]}),
        json!({}),
    );
    assert_valid(validate(&spec, json!("hello")));
    assert_valid(validate(&spec, json!(42)));
    assert_schema_invalid(validate(&spec, json!(true)));
}

#[test]
fn one_of_simple_types_ambiguous_pattern() {
    let spec = compile(
        "3.1.0",
        json!({"oneOf": [
            {"type": "string"}, {"type": "string", "pattern": "^[0-9]+$"}
        ]}),
        json!({}),
    );
    assert_error_paths(validate(&spec, json!("123")), &["body"]);
}

fn product_spec() -> CompiledSpec {
    compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/Product"}),
        json!({
            "ProductWidget": {"type": "object", "required": ["productName", "quantity", "color"], "properties": {
                "productName": {"type": "string", "enum": ["Widget"]},
                "quantity": {"type": "integer", "minimum": 1},
                "color": {"type": "string", "enum": ["Red", "Blue", "Green"]}
            }},
            "ProductGadget": {"type": "object", "required": ["productName", "quantity", "size"], "properties": {
                "productName": {"type": "string", "enum": ["Gadget"]},
                "quantity": {"type": "integer", "minimum": 1},
                "size": {"type": "string", "enum": ["Small", "Medium", "Large"]}
            }},
            "Product": {"oneOf": [
                {"$ref": "#/components/schemas/ProductWidget"}, {"$ref": "#/components/schemas/ProductGadget"}
            ], "discriminator": {"propertyName": "productName"}}
        }),
    )
}

#[test]
fn discriminator_one_of_with_refs_issue788() {
    let spec = product_spec();
    assert_valid(validate(
        &spec,
        json!({"productName": "Widget", "quantity": 1, "color": "Green"}),
    ));
    assert_valid(validate(
        &spec,
        json!({"productName": "Gadget", "quantity": 1, "size": "Small"}),
    ));
}

#[test]
fn discriminator_one_of_with_refs_invalid_data() {
    assert_error_paths(
        validate(
            &product_spec(),
            json!({"productName": "Widget", "quantity": 1}),
        ),
        &["body"],
    );
}

#[test]
fn discriminator_any_of_with_refs() {
    let spec = compile(
        "3.1.0",
        json!({"$ref": "#/components/schemas/Pet"}),
        json!({
            "Cat": {"type": "object", "required": ["petType", "meow"], "properties": {
                "petType": {"type": "string", "const": "cat"}, "meow": {"type": "boolean"}
            }},
            "Dog": {"type": "object", "required": ["petType", "bark"], "properties": {
                "petType": {"type": "string", "const": "dog"}, "bark": {"type": "boolean"}
            }},
            "Pet": {"anyOf": [{"$ref": "#/components/schemas/Cat"}, {"$ref": "#/components/schemas/Dog"}],
                "discriminator": {"propertyName": "petType"}}
        }),
    );
    assert_valid(validate(&spec, json!({"petType": "cat", "meow": true})));
    assert_valid(validate(&spec, json!({"petType": "dog", "bark": false})));
    assert_schema_invalid(validate(&spec, json!({"petType": "cat", "bark": true})));
}

#[test]
fn compilation_failure_is_distinct_from_body_validation() {
    // Deterministic adaptation of TestValidateSchema_CompilationFailure:
    // upstream's complex but valid regex only fails on some regex engines.
    let doc = document(
        "3.1.0",
        "application/json",
        json!({"schema": {
            "type": "object", "properties": {"password": {"type": "string", "pattern": "["}}
        }}),
        json!({}),
    );
    assert!(matches!(
        CompiledSpec::from_json(&doc.to_string()),
        Err(SpecError::SchemaCompileError(_))
    ));
}

// validate_schema_coercion_test.go: the default JSON-body behavior is strict.
// The optional Go WithScalarCoercion mode has no equivalent in this crate.
#[test]
fn scalar_coercion_disabled_by_default() {
    let spec = compile(
        "3.0.0",
        json!({"type": "object", "properties": {
            "active": {"type": "boolean"}, "count": {"type": "integer"}
        }}),
        json!({}),
    );
    assert_error_paths(
        validate(&spec, json!({"active": "true", "count": "42"})),
        &["body/active", "body/count"],
    );
    assert_valid(validate(&spec, json!({"active": true, "count": 42})));
}

#[test]
fn scalar_coercion_invalid_strings() {
    let spec = compile(
        "3.0.0",
        json!({"type": "object", "properties": {
            "active": {"type": "boolean"}, "count": {"type": "number"}
        }}),
        json!({}),
    );
    assert_error_paths(
        validate(&spec, json!({"active": "yes", "count": "abc"})),
        &["body/active", "body/count"],
    );
}
