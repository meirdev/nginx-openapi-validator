// Adapted from libopenapi-validator/schema_validation/validate_xml_test.go.
// Copyright 2023 Princess B33f Heavy Industries / Dave Shanley
// SPDX-License-Identifier: MIT

mod common;

use common::*;
use openapi_validator::error::ValidationErrorKind;
use serde_json::{json, Value};

const XML: &str = "application/xml";

fn xml_spec(schema: Value) -> openapi_validator::CompiledSpec {
    // Upstream extracts these schemas from responses. Our API validates
    // requests, so mount the same schemas as required request bodies.
    compile_media("3.0.0", XML, json!({"schema": schema}), json!({}))
}

#[test]
fn issue346_basic_xml_with_name() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "nice": {"type": "string"}
    }, "xml": {"name": "Cat"}}));
    assert_valid(validate_raw(&spec, XML, "<Cat><nice>true</nice></Cat>"));
}

#[test]
fn malformed_xml_empty() {
    let spec = xml_spec(json!({"type": "object", "xml": {"name": "Cat"}}));
    // Through the request API, empty content is a missing required body.
    assert_invalid(
        validate_raw(&spec, XML, ""),
        ValidationErrorKind::MissingRequiredBody,
    );
}

#[test]
fn malformed_xml_syntax() {
    // TestTransformXMLToSchemaJSON_InvalixXml.
    let spec = xml_spec(json!({"type": "object", "xml": {"name": "Cat"}}));
    assert_invalid(
        validate_raw(&spec, XML, "<Cat><nice>"),
        ValidationErrorKind::InvalidBody,
    );
}

#[test]
fn with_attributes() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "id": {"type": "integer", "xml": {"attribute": true}}, "name": {"type": "string"}
    }, "xml": {"name": "Cat"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        r#"<Cat id="123"><name>Fluffy</name></Cat>"#,
    ));
}

fn age_spec() -> openapi_validator::CompiledSpec {
    xml_spec(
        json!({"type": "object", "properties": {"age": {"type": "integer"}}, "xml": {"name": "Cat"}}),
    )
}

#[test]
fn type_validation_valid_integer() {
    assert_valid(validate_raw(&age_spec(), XML, "<Cat><age>5</age></Cat>"));
}

#[test]
fn type_validation_invalid_integer() {
    assert_schema_invalid(validate_raw(
        &age_spec(),
        XML,
        "<Cat><age>not-a-number</age></Cat>",
    ));
}

fn wrapped_array_spec() -> openapi_validator::CompiledSpec {
    xml_spec(json!({"type": "object", "properties": {"pets": {
        "type": "array", "xml": {"wrapped": true}, "items": {
            "type": "object", "properties": {"name": {"type": "string"}, "age": {"type": "integer"}},
            "xml": {"name": "pet"}
        }
    }}, "xml": {"name": "Pets"}}))
}

#[test]
fn wrapped_array_valid() {
    assert_valid(validate_raw(&wrapped_array_spec(), XML,
        "<Pets><pets><pet><name>Fluffy</name><age>3</age></pet><pet><name>Spot</name><age>5</age></pet></pets></Pets>"));
}

#[test]
fn wrapped_array_invalid_item() {
    assert_schema_invalid(validate_raw(
        &wrapped_array_spec(),
        XML,
        "<Pets><pets><pet><name>Fluffy</name><age>not-a-number</age></pet></pets></Pets>",
    ));
}

#[test]
fn multiple_properties_with_custom_names() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "userId": {"type": "integer", "xml": {"name": "id"}},
        "userName": {"type": "string", "xml": {"name": "username"}},
        "userEmail": {"type": "string", "xml": {"name": "email"}}
    }, "xml": {"name": "User"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        "<User><id>42</id><username>johndoe</username><email>john@example.com</email></User>",
    ));
}

fn product_spec() -> openapi_validator::CompiledSpec {
    xml_spec(
        json!({"type": "object", "required": ["productId", "name"], "properties": {
        "productId": {"type": "integer"}, "name": {"type": "string"}, "description": {"type": "string"}
    }, "xml": {"name": "Product"}}),
    )
}

#[test]
fn schema_violations_missing_required_property() {
    assert_schema_invalid(validate_raw(
        &product_spec(),
        XML,
        "<Product><productId>123</productId></Product>",
    ));
}

#[test]
fn schema_violations_valid_required_and_optional_properties() {
    let spec = product_spec();
    for body in [
        "<Product><productId>123</productId><name>Widget</name></Product>",
        "<Product><productId>123</productId><name>Widget</name><description>A useful widget</description></Product>",
    ] {
        assert_valid(validate_raw(&spec, XML, body));
    }
}

#[test]
fn empty_and_whitespace_elements() {
    let spec = xml_spec(
        json!({"type": "object", "properties": {"value": {"type": "string"}}, "xml": {"name": "Test"}}),
    );
    assert_valid(validate_raw(
        &spec,
        XML,
        "<Test>\n\t\t<value>hello</value>\n\t</Test>",
    ));
    assert_valid(validate_raw(&spec, XML, "<Test><value></value></Test>"));
}

#[test]
fn property_mismatch() {
    let spec = xml_spec(
        json!({"type": "object", "required": ["enabled", "maxRetries"], "properties": {
        "enabled": {"type": "boolean"}, "maxRetries": {"type": "integer"}
    }, "xml": {"name": "Config"}}),
    );
    assert_schema_invalid(validate_raw(
        &spec,
        XML,
        "<Config><isEnabled>true</isEnabled><retries>5</retries></Config>",
    ));
}

#[test]
fn attribute_type_mismatch() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "id": {"type": "integer", "xml": {"attribute": true}},
        "quantity": {"type": "integer", "xml": {"attribute": true}},
        "name": {"type": "string"}
    }, "xml": {"name": "Item"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        r#"<Item id="123" quantity="5"><name>Widget</name></Item>"#,
    ));
    assert_schema_invalid(validate_raw(
        &spec,
        XML,
        r#"<Item id="abc" quantity="5"><name>Widget</name></Item>"#,
    ));
}

#[test]
fn primitive_value() {
    let spec = xml_spec(json!({"type": "string", "xml": {"name": "Value"}}));
    assert_valid(validate_raw(&spec, XML, "<Value>hello world</Value>"));
}

#[test]
fn array_not_wrapped() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "items": {"type": "array", "items": {"type": "string"}}
    }, "xml": {"name": "Items"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        "<Items><items>one</items><items>two</items><items>three</items></Items>",
    ));
}

#[test]
fn wrapped_array_with_wrong_item_name() {
    let spec = xml_spec(json!({"type": "object", "properties": {"data": {
        "type": "array", "xml": {"wrapped": true}, "items": {
            "additionalProperties": false, "type": "object", "properties": {"value": {"type": "string"}},
            "xml": {"name": "record"}
        }
    }}, "xml": {"name": "Collection"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        "<Collection><data><record><value>test</value></record></data></Collection>",
    ));
    assert_schema_invalid(validate_raw(
        &spec,
        XML,
        "<Collection><data><item><value>test</value></item></data></Collection>",
    ));
}

#[test]
fn mixed_attributes_and_elements() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "id": {"type": "integer", "xml": {"attribute": true}},
        "isbn": {"type": "string", "xml": {"attribute": true}},
        "title": {"type": "string"}, "author": {"type": "string"}, "price": {"type": "number"}
    }, "xml": {"name": "Book"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        r#"<Book id="1" isbn="978-3-16-148410-0"><title>Go Programming</title><author>John Doe</author><price>29.99</price></Book>"#,
    ));
}

#[test]
fn nested_objects() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "orderId": {"type": "integer"}, "customer": {"type": "object", "properties": {
            "name": {"type": "string"}, "address": {"type": "object", "properties": {
                "street": {"type": "string"}, "city": {"type": "string"}
            }}
        }}
    }, "xml": {"name": "Order"}}));
    assert_valid(validate_raw(&spec, XML, "<Order><orderId>123</orderId><customer><name>Jane Doe</name><address><street>123 Main St</street><city>Springfield</city></address></customer></Order>"));
}

#[test]
fn type_coercion() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "intValue": {"type": "integer"}, "floatValue": {"type": "number"},
        "stringValue": {"type": "string"}, "boolValue": {"type": "string"}
    }, "xml": {"name": "Data"}}));
    assert_valid(validate_raw(&spec, XML, "<Data><intValue>42</intValue><floatValue>3.14</floatValue><stringValue>hello</stringValue><boolValue>true</boolValue></Data>"));
}

#[test]
fn complex_real_world_soap() {
    let media_type = "application/soap+xml";
    let spec = compile_media(
        "3.0.0",
        media_type,
        json!({"schema": {
            "type": "object", "properties": {
                "status": {"type": "string"}, "requestId": {"type": "string", "xml": {"attribute": true}},
                "timestamp": {"type": "integer"}, "data": {"type": "object", "properties": {"value": {"type": "string"}}}
            }, "xml": {"name": "Response"}
        }}),
        json!({}),
    );
    assert_valid(validate_raw(
        &spec,
        media_type,
        r#"<Response requestId="req-12345"><status>success</status><timestamp>1699372800</timestamp><data><value>result</value></data></Response>"#,
    ));
}

#[test]
fn with_namespace() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "subject": {"type": "string"}, "body": {"type": "string"}
    }, "xml": {"name": "Message"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        r#"<msg:Message xmlns:msg="http://example.com/message"><msg:subject>Hello</msg:subject><msg:body>World</msg:body></msg:Message>"#,
    ));
}

#[test]
fn float_precision() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "temperature": {"type": "number"}, "humidity": {"type": "number"}, "pressure": {"type": "number"}
    }, "xml": {"name": "Measurement"}}));
    for body in [
        "<Measurement><temperature>23.456</temperature><humidity>65.2</humidity><pressure>1013.25</pressure></Measurement>",
        "<Measurement><temperature>23</temperature><humidity>65</humidity><pressure>1013</pressure></Measurement>",
    ] {
        assert_valid(validate_raw(&spec, XML, body));
    }
}

#[test]
fn version30_with_nullable() {
    let spec = xml_spec(json!({"type": "object", "properties": {
        "value": {"type": "string", "nullable": true}
    }, "xml": {"name": "Item"}}));
    assert_valid(validate_raw(&spec, XML, "<Item><value>test</value></Item>"));
}

#[test]
fn no_properties() {
    let spec = xml_spec(json!({"type": "object", "xml": {"name": "Empty"}}));
    assert_valid(validate_raw(
        &spec,
        XML,
        "<Empty><anything>value</anything></Empty>",
    ));
}

fn namespace_spec() -> openapi_validator::CompiledSpec {
    // getXmlTestSchema: normalize the shorthand upstream version 3.1 to 3.1.0.
    compile_media(
        "3.1.0",
        XML,
        json!({"schema": {
            "type": "object", "additionalProperties": false, "properties": {
                "body": {"type": "object", "required": ["id", "success", "payload"],
                    "xml": {"prefix": "t", "namespace": "http://assert.t", "name": "reqBody"},
                    "properties": {
                        "id": {"type": "integer", "xml": {"attribute": true}},
                        "success": {"type": "boolean", "xml": {"name": "ok", "prefix": "j", "namespace": "http://j.j"}},
                        "payload": {"oneOf": [{"type": "integer"}, {"type": "object"}]}
                    }
                },
                "data": {"type": "array", "xml": {"wrapped": true, "name": "list"}, "items": {
                    "additionalProperties": false, "type": "object", "required": ["value"],
                    "properties": {"value": {"type": "string", "xml": {"namespace": "http://prop.arr", "prefix": "arr"}}},
                    "xml": {"name": "record", "prefix": "unt", "namespace": "http://expect.t"}
                }}
            }, "xml": {"name": "Collection"}
        }}),
        json!({}),
    )
}

#[test]
fn namespace_invalid_prefix() {
    assert_schema_invalid(validate_raw(
        &namespace_spec(),
        XML,
        "<Collection><reqBody></reqBody></Collection>",
    ));
}

#[test]
fn namespace_invalid_uri() {
    assert_schema_invalid(validate_raw(
        &namespace_spec(),
        XML,
        r#"<Collection><t:reqBody xmlns:t="incorrectUrl"></t:reqBody></Collection>"#,
    ));
}

#[test]
fn namespace_invalid_in_root() {
    let spec = xml_spec(
        json!({"type": "object", "xml": {"name": "Cat", "prefix": "c", "namespace": "http://cat.ca"}}),
    );
    assert_schema_invalid(validate_raw(
        &spec,
        XML,
        r#"<c:Cat xmlns:c="invalid"></c:Cat>"#,
    ));
}

#[test]
fn namespace_correct_in_root() {
    let spec = xml_spec(
        json!({"type": "string", "xml": {"name": "Cat", "prefix": "c", "namespace": "http://cat.ca"}}),
    );
    assert_valid(validate_raw(
        &spec,
        XML,
        r#"<c:Cat xmlns:c="http://cat.ca">meow</c:Cat>"#,
    ));
}

#[test]
fn xml_successfully_converted() {
    assert_valid(validate_raw(
        &namespace_spec(),
        XML,
        r#"<Collection><t:reqBody xmlns:t="http://assert.t" id="2"><j:ok xmlns:j="http://j.j">true</j:ok><payload><any>2</any></payload></t:reqBody>
<list xmlns:unt="http://expect.t"><unt:record><arr:value xmlns:arr="http://prop.arr">Text</arr:value></unt:record></list></Collection>"#,
    ));
}

#[test]
fn missing_prefix_in_object_properties() {
    assert_schema_invalid(validate_raw(
        &namespace_spec(),
        XML,
        r#"<Collection><t:reqBody xmlns:t="http://assert.t" id="2"><ok>true</ok><payload><any>2</any></payload></t:reqBody>
<list xmlns:unt="http://expect.t"><unt:record><arr:value xmlns:arr="http://prop.arr">Text</arr:value></unt:record></list></Collection>"#,
    ));
}

#[test]
fn missing_prefix_in_array_item_properties() {
    assert_schema_invalid(validate_raw(
        &namespace_spec(),
        XML,
        r#"<Collection><t:reqBody xmlns:t="http://assert.t" id="2"><j:ok xmlns:j="http://j.j">true</j:ok><payload><any>2</any></payload></t:reqBody>
<list xmlns:unt="http://expect.t"><unt:record><value>Text</value></unt:record></list></Collection>"#,
    ));
}

#[test]
fn xml_transformations_incorrect_schema() {
    assert_schema_invalid(validate_raw(
        &namespace_spec(),
        XML,
        r#"<Collection><t:reqBody xmlns:t="http://assert.t" id="2"><j:ok xmlns:j="http://j.j">NotBoolean</j:ok><payload><any>NotInteger</any></payload></t:reqBody>
<list xmlns:unt="http://expect.t"><unt:record><arr:value xmlns:arr="http://prop.arr">Text</arr:value></unt:record></list></Collection>"#,
    ));
}
