// Adapted from libopenapi-validator/requests/validate_body_test.go,
// validate_request_test.go, and kin_parity_test.go.
// Copyright 2023-2026 Princess Beef Heavy Industries, LLC / Dave Shanley
// SPDX-License-Identifier: MIT

mod common;

use common::{
    assert_error_paths, assert_schema_invalid, assert_valid, compile, compile_media, validate,
};
use openapi_validator::error::{ValidationErrorKind as Kind, ValidationResult};
use openapi_validator::{validate_request, CompiledSpec, RequestData, ValidationConfig};
use serde_json::{json, Value};
use std::sync::OnceLock;

const BURGER_PATH: &str = "/burgers/createBurger";
const GOOD: &str = r#"{"name":"Big Mac","patties":2,"vegetarian":true}"#;
const BAD: &str = r#"{"name":"Big Mac","patties":false,"vegetarian":2}"#;
const NUTRIENTS: &str =
    r#"{"name":"Big Mac","patties":2,"vegetarian":true,"fat":10.0,"salt":0.5,"meat":"beef"}"#;

fn fixture(name: &str) -> Value {
    static FIXTURES: OnceLock<Value> = OnceLock::new();
    let fixtures = FIXTURES.get_or_init(|| {
        serde_json::from_str(include_str!("fixtures/request_body_specs.json")).unwrap()
    });
    assert!(
        fixtures.get(name).is_some(),
        "missing upstream fixture {name}"
    );
    fixtures[name]["spec"].clone()
}

fn compiled(name: &str) -> CompiledSpec {
    CompiledSpec::from_json(&fixture(name).to_string())
        .unwrap_or_else(|error| panic!("upstream fixture {name} must compile: {error}"))
}

fn request(
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Option<&str>,
) -> RequestData {
    RequestData {
        method: method.into(),
        path: path.into(),
        query_string: None,
        headers: content_type
            .into_iter()
            .map(|value| ("Content-Type".into(), value.into()))
            .collect(),
        body: body.map(|value| value.as_bytes().to_vec()),
    }
}

fn run(spec: &CompiledSpec, request: &RequestData) -> ValidationResult {
    validate_request(spec, request, &ValidationConfig::default())
}

#[track_caller]
fn assert_errors(result: ValidationResult, expected: &[(Kind, &str)]) {
    assert_eq!(
        result.http_status(),
        expected.first().map(|(kind, _)| kind.http_status()),
        "{result:#?}"
    );
    let ValidationResult::Invalid(errors) = result else {
        panic!("expected rejection")
    };
    // Order of independent schema errors is not part of the public contract.
    let mut actual: Vec<_> = errors
        .iter()
        .map(|error| {
            assert!(!error.message.is_empty());
            (format!("{:?}", error.kind), error.path.as_str())
        })
        .collect();
    let mut expected: Vec<_> = expected
        .iter()
        .map(|(kind, path)| (format!("{kind:?}"), *path))
        .collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected, "{errors:#?}");
}

macro_rules! body_case {
    ($name:ident, $fixture:literal, $method:literal, $path:expr, $ct:expr, $body:expr, $errors:expr) => {
        #[test]
        fn $name() {
            let spec = compiled($fixture);
            let request = request($method, $path, $ct, $body);
            let expected: &[(Kind, &str)] = $errors;
            // Repeated validation must give the same result and preserve data.
            for _ in 0..2 {
                let result = run(&spec, &request);
                if expected.is_empty() {
                    assert_valid(result);
                } else {
                    assert_errors(result, expected);
                }
            }
            assert_eq!(
                request.body.as_deref(),
                ($body as Option<&str>).map(str::as_bytes)
            );
        }
    };
}

body_case!(
    not_required_body_without_content_type,
    "NotRequiredBody",
    "POST",
    BURGER_PATH,
    None,
    None,
    &[]
);
body_case!(
    optional_empty_body_issue146,
    "OptionalRequestBody_EmptyBody",
    "POST",
    "/test",
    Some("application/json"),
    None,
    &[]
);
body_case!(
    optional_explicit_empty_body,
    "OptionalRequestBody_EmptyBody",
    "POST",
    "/test",
    Some("application/json"),
    Some(""),
    &[]
);
body_case!(
    missing_required_body,
    "MissingBody",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    None,
    &[(Kind::MissingRequiredBody, "body")]
);
body_case!(
    explicit_empty_required_body,
    "MissingBody",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(""),
    &[(Kind::MissingRequiredBody, "body")]
);
body_case!(
    schema_has_no_request_body,
    "SchemaHasNoRequestBody",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    None,
    &[]
);
body_case!(
    no_body_no_content_type,
    "SchemaHasNoRequestBody",
    "POST",
    BURGER_PATH,
    None,
    None,
    &[]
);
body_case!(
    media_type_has_null_schema,
    "MediaTypeHasNullSchema",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    None,
    &[]
);
body_case!(
    media_range_wildcard_end,
    "MediaRangeContentType_Wildcard_end",
    "POST",
    BURGER_PATH,
    Some("thomas/tank-engine"),
    Some(BAD),
    &[]
);
body_case!(
    media_range_wildcards,
    "MediaRangeContentType_Wildcards",
    "POST",
    BURGER_PATH,
    Some("thomas/tank-engine"),
    Some(BAD),
    &[]
);
body_case!(
    media_range_json_validates_data,
    "InvalidBasicSchema_MediaRangeContentType_Wildcard_Required",
    "POST",
    BURGER_PATH,
    Some("foo/json"),
    Some(BAD),
    &[
        (Kind::SchemaValidation, "body/patties"),
        (Kind::SchemaValidation, "body/vegetarian")
    ]
);
body_case!(
    unknown_content_type,
    "ValidBasicSchema",
    "POST",
    BURGER_PATH,
    Some("thomas/tank-engine"),
    Some(GOOD),
    &[(Kind::UnsupportedContentType, "header.Content-Type")]
);
body_case!(
    content_type_not_set,
    "ValidBasicSchema",
    "POST",
    BURGER_PATH,
    None,
    Some(GOOD),
    &[(Kind::UnsupportedContentType, "header.Content-Type")]
);
body_case!(
    path_not_found,
    "ValidBasicSchema",
    "POST",
    "/I do not exist",
    Some("application/json"),
    Some(GOOD),
    &[(Kind::PathNotFound, "path")]
);
body_case!(
    operation_not_found,
    "ValidBasicSchema",
    "GET",
    BURGER_PATH,
    Some("application/json"),
    Some(GOOD),
    &[(Kind::MethodNotAllowed, "method")]
);
body_case!(
    valid_basic_schema,
    "ValidBasicSchema",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(GOOD),
    &[]
);
body_case!(
    invalid_optional_body_is_still_validated,
    "NotRequiredBody",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(BAD),
    &[
        (Kind::SchemaValidation, "body/patties"),
        (Kind::SchemaValidation, "body/vegetarian")
    ]
);
body_case!(
    full_content_type_header,
    "ValidBasicSchema",
    "POST",
    BURGER_PATH,
    Some("application/json; charset=utf-8; boundary=12345"),
    Some(GOOD),
    &[]
);
body_case!(
    valid_schema_using_all_of,
    "ValidSchemaUsingAllOf",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(NUTRIENTS),
    &[]
);
body_case!(
    invalid_schema_using_all_of,
    "ValidSchemaUsingAllOf",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(
        r#"{"name":"Big Mac","patties":2,"vegetarian":true,"fat":10.0,"salt":false,"meat":"turkey"}"#
    ),
    &[
        (Kind::SchemaValidation, "body/salt"),
        (Kind::SchemaValidation, "body/meat")
    ]
);
// Upstream calls this AllOfAnyOf, but its actual schema combines allOf and oneOf.
body_case!(
    valid_schema_using_all_of_one_of,
    "ValidSchemaUsingAllOfAnyOf",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(
        r#"{"name":"Big Mac","patties":2,"vegetarian":true,"fat":10.0,"salt":0.5,"meat":"beef","usedOil":true,"usedAnimalFat":false}"#
    ),
    &[]
);
body_case!(
    invalid_schema_using_one_of,
    "ValidSchemaUsingAllOfAnyOf",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(NUTRIENTS),
    &[(Kind::SchemaValidation, "body")]
);
body_case!(
    invalid_schema_min_max,
    "InvalidSchemaMinMax",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(r#"{"name":"Big Mac","patties":5,"vegetarian":true,"fat":10.0,"salt":0.5,"meat":"beef"}"#),
    &[(Kind::SchemaValidation, "body/patties")]
);
body_case!(
    invalid_schema_bad_decode,
    "InvalidSchemaMinMax",
    "POST",
    BURGER_PATH,
    Some("application/json"),
    Some(r#"{"bad":"json",}"#),
    &[(Kind::InvalidBody, "body")]
);
body_case!(
    schema_no_type_issue75,
    "SchemaNoType_Issue75",
    "PUT",
    "/path1",
    Some("application/json"),
    None,
    &[(Kind::MissingRequiredBody, "body")]
);
body_case!(
    urlencoded_request_integer,
    "URLEncodedRequest",
    "POST",
    BURGER_PATH,
    Some("application/x-www-form-urlencoded"),
    Some("name=cheeseburger&patties=23"),
    &[]
);
body_case!(
    urlencoded_request_fraction_rejected,
    "URLEncodedRequest",
    "POST",
    BURGER_PATH,
    Some("application/x-www-form-urlencoded"),
    Some("name=cheeseburger&patties=23.4"),
    &[(Kind::SchemaValidation, "body/patties")]
);
body_case!(
    xml_request_fragment,
    "XmlRequest",
    "POST",
    BURGER_PATH,
    Some("application/xml"),
    Some("<name>cheeseburger</name><cost>23</cost>"),
    &[]
);
body_case!(
    xml_malformed_request_empty,
    "XmlMalformedRequest",
    "POST",
    BURGER_PATH,
    Some("application/xml"),
    Some(""),
    &[(Kind::InvalidBody, "body")]
);
body_case!(
    xml_request_transformations,
    "XmlRequestTransformations",
    "POST",
    BURGER_PATH,
    Some("application/xml"),
    Some("<Burger><name>cheeseburger</name><cost>23</cost></Burger>"),
    &[]
);

#[test]
fn invalid_schema_max_items() {
    let spec = compiled("InvalidSchemaMaxItems");
    let item: Value = serde_json::from_str(NUTRIENTS).unwrap();
    let body = json!([item, item, item, item]).to_string();
    assert_errors(
        run(
            &spec,
            &request("POST", BURGER_PATH, Some("application/json"), Some(&body)),
        ),
        &[(Kind::SchemaValidation, "body")],
    );
}

#[test]
fn schema_min_max_boundaries() {
    // Added controls for the upstream out-of-range example.
    let spec = compiled("InvalidSchemaMinMax");
    for patties in [0, 1, 3, 4] {
        let body = json!({"name":"Big Mac", "patties":patties, "vegetarian":true}).to_string();
        let result = run(
            &spec,
            &request("POST", BURGER_PATH, Some("application/json"), Some(&body)),
        );
        if patties == 0 || patties == 4 {
            assert_errors(result, &[(Kind::SchemaValidation, "body/patties")]);
        } else {
            assert_valid(result);
        }
    }
}

#[test]
fn schema_no_type_any_of_data_controls() {
    // Added data controls for issue75's schema (upstream only sends no body).
    let spec = compiled("SchemaNoType_Issue75");
    for (body, valid) in [
        (json!({"name":"John"}), true),
        (json!({"email":"john@example.com"}), true),
        (json!({"name":""}), false),
        (json!({"age":42}), false),
    ] {
        let result = run(
            &spec,
            &request(
                "PUT",
                "/path1",
                Some("application/json"),
                Some(&body.to_string()),
            ),
        );
        if valid {
            assert_valid(result);
        } else {
            assert_errors(result, &[(Kind::SchemaValidation, "body")]);
        }
    }
}

#[test]
fn skip_validation_for_non_json_default_decoder_policy() {
    // TestValidateBody_SkipValidationForNonJSON; no standard YAML decoder is enabled.
    let mut doc = fixture("ValidBasicSchema");
    let content = &mut doc["paths"][BURGER_PATH]["post"]["requestBody"]["content"];
    *content = json!({"application/yaml": content["application/json"].clone()});
    let spec = CompiledSpec::from_json(&doc.to_string()).unwrap();
    assert_valid(run(
        &spec,
        &request("POST", BURGER_PATH, Some("application/yaml"), Some(BAD)),
    ));
}

#[test]
fn replacing_request_body_does_not_reuse_previous_data() {
    // Adaptation of PrefersAssignedBodyOverStaleGetBody: there is no Go stream
    // replay API here. Reuse the compiled spec while replacing the actual bytes.
    let spec = compiled("ValidBasicSchema");
    let mut req = request("POST", BURGER_PATH, Some("application/json"), Some(GOOD));
    assert_valid(run(&spec, &req));
    req.body = Some(BAD.as_bytes().to_vec());
    assert_errors(
        run(&spec, &req),
        &[
            (Kind::SchemaValidation, "body/patties"),
            (Kind::SchemaValidation, "body/vegetarian"),
        ],
    );
    req.body = Some(GOOD.as_bytes().to_vec());
    assert_valid(run(&spec, &req));
}

#[test]
fn boolean_exclusive_minimum_openapi30() {
    // TestValidateRequestSchema/FailOnBooleanExclusiveMinimum and
    // TestBooleanExclusiveMin_ValidValue.
    let spec = compile(
        "3.0.0",
        json!({"type":"object", "properties": {
            "exclusiveNumber": {"type":"number", "exclusiveMinimum":true, "minimum":10}
        }}),
        json!({}),
    );
    assert_error_paths(
        validate(&spec, json!({"exclusiveNumber":10})),
        &["body/exclusiveNumber"],
    );
    assert_valid(validate(&spec, json!({"exclusiveNumber":13})));
}

#[test]
fn numeric_exclusive_minimum_openapi31() {
    // TestValidateRequestSchema/PassWithCorrectExclusiveMinimum.
    let spec = compile(
        "3.1.0",
        json!({"type":"object", "properties": {
            "exclusiveNumber": {"type":"number", "exclusiveMinimum":12, "minimum":12}
        }}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"exclusiveNumber":15})));
    assert_error_paths(
        validate(&spec, json!({"exclusiveNumber":12})),
        &["body/exclusiveNumber"],
    );
}

#[test]
fn nested_read_only_required_ignored() {
    let spec = compile(
        "3.1.0",
        json!({"type":"object", "required":["profile"], "properties": {
            "profile": {"type":"object", "required":["id","email"], "properties": {
                "id": {"type":"string", "readOnly":true}, "email": {"type":"string"}
            }}
        }}),
        json!({}),
    );
    assert_valid(validate(
        &spec,
        json!({"profile":{"email":"john@example.com"}}),
    ));
    // Keep the non-readOnly requirement enforced after pruning.
    assert_error_paths(validate(&spec, json!({"profile":{}})), &["body/profile"]);
}

#[test]
fn all_of_read_only_required_ignored() {
    let spec = compile(
        "3.1.0",
        json!({"allOf":[{
            "type":"object", "required":["id","name"], "properties": {
                "id": {"type":"string", "readOnly":true}, "name": {"type":"string"}
            }
        }]}),
        json!({}),
    );
    assert_valid(validate(&spec, json!({"name":"John"})));
    assert_error_paths(validate(&spec, json!({})), &["body"]);
}

#[test]
fn vendor_json_decoder_compatibility() {
    // kin_parity_test.go::TestVendorJSONDecoderCompatibility, plus a negative control.
    let media_type = "application/vnd.test+json";
    let spec = compile_media(
        "3.1.0",
        media_type,
        json!({"schema": {
            "type":"object", "required":["ok"], "properties":{"ok":{"type":"boolean"}}
        }}),
        json!({}),
    );
    assert_valid(run(
        &spec,
        &request("POST", "/test", Some(media_type), Some(r#"{"ok":true}"#)),
    ));
    assert_error_paths(
        run(
            &spec,
            &request("POST", "/test", Some(media_type), Some(r#"{"ok":"true"}"#)),
        ),
        &["body/ok"],
    );
}

#[test]
fn legacy_validate_request_schema_malformed_json() {
    // kin_parity_test.go::TestLegacyValidateRequestSchemaMalformedJSON.
    let spec = compile("3.1.0", json!({"type":"object"}), json!({}));
    assert_errors(
        run(
            &spec,
            &request("POST", "/test", Some("application/json"), Some("{")),
        ),
        &[(Kind::InvalidBody, "body")],
    );
}

#[test]
fn urlencoded_nonfinite_number_rejected() {
    // TestValidateRequestBody_URLEncodedMarshalError. Rust may reject this as a
    // schema mismatch instead of Go's failed float serialization.
    let media_type = "application/x-www-form-urlencoded";
    let spec = compile_media(
        "3.1.0",
        media_type,
        json!({"schema": {
            "type":"object", "properties":{"bad_number":{"type":"number"}}
        }}),
        json!({}),
    );
    assert_schema_invalid(run(
        &spec,
        &request("POST", "/test", Some(media_type), Some("bad_number=NaN")),
    ));
}
