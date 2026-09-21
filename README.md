# NGINX OpenAPI Validator

A Rust NGINX dynamic module that validates incoming HTTP requests against an OpenAPI specification before they reach your upstream service. Reject invalid requests in **block** mode, or log validation failures and let requests continue in **audit** mode.

The workspace also includes a framework-independent Rust validation library.

## Features

- OpenAPI 3.0.x and 3.1.x specifications in JSON format.
- Path and HTTP method matching, including path templates, trailing-slash normalization, and `HEAD` fallback to `GET`.
- Path, query, header, and optional cookie parameter validation with schema-aware type conversion and OpenAPI serialization styles.
- Request `Content-Type`, required-body, and schema validation for JSON, URL-encoded forms, and XML, including `+json` and `+xml` media types.
- JSON Schema validation with local component references, recursive schemas, and composition keywords such as `allOf`, `anyOf`, and `oneOf`.
- OpenAPI 3.0 `nullable` and exclusive-bound handling; request validation does not require `readOnly` properties.
- Per-location configuration and NGINX variables for validation results.

Specifications and schemas are compiled when NGINX loads its configuration and reused across requests. Validation runs in the HTTP access phase.

## Quick start

You need Docker with the Compose plugin. Run these commands from the repository root:

```sh
docker compose -f example/docker-compose.yml up -d --build --wait
```

The example exposes NGINX at `http://localhost:18088` and proxies accepted requests to an echo backend. Requests under `/api/` are validated against [example/api-spec.json](example/api-spec.json); other paths bypass validation.

```sh
# Valid query parameters: 200
curl -i 'http://localhost:18088/api/pets?status=available&limit=10'

# Missing the required status parameter: 400
curl -i 'http://localhost:18088/api/pets'

# Unknown path: 404
curl -i 'http://localhost:18088/api/unknown'

# Method not defined for this path: 405
curl -i -X DELETE 'http://localhost:18088/api/pets'

# Valid JSON body: 200 from the echo backend
curl -i 'http://localhost:18088/api/pets' \
  -H 'Content-Type: application/json' \
  -d '{"name":"Rex","species":"dog"}'

# Unsupported media type: 415
curl -i 'http://localhost:18088/api/pets' \
  -H 'Content-Type: text/plain' \
  -d 'hello'
```

The echo backend always returns `200` for accepted requests, even when the specification documents a different response status. This module validates requests only.

View logs or stop the example:

```sh
docker compose -f example/docker-compose.yml logs -f nginx
docker compose -f example/docker-compose.yml down
```

The Docker image copies the configuration and specification at build time. After editing either file, run the `up --build` command again.

## NGINX configuration

Load the module in the main configuration context, then enable validation in a `location`:

```nginx
load_module modules/libngx_oav_module.so;

events {}

http {
    log_format oav '$remote_addr "$request" $status '
                   'oav_status=$oav_status errors=$oav_error_count '
                   'detail="$oav_first_error"';
    access_log /var/log/nginx/access.log oav;

    server {
        listen 80;

        location /api/ {
            openapi_spec /etc/nginx/api-spec.json;
            openapi_validate on;
            openapi_validate_mode block;

            proxy_pass http://backend:5678/;
        }
    }
}
```

Replace the upstream address and file paths for your deployment. See [example/nginx.conf](example/nginx.conf) for the complete Docker configuration.

The specification's paths must match the NGINX request URI at validation time. In this example, they include `/api/pets`, even though the trailing slash in `proxy_pass` strips `/api/` before forwarding to the backend. OpenAPI `servers` URLs do not supply a routing prefix.

### Directives

All four directives are supported in the `location` context. Nested locations inherit settings they do not override.

| Directive | Values | Default | Purpose |
| --- | --- | --- | --- |
| `openapi_spec` | Path to a JSON specification | Unset | Read and compile the specification during configuration loading. Relative paths use the NGINX prefix. |
| `openapi_validate` | `on`, `off` | `off` | Enable request validation. A loaded specification is also required. |
| `openapi_validate_mode` | `block`, `audit` | `block` | Reject invalid requests or log errors and continue processing. |
| `openapi_validate_parts` | Comma-separated list | `all` | Select which request checks to run. |

An unreadable or uncompileable specification causes configuration loading to fail. A changed specification takes effect when NGINX reloads its configuration; it is not watched automatically.

### Selective validation

An explicit list replaces the inherited/default selection. Pass it as a single argument without spaces, or quote the argument.

| Part | Enabled by default | Checks |
| --- | --- | --- |
| `path` | Yes | Request path exists in the specification. |
| `method` | Yes | HTTP method exists for the matched path. |
| `path_params` | Yes | Path parameter values. |
| `query_params` | Yes | Query parameter presence and values. |
| `header_params` | Yes | Header parameter presence and values. |
| `cookie_params` | No | Cookie parameter presence and values. |
| `content_type` | Yes | Media type declared for the request body. |
| `body` | Yes | Required body presence, decoding, and schema constraints. |
| `disallow_additional_query_params` | No | Reject undeclared query parameters; requires `query_params`. |

`all` restores the default selection, which excludes cookies and the strict query-parameter check. Put extra checks after `all`:

```nginx
# Default checks, plus cookies and rejection of undeclared query parameters
openapi_validate_parts all,cookie_params,disallow_additional_query_params;

# Alternatively, check routing and query parameters only
# openapi_validate_parts path,method,query_params;
```

If no path or operation matches and its corresponding check is disabled, validation passes without running the remaining checks for that request.

### Audit mode and logging

To observe failures while allowing requests to continue:

```nginx
openapi_validate_mode audit;
```

Audit mode writes validation errors to the NGINX error log at error level. Both modes expose the following variables for access logs:

| Variable | Value after validation |
| --- | --- |
| `$oav_status` | `valid` or `invalid` |
| `$oav_error_count` | Number of validation errors; `0` for valid requests |
| `$oav_first_error` | First error's path and message, truncated for logging; empty for valid requests |
| `$oav_errors_json` | JSON array of errors with `kind`, `message`, and `path`; `[]` for valid requests |

All four variables return `-` when no validation result is available. Validation runs in the access phase, so these values are intended for later phases such as access logging.

### Rejection status codes

| Status | Reason |
| --- | --- |
| `400 Bad Request` | Missing or invalid parameters, missing required body, malformed body, or schema violations |
| `404 Not Found` | No matching specification path |
| `405 Method Not Allowed` | Method not defined for the matched path |
| `415 Unsupported Media Type` | Missing, malformed, or unsupported `Content-Type` when required |

If several errors apply, status precedence is `404`, then `405`, then `415`, then `400`. Rejections use NGINX's normal error response handling; the module does not automatically return `$oav_errors_json` as the response body.

## Building the module

The [example Dockerfile](example/Dockerfile) builds the Rust `cdylib` and copies `libngx_oav_module.so` into the NGINX image. It includes the native build tools used by the vendored NGINX dependency.

For a native build in a suitably configured Unix environment, install Rust/Cargo, a C toolchain, libclang, CMake, and Make, then run:

```sh
cargo build --release --locked -p ngx-oav-module
```

On Linux, the module is written to `target/release/libngx_oav_module.so`. Copy it into your NGINX module directory and load it with `load_module`.

The module must match the runtime NGINX version and module ABI. The checked-in lockfile resolves `nginx-src` to `1.28.4+1.28.2` (NGINX `1.28.2`); the example runtime uses the `nginx:1.28` image tag. Keep the builder and runtime aligned when changing dependencies or images, and verify loading with `nginx -t`.

## Using the Rust library

The `openapi-validator` crate can validate requests independently of NGINX. Add it as a path dependency from your application, then compile a specification once and reuse it:

```rust
use openapi_validator::{validate_request, CompiledSpec, RequestData, ValidationConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = std::fs::read_to_string("example/api-spec.json")?;
    let spec = CompiledSpec::from_json(&document)?;
    let request = RequestData {
        method: "GET".into(),
        path: "/api/pets".into(),
        query_string: Some("status=available&limit=10".into()),
        headers: vec![],
        body: None,
    };

    let result = validate_request(&spec, &request, &ValidationConfig::default());
    assert!(result.is_valid());
    assert_eq!(result.http_status(), None);
    Ok(())
}
```

Unlike the NGINX directive, `ValidationConfig::default()` enables validation. Supply a decoded path and a query string in its original URL-encoded form without the leading `?`. The library returns `ValidationResult`; your application decides how to log or reject failures. Enforcement mode is applied by the NGINX integration, not by `validate_request` itself.

## Tests

Run the library's unit and integration tests with Rust/Cargo; these do not require NGINX or Docker:

```sh
cargo test -p openapi-validator --locked
```

The [test README](crates/openapi-validator/tests/README.md) describes coverage, fixture provenance, and compatibility decisions.

Run end-to-end checks with Docker Compose, Bash, `curl`, and Python 3:

```sh
bash example/test.sh
```

The script builds and starts the example, checks routing, parameters, bodies, and log variables, and shuts down the Compose services on exit. To check an already-running instance of the example configuration instead:

```sh
OAV_BASE_URL=http://localhost:18088 \
OAV_LOG_CMD='docker compose logs nginx' \
bash example/test.sh
```

`OAV_LOG_CMD` runs from the `example/` directory. Omit it to skip log-variable checks.

## Scope and limitations

- Specifications must be JSON. YAML and Swagger/OpenAPI 2.0 are not supported.
- Response validation and OpenAPI security-scheme enforcement are not implemented.
- Parameter validation uses `schema`; parameter `content` schemas are not implemented.
- JSON, URL-encoded forms, and XML have body decoders. Other media types, including multipart uploads, are treated as opaque: declared media type and required-body presence can be checked, but body schemas are not evaluated.
- Body and content-type checks apply when the operation declares `requestBody`. Undeclared request bodies are not rejected automatically.
- With body validation enabled, NGINX buffers the whole request body before proxying, even with `proxy_request_buffering off`. Bodies spooled to temporary files are supported and are read into memory for validation. Set `client_max_body_size` for your workload.
- JSON body values retain their types: a string such as `"42"` does not satisfy an integer schema. Parameter and form decoding perform schema-aware conversion.
- Compilation prepares request validation; it is not a complete OpenAPI document conformance check. See the test README for the precise scope of parity coverage.

## Project layout

```text
crates/
  openapi-validator/       Framework-independent validation library and tests
  ngx-oav-module/          NGINX dynamic module, directives, and variables
example/
  api-spec.json           Example Pet Store specification
  nginx.conf              Validation and proxy configuration
  Dockerfile              Module build and NGINX runtime image
  docker-compose.yml      NGINX and echo-backend services
  test.sh                 End-to-end checks
```
