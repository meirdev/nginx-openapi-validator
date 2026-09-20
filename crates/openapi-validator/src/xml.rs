//! Conversion of XML bodies into JSON, driven by the schema's `xml` objects
//! (name, attribute, wrapped, prefix, namespace), so the same JSON Schema
//! validator can check them.
//!
//! Elements and attributes that do not match any declared property are kept
//! under their literal names. That way `additionalProperties: false` and
//! `required` report mismatched names, prefixes or namespaces exactly as they
//! would for a JSON body.

use roxmltree::{Document, Node};
use serde_json::{Map, Value};

use crate::coerce::{self, deref};
use crate::compiled_spec::CompiledMediaType;
use crate::error::{ValidationError, ValidationErrorKind};

pub fn decode(
    raw: &[u8],
    media: Option<&CompiledMediaType>,
    root: &Value,
) -> Result<Value, Vec<ValidationError>> {
    let text = std::str::from_utf8(raw).map_err(|_| {
        vec![invalid_body("XML body is not valid UTF-8".to_string())]
    })?;
    let doc = Document::parse(text)
        .map_err(|e| vec![invalid_body(format!("Failed to parse request body as XML: {e}"))])?;

    let schema = media.and_then(|m| m.schema.as_ref()).map(|s| deref(root, s));
    let element = doc.root_element();

    if let Some(xml) = schema.and_then(|s| s.get("xml")) {
        if !namespace_matches(element, xml) {
            return Err(vec![ValidationError {
                kind: ValidationErrorKind::SchemaValidation,
                message: format!(
                    "Root element '{}' does not match the declared XML namespace or prefix",
                    element.tag_name().name()
                ),
                path: "body".to_string(),
            }]);
        }
    }

    let mut errors = Vec::new();
    let value = convert(element, schema, root, "body", &mut errors);
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(errors)
    }
}

/// Convert an element according to its schema.
///
/// `path` is the instance location used in error reports. Elements whose
/// name matches a property but whose namespace or prefix does not are
/// reported into `errors`, since keeping them under the literal name would
/// let them satisfy the property they failed to match.
fn convert(
    node: Node,
    schema: Option<&Value>,
    root: &Value,
    path: &str,
    errors: &mut Vec<ValidationError>,
) -> Value {
    let schema = schema.map(|s| deref(root, s));
    let has_structure = node.attributes().len() > 0 || node.children().any(|c| c.is_element());

    if coerce::is_object_schema(root, schema)
        || (schema.map_or(true, |s| coerce::schema_types(s).is_empty()) && has_structure)
    {
        convert_object(node, schema, root, path, errors)
    } else {
        coerce::coerce(&text_of(node), root, schema)
    }
}

/// How a property is expected to appear in XML.
struct Mapping<'a> {
    property: &'a str,
    element_name: String,
    attribute: bool,
    schema: &'a Value,
    xml: Option<&'a Value>,
}

fn mappings<'a>(schema: Option<&'a Value>, root: &'a Value) -> Vec<Mapping<'a>> {
    let Some(properties) = schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, prop)| {
            let prop = deref(root, prop);
            let xml = prop.get("xml");
            Mapping {
                property: name,
                element_name: xml
                    .and_then(|x| x.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_string(),
                attribute: xml
                    .and_then(|x| x.get("attribute"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                schema: prop,
                xml,
            }
        })
        .collect()
}

fn convert_object(
    node: Node,
    schema: Option<&Value>,
    root: &Value,
    path: &str,
    errors: &mut Vec<ValidationError>,
) -> Value {
    let mappings = mappings(schema, root);
    let mut object = Map::new();

    for attr in node.attributes() {
        let mapping = mappings.iter().find(|m| {
            m.attribute
                && m.element_name == attr.name()
                && m.xml.map_or(true, |x| attribute_namespace_matches(attr, x))
        });
        match mapping {
            Some(m) => {
                object.insert(
                    m.property.to_string(),
                    coerce::coerce(attr.value(), root, Some(m.schema)),
                );
            }
            None => {
                object.insert(attr.name().to_string(), Value::String(attr.value().to_string()));
            }
        }
    }

    for child in node.children().filter(|c| c.is_element()) {
        let local_name = child.tag_name().name();
        let by_name: Vec<&Mapping> = mappings
            .iter()
            .filter(|m| !m.attribute && m.element_name == local_name)
            .collect();
        let mapping = by_name
            .iter()
            .find(|m| m.xml.map_or(true, |x| namespace_matches(child, x)))
            .copied();

        match mapping {
            Some(m) if coerce::is_array_schema(root, Some(m.schema)) => {
                let wrapped = m
                    .xml
                    .and_then(|x| x.get("wrapped"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let items_schema = coerce::items_schema(root, Some(m.schema));
                let child_path = format!("{path}/{}", m.property);
                if wrapped {
                    let value =
                        convert_wrapped_array(child, m.property, items_schema, root, &child_path, errors);
                    object.insert(m.property.to_string(), value);
                } else {
                    let index = object.get(m.property).and_then(Value::as_array).map_or(0, Vec::len);
                    let item_path = format!("{child_path}/{index}");
                    let value = convert(child, items_schema, root, &item_path, errors);
                    push(&mut object, m.property, value, true);
                }
            }
            Some(m) => {
                let child_path = format!("{path}/{}", m.property);
                let value = convert(child, Some(m.schema), root, &child_path, errors);
                push(&mut object, m.property, value, false);
            }
            None => {
                if let Some(m) = by_name.first() {
                    errors.push(ValidationError {
                        kind: ValidationErrorKind::SchemaValidation,
                        message: format!(
                            "Element '{local_name}' does not match the XML namespace or prefix declared for property '{}'",
                            m.property
                        ),
                        path: format!("{path}/{}", m.property),
                    });
                }
                let child_path = format!("{path}/{local_name}");
                let value = convert(child, None, root, &child_path, errors);
                push(&mut object, local_name, value, false);
            }
        }
    }

    Value::Object(object)
}

/// A `wrapped` array: the element holds one child element per item, each
/// named after the items schema (`xml.name`) or the property.
fn convert_wrapped_array(
    wrapper: Node,
    property: &str,
    items_schema: Option<&Value>,
    root: &Value,
    path: &str,
    errors: &mut Vec<ValidationError>,
) -> Value {
    let items_xml = items_schema.and_then(|s| s.get("xml"));
    let item_name = items_xml
        .and_then(|x| x.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(property);

    let children: Vec<Node> = wrapper.children().filter(|c| c.is_element()).collect();
    let all_match = children.iter().all(|c| {
        c.tag_name().name() == item_name && items_xml.map_or(true, |x| namespace_matches(*c, x))
    });

    if !all_match {
        // Mismatched item names: expose the wrapper's real shape so the
        // array type check fails with a clear error.
        return convert_object(wrapper, None, root, path, errors);
    }

    Value::Array(
        children
            .into_iter()
            .enumerate()
            .map(|(i, c)| convert(c, items_schema, root, &format!("{path}/{i}"), errors))
            .collect(),
    )
}

/// Insert a child value, turning repeated elements into an array.
fn push(object: &mut Map<String, Value>, key: &str, value: Value, force_array: bool) {
    match object.get_mut(key) {
        Some(Value::Array(items)) => items.push(value),
        Some(existing) => {
            let first = std::mem::take(existing);
            *existing = Value::Array(vec![first, value]);
        }
        None => {
            object.insert(
                key.to_string(),
                if force_array { Value::Array(vec![value]) } else { value },
            );
        }
    }
}

/// Text content of an element, ignoring surrounding whitespace.
fn text_of(node: Node) -> String {
    node.children()
        .filter_map(|c| c.text())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Check an element against the `prefix` and `namespace` of an `xml` object.
/// Only the declared aspects are compared.
fn namespace_matches(node: Node, xml: &Value) -> bool {
    let ns = node.tag_name().namespace();
    if let Some(expected_ns) = xml.get("namespace").and_then(Value::as_str) {
        if ns != Some(expected_ns) {
            return false;
        }
    }
    if let Some(expected_prefix) = xml.get("prefix").and_then(Value::as_str) {
        let prefix = ns.and_then(|uri| node.lookup_prefix(uri));
        if prefix != Some(expected_prefix) {
            return false;
        }
    }
    true
}

fn attribute_namespace_matches(attr: roxmltree::Attribute, xml: &Value) -> bool {
    if let Some(expected_ns) = xml.get("namespace").and_then(Value::as_str) {
        if attr.namespace() != Some(expected_ns) {
            return false;
        }
    }
    true
}

fn invalid_body(message: String) -> ValidationError {
    ValidationError {
        kind: ValidationErrorKind::InvalidBody,
        message,
        path: "body".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn to_json(xml: &str, schema: Value) -> Value {
        let doc = Document::parse(xml).unwrap();
        let mut errors = Vec::new();
        let v = convert(doc.root_element(), Some(&schema), &Value::Null, "body", &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        v
    }

    #[test]
    fn elements_attributes_and_coercion() {
        let v = to_json(
            r#"<Cat id="7"><name>Tom</name><age>3</age></Cat>"#,
            json!({"type": "object", "properties": {
                "id": {"type": "integer", "xml": {"attribute": true}},
                "name": {"type": "string"}, "age": {"type": "integer"}
            }}),
        );
        assert_eq!(v, json!({"id": 7, "name": "Tom", "age": 3}));
    }

    #[test]
    fn wrapped_and_unwrapped_arrays() {
        let v = to_json(
            "<R><pets><pet>a</pet><pet>b</pet></pets><tags>x</tags><tags>y</tags></R>",
            json!({"type": "object", "properties": {
                "pets": {"type": "array", "xml": {"wrapped": true}, "items": {"type": "string", "xml": {"name": "pet"}}},
                "tags": {"type": "array", "items": {"type": "string"}}
            }}),
        );
        assert_eq!(v, json!({"pets": ["a", "b"], "tags": ["x", "y"]}));
    }

    #[test]
    fn unknown_elements_are_kept_literally() {
        let v = to_json("<R><other>1</other></R>", json!({"type": "object", "properties": {}}));
        assert_eq!(v, json!({"other": 1}));
    }
}
