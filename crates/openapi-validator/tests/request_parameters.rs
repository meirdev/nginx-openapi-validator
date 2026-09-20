// Adapted from libopenapi-validator/parameters/{query,header,cookie,path}_parameters_test.go.
// Copyright 2023-2025 Princess Beef Heavy Industries, LLC / Dave Shanley
// SPDX-License-Identifier: MIT

use openapi_validator::error::{ValidationErrorKind, ValidationResult};
use openapi_validator::{validate_request, CompiledSpec, RequestData, ValidationConfig};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    source: String,
    template: String,
    parameter: Value,
    request: Request,
    expected: Expected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    method: String,
    path: String,
    query_string: Option<String>,
    headers: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    valid: bool,
    error_kind: Option<String>,
    error_path: Option<String>,
}

fn cases() -> &'static BTreeMap<String, Case> {
    static CASES: OnceLock<BTreeMap<String, Case>> = OnceLock::new();
    CASES.get_or_init(|| {
        serde_json::from_str(include_str!("fixtures/request_parameters.json")).unwrap()
    })
}

fn run(name: &str) {
    let case = &cases()[name];
    let doc = json!({
        "openapi": "3.1.0", "info": {"title": "Request parameter parity", "version": "1.0.0"},
        "paths": {&case.template: {case.request.method.to_lowercase(): {
            "parameters": [&case.parameter], "responses": {"200": {"description": "OK"}}
        }}}
    });
    let spec = CompiledSpec::from_json(&doc.to_string())
        .unwrap_or_else(|error| panic!("{}: spec did not compile: {error}", case.source));
    let request = RequestData {
        method: case.request.method.clone(),
        path: case.request.path.clone(),
        query_string: case.request.query_string.clone(),
        headers: case.request.headers.clone(),
        body: None,
    };
    let mut config = ValidationConfig::default();
    config.parts.cookie_params = true;
    let result = validate_request(&spec, &request, &config);
    assert_eq!(
        result.is_valid(),
        case.expected.valid,
        "{}\n{result:#?}",
        case.source
    );
    if case.expected.valid {
        assert_eq!(result.http_status(), None, "{}", case.source);
        assert!(case.expected.error_kind.is_none() && case.expected.error_path.is_none());
        return;
    }
    let kind = match case.expected.error_kind.as_deref().unwrap() {
        "path_not_found" => ValidationErrorKind::PathNotFound,
        "missing_required_param" => ValidationErrorKind::MissingRequiredParam,
        "invalid_param_value" => ValidationErrorKind::InvalidParamValue,
        other => panic!("unrecognized expected kind: {other}"),
    };
    assert_eq!(
        result.http_status(),
        Some(kind.http_status()),
        "{}",
        case.source
    );
    let ValidationResult::Invalid(errors) = result else {
        unreachable!()
    };
    assert!(!errors.is_empty());
    for error in errors {
        assert_eq!(error.kind, kind, "{}: {error:#?}", case.source);
        assert_eq!(
            Some(error.path.as_str()),
            case.expected.error_path.as_deref(),
            "{}: {error:#?}",
            case.source
        );
        assert!(!error.message.is_empty());
    }
}

macro_rules! parameter_case {
    ($name:ident) => {
        #[test]
        fn $name() {
            run(stringify!($name));
        }
    };
}

// Each invocation is independently runnable and links to its Go source through
// the fixture's `source` field. Expectations come from upstream assertions.
parameter_case!(query_array_max_items);
parameter_case!(query_array_min_items);
parameter_case!(query_array_unique_items);
parameter_case!(query_deep_object_invalid_leaf);
parameter_case!(query_nested_deep_object);
parameter_case!(query_param_missing);
parameter_case!(query_param_not_missing);
parameter_case!(query_param_minimum_length_violation);
parameter_case!(query_param_minimum);
parameter_case!(query_param_maximum);
parameter_case!(query_param_post);
parameter_case!(query_param_put);
parameter_case!(query_param_delete);
parameter_case!(query_param_options);
parameter_case!(query_param_head);
parameter_case!(query_param_patch);
parameter_case!(query_param_trace);
parameter_case!(query_param_bad_path);
parameter_case!(query_param_wrong_type_number);
parameter_case!(query_param_valid_type_number);
parameter_case!(query_param_minimum_number);
parameter_case!(query_param_minimum_number_violation);
parameter_case!(query_param_maximum_number);
parameter_case!(query_param_maximum_number_violation);
parameter_case!(query_param_valid_type_float);
parameter_case!(query_param_wrong_type_integer_string_value);
parameter_case!(query_param_wrong_type_integer_float_value);
parameter_case!(query_param_valid_type_integer);
parameter_case!(query_param_minimum_integer);
parameter_case!(query_param_minimum_integer_violation);
parameter_case!(query_param_maximum_integer);
parameter_case!(query_param_maximum_integer_violation);
parameter_case!(query_param_wrong_type_bool);
parameter_case!(query_param_valid_type_bool);
parameter_case!(query_param_invalid_enum_string);
parameter_case!(query_param_invalid_enum_integer);
parameter_case!(query_param_valid_enum_integer);
parameter_case!(query_param_invalid_enum_number);
parameter_case!(query_param_valid_enum_number);
parameter_case!(query_param_valid_type_array_string);
parameter_case!(query_param_invalid_type_array_string_enum);
parameter_case!(query_param_invalid_type_array_integer_enum);
parameter_case!(query_param_invalid_type_array_integer);
parameter_case!(query_param_invalid_type_array_number_enum);
parameter_case!(query_param_invalid_type_array_number);
parameter_case!(query_param_valid_enum_string_type);
parameter_case!(query_param_valid_exploded_type);
parameter_case!(query_param_valid_exploded_array);
parameter_case!(query_param_invalid_exploded_array_and_invalid_type);
parameter_case!(query_param_valid_exploded);
parameter_case!(query_param_invalid_type_array_bool);
parameter_case!(query_param_invalid_type_array_float);
parameter_case!(query_param_invalid_type_array_float_pipe_delimited);
parameter_case!(query_param_invalid_type_array_object_pipe_delimited);
parameter_case!(header_param_missing);
parameter_case!(header_path_missing);
parameter_case!(header_param_default_encoding_invalid_param_type_integer);
parameter_case!(header_param_default_encoding_invalid_param_type_number);
parameter_case!(header_param_default_encoding_invalid_param_type_boolean);
parameter_case!(header_param_default_encoding_invalid_param_type_object_invalid);
parameter_case!(header_param_default_encoding_invalid_param_type_object_integer);
parameter_case!(header_param_default_encoding_invalid_param_type_object_number);
parameter_case!(header_param_default_encoding_valid_param_type_object_boolean);
parameter_case!(header_param_invalid_simple_encoding);
parameter_case!(header_param_non_default_encoding_valid_param_type_object);
parameter_case!(header_param_non_default_encoding_invalid_param_type_object_number);
parameter_case!(header_param_non_default_encoding_invalid_param_type_object_integer);
parameter_case!(header_param_non_default_encoding_valid_param_type_array_string);
parameter_case!(header_param_non_default_encoding_valid_param_type_array_number);
parameter_case!(header_param_non_default_encoding_valid_param_type_array_integer);
parameter_case!(header_param_non_default_encoding_valid_param_type_array_bool);
parameter_case!(header_param_non_default_encoding_invalid_param_type_array_number);
parameter_case!(header_param_non_default_encoding_invalid_param_type_array_bool);
parameter_case!(header_param_string_valid_enum);
parameter_case!(header_param_string_invalid_enum);
parameter_case!(header_param_integer_valid_enum);
parameter_case!(header_param_number_invalid_enum);
parameter_case!(header_param_integer_invalid_enum);
parameter_case!(header_param_string_valid_pattern);
parameter_case!(header_param_string_invalid_pattern);
parameter_case!(header_param_string_valid_min_length);
parameter_case!(header_param_string_invalid_min_length);
parameter_case!(header_param_string_valid_max_length);
parameter_case!(header_param_string_invalid_max_length);
parameter_case!(header_param_string_valid_pattern_and_min_max_length);
parameter_case!(header_param_string_invalid_pattern_but_valid_length);
parameter_case!(header_param_string_valid_enum_and_pattern);
parameter_case!(cookie_no_path);
parameter_case!(cookie_param_number_valid);
parameter_case!(cookie_param_number_valid_float);
parameter_case!(cookie_param_number_invalid);
parameter_case!(cookie_param_integer_valid);
parameter_case!(cookie_param_integer_invalid);
parameter_case!(cookie_param_boolean_valid);
parameter_case!(cookie_param_enum_valid_string);
parameter_case!(cookie_param_enum_invalid_string);
parameter_case!(cookie_param_boolean_invalid);
parameter_case!(cookie_param_object_valid);
parameter_case!(cookie_param_object_invalid);
parameter_case!(cookie_param_array_valid_number);
parameter_case!(cookie_param_array_invalid_number);
parameter_case!(cookie_param_array_valid_integer);
parameter_case!(cookie_param_array_invalid_integer);
parameter_case!(cookie_param_array_valid_boolean);
parameter_case!(cookie_param_array_string);
parameter_case!(cookie_param_array_invalid_boolean);
parameter_case!(cookie_param_array_invalid_boolean_zero_one);
parameter_case!(cookie_param_array_valid_integer_enum);
parameter_case!(cookie_param_array_invalid_integer_enum);
parameter_case!(cookie_param_array_valid_number_enum);
parameter_case!(cookie_param_array_invalid_number_enum);
parameter_case!(cookie_required_missing);
parameter_case!(cookie_optional_missing);
parameter_case!(cookie_optional_missing_no_required_field);
parameter_case!(cookie_case_sensitive);
parameter_case!(cookie_required_with_invalid_value);
parameter_case!(cookie_required_integer_missing);
parameter_case!(cookie_required_boolean_missing);
parameter_case!(cookie_required_string_missing);
parameter_case!(cookie_required_array_missing);
parameter_case!(cookie_required_object_missing);
parameter_case!(cookie_param_string_valid_pattern);
parameter_case!(cookie_param_string_invalid_pattern);
parameter_case!(cookie_param_string_valid_min_length);
parameter_case!(cookie_param_string_invalid_min_length);
parameter_case!(cookie_param_string_valid_max_length);
parameter_case!(cookie_param_string_invalid_max_length);
parameter_case!(cookie_param_string_valid_pattern_and_min_max_length);
parameter_case!(cookie_param_string_invalid_pattern_but_valid_length);
parameter_case!(cookie_param_missing_required);
parameter_case!(simple_array_encoded_path);
parameter_case!(path_param_url_encoding_integer_enum);
parameter_case!(simple_array_encoded_path_invalid_integer);
parameter_case!(simple_array_encoded_path_invalid_number);
parameter_case!(simple_array_encoded_path_invalid_bool);
parameter_case!(simple_object_encoded_path);
parameter_case!(simple_object_encoded_path_invalid);
parameter_case!(simple_object_encoded_path_exploded);
parameter_case!(simple_object_encoded_path_exploded_invalid);
parameter_case!(object_encoded_path);
parameter_case!(simple_encoded_path_invalid_integer);
parameter_case!(simple_encoded_path_minimum_integer_violation);
parameter_case!(simple_encoded_path_minimum_integer);
parameter_case!(simple_encoded_path_maximum_integer_violation);
parameter_case!(simple_encoded_path_maximum_integer);
parameter_case!(simple_encoded_path_invalid_number);
parameter_case!(simple_encoded_path_minimum_number_violation);
parameter_case!(simple_encoded_path_minimum_number);
parameter_case!(simple_encoded_path_maximum_number_violation);
parameter_case!(simple_encoded_path_maximum_number);
parameter_case!(simple_encoded_path_invalid_boolean);
parameter_case!(label_encoded_path_invalid_integer);
parameter_case!(label_encoded_path_minimum_integer_violation);
parameter_case!(label_encoded_path_maximum_integer_violation);
parameter_case!(label_encoded_path_invalid_boolean);
parameter_case!(label_encoded_path_valid_boolean);
parameter_case!(label_encoded_path_valid_array_integer);
parameter_case!(label_encoded_path_valid_array_integer_exploded);
parameter_case!(label_encoded_path_invalid_array_integer_exploded);
parameter_case!(label_encoded_path_invalid_array_integer);
parameter_case!(label_encoded_path_valid_array_number);
parameter_case!(label_encoded_path_valid_array_number_exploded);
parameter_case!(label_encoded_path_invalid_array_number_exploded);
parameter_case!(label_encoded_path_invalid_array_number);
parameter_case!(label_encoded_path_invalid_object);
parameter_case!(label_encoded_path_invalid_object_exploded);
parameter_case!(matrix_encoded_path_valid_integer);
parameter_case!(matrix_encoded_path_invalid_integer);
parameter_case!(matrix_encoded_path_minimum_integer_violation);
parameter_case!(matrix_encoded_path_maximum_integer_violation);
parameter_case!(matrix_encoded_path_invalid_number);
parameter_case!(matrix_encoded_path_minimum_number_violation);
parameter_case!(matrix_encoded_path_maximum_number_violation);
parameter_case!(matrix_encoded_path_valid_primitive_boolean);
parameter_case!(matrix_encoded_path_invalid_primitive_boolean);
parameter_case!(matrix_encoded_path_valid_object);
parameter_case!(matrix_encoded_path_invalid_object);
parameter_case!(matrix_encoded_path_valid_object_exploded);
parameter_case!(matrix_encoded_path_invalid_object_exploded);
parameter_case!(matrix_encoded_path_valid_array);
parameter_case!(matrix_encoded_path_invalid_array);
parameter_case!(matrix_encoded_path_valid_array_exploded);
parameter_case!(matrix_encoded_path_invalid_array_exploded);
parameter_case!(path_params_path_not_found);
parameter_case!(path_param_string_enum_valid);
parameter_case!(path_param_string_enum_invalid);
parameter_case!(path_param_string_min_length_violation);
parameter_case!(path_param_string_max_length_violation);
parameter_case!(path_param_integer_enum_valid);
parameter_case!(path_param_integer_enum_invalid);
parameter_case!(path_param_number_enum_valid);
parameter_case!(path_param_number_enum_invalid);
parameter_case!(path_label_eum_valid);
parameter_case!(path_label_eum_invalid);
parameter_case!(path_matrix_eum_invalid);
parameter_case!(mandatory_path_segment_empty);

fn strict_query_result(query: &str) -> ValidationResult {
    // TestNewValidator_QueryParams_StrictMode_{UndeclaredParam,ValidRequest}.
    // Our equivalent setting is disallow_additional_query_params.
    let doc = json!({
        "openapi":"3.1.0", "info":{"title":"Query strict mode parity","version":"1.0.0"},
        "paths":{"/api/search":{"get":{
            "parameters":[
                {"name":"query","in":"query","required":true,"schema":{"type":"string"}},
                {"name":"limit","in":"query","schema":{"type":"integer"}}
            ], "responses":{"200":{"description":"OK"}}
        }}}
    });
    let spec = CompiledSpec::from_json(&doc.to_string()).unwrap();
    let mut config = ValidationConfig::default();
    config.parts.disallow_additional_query_params = true;
    validate_request(
        &spec,
        &RequestData {
            method: "GET".into(),
            path: "/api/search".into(),
            query_string: Some(query.into()),
            headers: vec![],
            body: None,
        },
        &config,
    )
}

#[test]
fn strict_query_undeclared_parameter() {
    let result = strict_query_result("query=test&limit=10&extra=undeclared");
    assert_eq!(result.http_status(), Some(400));
    let ValidationResult::Invalid(errors) = result else {
        panic!("expected rejection")
    };
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind, ValidationErrorKind::InvalidParamValue);
    assert_eq!(errors[0].path, "query.extra");
    assert!(!errors[0].message.is_empty());
}

#[test]
fn strict_query_valid_request() {
    let result = strict_query_result("query=test&limit=10");
    assert!(result.is_valid(), "{result:#?}");
    assert_eq!(result.http_status(), None);
}
