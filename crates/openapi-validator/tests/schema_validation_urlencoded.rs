// Adapted from libopenapi-validator/schema_validation/validate_urlencoded_test.go.
// Upstream license is preserved in fixtures/LIBOPENAPI_VALIDATOR_LICENSE.md.

mod common;

use common::*;
use openapi_validator::error::ValidationErrorKind;
use serde_json::json;

const FORM: &str = "application/x-www-form-urlencoded";

// Every TestComplexBodies payload is retained, with separate Rust tests so one
// mismatch cannot hide subsequent cases. The media schema/encoding is verbatim.
macro_rules! complex_case {
    ($name:ident, $body:expr, $valid:literal) => {
        #[test]
        fn $name() {
            let media =
                serde_json::from_str(include_str!("fixtures/urlencoded_complex_media.json"))
                    .unwrap();
            let spec = compile_media("3.1.0", FORM, media, json!({}));
            let result = validate_raw(&spec, FORM, $body);
            if $valid {
                assert_valid(result);
            } else {
                assert_schema_invalid(result);
            }
        }
    };
}

complex_case!(
    complex_nested_boolean,
    "bool=false&title=test&content[0][name]=true",
    true
);
complex_case!(
    complex_nested_integer,
    "bool=false&title=test&content[0][name]=4",
    true
);
complex_case!(
    complex_nested_fraction_rejected,
    "bool=false&title=test&content[0][name]=4.4",
    false
);
complex_case!(
    complex_boolean_enum_rejected,
    "bool=true&title=test&content[0][name]=true",
    false
);
complex_case!(
    complex_missing_title_rejected,
    "bool=false&content[0][name]=true",
    false
);
complex_case!(complex_empty_title, "bool=false&title", true);
complex_case!(
    complex_json_field,
    r#"bool=false&title&payload={"hey": [true, false]}"#,
    true
);
complex_case!(
    complex_invalid_json_field_schema,
    r#"bool=false&title&payload={"hey": [2], "adittional": false}"#,
    false
);
complex_case!(
    complex_reserved_characters_allowed,
    "bool=false&title=do not use #",
    true
);
complex_case!(
    complex_reserved_characters_rejected,
    "bool=false&title&reserved=do not use #",
    false
);
complex_case!(
    complex_pipe_delimited_array,
    "bool=false&title&pipeArr=1|2|3",
    true
);
complex_case!(
    complex_space_delimited_array,
    "bool=false&title&spaceArr=1 2 3",
    true
);
complex_case!(
    complex_encoded_space_delimited_array,
    "bool=false&title&spaceArr=1%202%203",
    true
);
complex_case!(
    complex_unexploded_array,
    "bool=false&title&unexplodedArr=1,2,3",
    true
);

#[test]
fn validate_urlencoded_object() {
    let spec = compile_media(
        "3.1.0",
        FORM,
        json!({"schema": {"type": "object"}}),
        json!({}),
    );
    assert_valid(validate_raw(&spec, FORM, "a=1"));
}

#[test]
fn malformed_url_encoding() {
    // TestTransformURLEncodedToSchemaJSON / Malformed URL Encoding.
    let spec = compile_media(
        "3.1.0",
        FORM,
        json!({"schema": {"type": "object"}}),
        json!({}),
    );
    assert_invalid(
        validate_raw(&spec, FORM, "bad_encoding=%zz"),
        ValidationErrorKind::InvalidBody,
    );
}

#[test]
fn invalid_json_content_type_field() {
    // TestTransformURLEncodedToSchemaJSON / Encoding Error.
    let spec = compile_media(
        "3.1.0",
        FORM,
        json!({
            "schema": {"type": "object", "properties": {"badJson": {}}},
            "encoding": {"badJson": {"contentType": "application/json"}}
        }),
        json!({}),
    );
    assert_invalid(
        validate_raw(&spec, FORM, "badJson={invalid"),
        ValidationErrorKind::InvalidBody,
    );
}
