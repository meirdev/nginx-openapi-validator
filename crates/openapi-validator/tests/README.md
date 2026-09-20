# Schema validation parity tests

These integration tests adapt request-relevant cases from the local
`libopenapi-validator/schema_validation` checkout at commit
`f309f59bdf6b385a965a86efa1faa63356af4e77`. The original checkout was
`/Users/meirelbaz/Code/libopenapi-validator`; it is **not needed to run the tests**.
The upstream MIT license is preserved in
[fixtures/LIBOPENAPI_VALIDATOR_LICENSE.md](fixtures/LIBOPENAPI_VALIDATOR_LICENSE.md).

All cases compile an OpenAPI document through `CompiledSpec::from_json` and call
`validate_request` with `RequestData`. They do not bypass our library by testing
the `jsonschema` dependency directly. JSON specs, bodies, and the form fixture
are self-contained; no NGINX, Go runtime, network, or new dependencies are needed.

Run every case even when another integration target fails:

```sh
cargo test -p openapi-validator --tests --no-fail-fast
```

Run one group or one case:

```sh
cargo test -p openapi-validator --test schema_validation_openapi
cargo test -p openapi-validator --test schema_validation one_of_multiple_matches_issue520
```

## Status

All **87 integration tests pass**. The initial port ran 58 pass / 29 fail;
the gaps and how they were closed:

| Test file | What the failures exposed | Fix |
| --- | --- | --- |
| `schema_validation.rs` | `dependentSchemas` was not enforced | Schemas are now compiled from the raw JSON document (`compiled_spec.rs`), so no keyword is lost on the way to the validator. The earlier typed round trip through an OpenAPI struct dropped `dependentSchemas`, `not`, `if`/`then`, `patternProperties`, `propertyNames`, `contains`, `unevaluated*` and more. |
| `schema_validation_openapi.rs` | OpenAPI 3.0 `nullable`, nullable enums, boolean `exclusiveMinimum` | `dialect.rs` rewrites 3.0.x documents into Draft 2020-12 form before compilation. Numeric `exclusiveMinimum` in a 3.0 document is a compile error. |
| `directional_schema.rs` | `readOnly` properties stayed in `required` for request bodies | `dialect.rs` strips `readOnly` properties from every `required` list, following `$ref`. |
| `schema_validation_urlencoded.rs` | Form bodies were not decoded or validated | `form.rs` decodes `application/x-www-form-urlencoded` bodies into JSON using the media type's schema and `encoding` (per-field `contentType`, `style`, `explode`, `allowReserved`, bracketed nested keys, strict percent-decoding). |
| `schema_validation_xml.rs` | XML bodies were not decoded or validated | `xml.rs` converts XML into JSON using the schema's `xml` objects (name, attribute, wrapped, prefix, namespace). Elements that match a property name but not its namespace or prefix are reported as schema errors. |

Failing parity tests were deliberately kept active while the gaps existed,
rather than using `#[ignore]` or `#[should_panic]`.

## Source mapping and adaptations

Rust test names follow the upstream case names, with separate names for
positive/negative examples where useful. Comments identify cases whose names or
semantics needed adaptation.

| Upstream source | Ported cases |
| --- | --- |
| `validate_schema_test.go` | Simple valid/invalid burgers, multiple array errors, malformed JSON, nested component references, numeric exclusive minimum, dependent schemas, all `oneOf` scenarios (issue 520), discriminator with `oneOf`/`anyOf` references (issue 788), compilation failure |
| `validate_schema_openapi_test.go` | Nullable in 3.0 versus 3.1, nullable enum, multiple OpenAPI annotations, discriminator annotation, circular references |
| `validate_schema_coercion_test.go` | Default strict JSON-body behavior and invalid boolean/number strings |
| `schema_resources_test.go` | Recursive schemas through arrays, recursive error location, request-direction requirements through component references |
| `directional_schema_test.go` | Request-direction required properties, empty required list after removing readOnly fields, tuple `prefixItems` |
| `validate_urlencoded_test.go` | All 14 `TestComplexBodies` payloads and the exact media schema/encoding fixture, basic object, malformed URL encoding, malformed JSON-encoded field |
| `validate_xml_test.go` | Basic XML name (issue 346), empty/malformed XML, attributes, integer types, wrapped/unwrapped arrays, custom property names, required/optional fields, whitespace/empty elements, property mismatch, attribute mismatch, primitive value, incorrect wrapped item name, nested objects, mixed attributes/elements, scalar coercion, SOAP, float precision, nullable annotation, no properties, namespace/prefix checks at root and nested object/array properties |

Adaptations:

- Upstream YAML schemas are expressed as JSON, with the title, version, response
  description, and request wrapper required by our public API. Response-located
  XML schemas are mounted as request-body schemas; this does not test response
  validation. Empty XML is a missing required body through this API.
- Go's string/byte/object entry points collapse to our byte-oriented request
  entry point. The two simple-valid cases therefore share one test.
- Rust exposes flat errors with body instance paths, not Go's nested errors,
  schema keyword paths, or YAML line/column numbers. Tests check error kinds,
  HTTP status, nonempty messages, and exact instance paths where meaningful.
  Composition error counts/wording are intentionally not copied across engines.
- The Go compilation-failure example uses a valid, complex regex and can skip
  depending on the engine. Our version uses an unclosed character class to
  require a deterministic `SchemaCompileError`.
- The original invalid `dependentSchemas` example is malformed JSON, and the
  original valid example never triggers the dependency. Both are preserved;
  `v3_1_dependent_schemas_triggered` additionally supplies valid JSON that
  triggers the dependency inside `fishCake`. This extra control exposes the gap.
- Extra negative controls cover numeric boundaries, recursive child values,
  discriminator branch mismatches, and required writeOnly fields. The product
  discriminator cases share the stricter valid-case component definitions.

## Not a complete port of Go internals

The following upstream areas have no equivalent public API here and are not
represented as passing parity claims:

- `validate_document_test.go` and `openapi_schemas/load_schema_test.go` validate
  whole OpenAPI documents against metaschemas, including YAML-specific behavior.
  Our compiler is not a document-validation API.
- `property_locator_test.go`, `locate_schema_property_test.go`, and
  `validate_schema_extract_errors_test.go` primarily exercise Go/YAML source-node
  lookup and error rendering. Rust instance paths are covered instead.
- Cache lifecycle, render fallback branches, resource index internals, external
  resource loading, nil schema pointers, and invalid Go-native value types have
  no corresponding request-level operations. Local recursive resources are
  covered through actual requests.
- Response/generic validation purposes, version overrides, OpenAPI-mode toggles,
  and optional JSON-body scalar coercion have no configuration equivalents.
  Query-parameter coercion is not substituted for JSON-body coercion.
- Internal XML/form transformation helpers and content-type classification
  helpers return structures or classifications not exposed by our request API.
  Their behavior is exercised through body-validation cases where applicable;
  form `TestComplexBodies` is complete.
