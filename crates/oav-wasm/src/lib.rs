//! WebAssembly bindings used by the browser demo in `demo/`.
//!
//! The demo mirrors what the nginx module does for one request in one
//! `location`: the configuration is given as the literal directive values and
//! parsed by the same code the module uses, the request URI is normalized the
//! way nginx builds `r->uri`, and the outcome is reported as the phase-handler
//! decision plus the `$oav_*` variable values.
//!
//! The logic lives in plain Rust functions so it can be tested natively; the
//! `#[wasm_bindgen]` layer only passes JSON strings across the boundary.

use openapi_validator::compiled_spec::EnforcementMode;
use openapi_validator::error::ValidationResult;
use openapi_validator::{
    validate_request, CompiledSpec, RequestData, ValidationConfig, ValidationParts,
};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

/// Longest text placed in `$oav_first_error` or an audit log line, matching
/// the nginx module.
const MAX_LOG_VALUE_LEN: usize = 512;

/// A compiled OpenAPI document, the equivalent of `openapi_spec`.
#[wasm_bindgen]
pub struct Spec {
    inner: CompiledSpec,
}

#[wasm_bindgen]
impl Spec {
    /// Compile a JSON OpenAPI document. Fails with the same message nginx
    /// reports when `openapi_spec` cannot load a file.
    #[wasm_bindgen(constructor)]
    pub fn new(json: &str) -> Result<Spec, JsError> {
        CompiledSpec::from_json(json)
            .map(|inner| Spec { inner })
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Run one request through the simulated module.
    ///
    /// `directives` is a JSON object with the directive values as strings:
    /// `{"openapi_validate": "on", "openapi_validate_mode": "block",
    /// "openapi_validate_parts": "all"}`. A missing key takes the module's
    /// default.
    ///
    /// `request` is a JSON object: `{"method": "GET", "uri": "/pets?x=1",
    /// "headers": [["Name", "value"], ...], "body": "..."}`.
    ///
    /// Returns the outcome as a JSON string; see [`simulate`].
    pub fn validate(&self, directives: &str, request: &str) -> Result<String, JsError> {
        let directives: Value = serde_json::from_str(directives)
            .map_err(|e| JsError::new(&format!("directives: {e}")))?;
        let request: Value =
            serde_json::from_str(request).map_err(|e| JsError::new(&format!("request: {e}")))?;
        let config = parse_directives(&directives).map_err(|e| JsError::new(&e))?;
        let request = parse_request(&request).map_err(|e| JsError::new(&e))?;
        Ok(simulate(&self.inner, &config, request).to_string())
    }
}

/// The names accepted by `openapi_validate_parts` and whether each is in the
/// default (`all`) selection, as a JSON array of `{"name", "default"}`.
#[wasm_bindgen(js_name = partNames)]
pub fn part_names() -> String {
    let defaults = ValidationParts::default();
    let names: Vec<Value> = ValidationParts::NAMES
        .iter()
        .map(|name| {
            let only = ValidationParts::parse(name).expect("NAMES entries parse");
            json!({ "name": name, "default": is_subset(&only, &defaults) })
        })
        .collect();
    Value::Array(names).to_string()
}

fn is_subset(a: &ValidationParts, b: &ValidationParts) -> bool {
    (!a.path || b.path)
        && (!a.method || b.method)
        && (!a.path_params || b.path_params)
        && (!a.query_params || b.query_params)
        && (!a.header_params || b.header_params)
        && (!a.cookie_params || b.cookie_params)
        && (!a.content_type || b.content_type)
        && (!a.body || b.body)
        && (!a.disallow_additional_query_params || b.disallow_additional_query_params)
}

/// Parse directive values with the same rules and messages as nginx.
pub fn parse_directives(d: &Value) -> Result<ValidationConfig, String> {
    let get = |key: &str| d.get(key).and_then(Value::as_str);

    let enabled = match get("openapi_validate") {
        None => false,
        Some(v) if v.eq_ignore_ascii_case("on") => true,
        Some(v) if v.eq_ignore_ascii_case("off") => false,
        Some(_) => return Err("openapi_validate: expected 'on' or 'off'".into()),
    };
    let enforcement = match get("openapi_validate_mode") {
        None => EnforcementMode::default(),
        Some(v) => v
            .parse()
            .map_err(|()| "openapi_validate_mode: expected 'block' or 'audit'".to_string())?,
    };
    let parts = match get("openapi_validate_parts") {
        None => ValidationParts::default(),
        Some(v) => ValidationParts::parse(v)
            .map_err(|p| format!("openapi_validate_parts: unknown part '{p}'"))?,
    };
    Ok(ValidationConfig {
        enabled,
        enforcement,
        parts,
    })
}

/// A request as typed into the demo, before nginx-style URI processing.
pub struct DemoRequest {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

fn parse_request(r: &Value) -> Result<DemoRequest, String> {
    let method = r
        .get("method")
        .and_then(Value::as_str)
        .ok_or("request.method must be a string")?
        .to_string();
    let uri = r
        .get("uri")
        .and_then(Value::as_str)
        .ok_or("request.uri must be a string")?
        .to_string();
    let mut headers = Vec::new();
    for pair in r
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match pair.as_array().map(Vec::as_slice) {
            Some([Value::String(k), Value::String(v)]) => headers.push((k.clone(), v.clone())),
            _ => return Err("request.headers must be [name, value] string pairs".into()),
        }
    }
    let body = r
        .get("body")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty())
        .map(|b| b.as_bytes().to_vec());
    Ok(DemoRequest {
        method,
        uri,
        headers,
        body,
    })
}

/// Run one request through the same steps as the module's access handler.
///
/// The result has:
/// * `outcome`: `"skipped"` (validation off), `"rejected_by_nginx"` (the URI
///   is invalid before the module runs), `"passed"`, `"blocked"`, or
///   `"logged"` (invalid, audit mode);
/// * `http_status`: the status nginx responds with when it stops the request;
/// * `uri` / `args`: the normalized `$uri` and `$args` the validator saw;
/// * `variables`: the `$oav_*` values for the access log;
/// * `errors`: the validation errors;
/// * `error_log`: lines audit mode writes to the error log.
pub fn simulate(spec: &CompiledSpec, config: &ValidationConfig, req: DemoRequest) -> Value {
    let (raw_path, args) = split_uri(&req.uri);
    let Some(path) = normalize_path(raw_path) else {
        return json!({
            "outcome": "rejected_by_nginx",
            "http_status": 400,
            "reason": "nginx rejects this URI while parsing the request line",
            "variables": unset_variables(),
            "errors": [],
            "error_log": [],
        });
    };

    if !config.enabled {
        return json!({
            "outcome": "skipped",
            "http_status": null,
            "uri": path,
            "args": args,
            "variables": unset_variables(),
            "errors": [],
            "error_log": [],
        });
    }

    // The module reads the body only when body validation is on, so with
    // `body` disabled the validator sees a request without one.
    let body = if config.parts.body { req.body } else { None };
    let request = RequestData {
        method: req.method,
        path: path.clone(),
        query_string: args.map(str::to_string),
        headers: req.headers,
        body,
    };
    let result = validate_request(spec, &request, config);

    let errors = match &result {
        ValidationResult::Valid => Vec::new(),
        ValidationResult::Invalid(errors) => errors.clone(),
    };
    let variables = match &result {
        ValidationResult::Valid => json!({
            "oav_status": "valid",
            "oav_error_count": "0",
            "oav_first_error": "",
            "oav_errors_json": "[]",
        }),
        ValidationResult::Invalid(errors) => json!({
            "oav_status": "invalid",
            "oav_error_count": errors.len().to_string(),
            "oav_first_error": errors
                .first()
                .map(|e| truncate(&format!("{}: {}", e.path, e.message), MAX_LOG_VALUE_LEN))
                .unwrap_or_default(),
            "oav_errors_json": serde_json::to_string(errors).unwrap_or_else(|_| "[]".into()),
        }),
    };

    let (outcome, http_status, error_log) = match (&result, config.enforcement) {
        (ValidationResult::Valid, _) => ("passed", None, Vec::new()),
        (ValidationResult::Invalid(errors), EnforcementMode::Audit) => {
            let lines = errors
                .iter()
                .map(|e| {
                    format!(
                        "openapi_validate [audit]: {} - {}",
                        sanitize_for_log(&e.path),
                        sanitize_for_log(&e.message)
                    )
                })
                .collect();
            ("logged", None, lines)
        }
        (ValidationResult::Invalid(_), EnforcementMode::Block) => {
            let status = match result.http_status().unwrap_or(400) {
                s @ (404 | 405 | 415) => s,
                _ => 400,
            };
            ("blocked", Some(status), Vec::new())
        }
    };

    json!({
        "outcome": outcome,
        "http_status": http_status,
        "uri": path,
        "args": args,
        "variables": variables,
        "errors": errors,
        "error_log": error_log,
    })
}

/// Variable values when the module produced no result.
fn unset_variables() -> Value {
    json!({
        "oav_status": "-",
        "oav_error_count": "-",
        "oav_first_error": "-",
        "oav_errors_json": "-",
    })
}

/// Split a request target into path and query string, dropping any fragment.
fn split_uri(uri: &str) -> (&str, Option<&str>) {
    let uri = uri.split('#').next().unwrap_or("");
    match uri.split_once('?') {
        Some((path, query)) => (path, Some(query).filter(|q| !q.is_empty())),
        None => (uri, None),
    }
}

/// Build `$uri` from the request path the way nginx does with its default
/// settings: percent-decode, merge repeated slashes, and resolve `.` and `..`
/// segments. Returns `None` for paths nginx rejects with 400.
pub fn normalize_path(raw: &str) -> Option<String> {
    if !raw.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(raw)?;
    let decoded = String::from_utf8_lossy(&decoded);

    let mut segments: Vec<&str> = Vec::new();
    let mut parts = decoded.split('/').skip(1).peekable();
    let mut trailing_slash = false;
    while let Some(seg) = parts.next() {
        let last = parts.peek().is_none();
        match seg {
            "" | "." => trailing_slash = last,
            ".." => {
                segments.pop()?;
                trailing_slash = last;
            }
            s => {
                segments.push(s);
                trailing_slash = false;
            }
        }
    }

    let mut out = String::with_capacity(decoded.len());
    for seg in &segments {
        out.push('/');
        out.push_str(seg);
    }
    if trailing_slash || segments.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// Decode `%XX` escapes. nginx rejects a malformed escape and a decoded NUL.
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let byte = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            if byte == 0 {
                return None;
            }
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

fn sanitize_for_log(s: &str) -> String {
    truncate(s, MAX_LOG_VALUE_LEN)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../../example/api-spec.json");

    fn run(
        directives: Value,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Value {
        let spec = CompiledSpec::from_json(SPEC).unwrap();
        let config = parse_directives(&directives).unwrap();
        let req = DemoRequest {
            method: method.into(),
            uri: uri.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Some(body.as_bytes().to_vec()).filter(|b| !b.is_empty()),
        };
        simulate(&spec, &config, req)
    }

    fn on() -> Value {
        json!({ "openapi_validate": "on" })
    }

    #[test]
    fn directive_errors_match_nginx() {
        let err = parse_directives(&json!({ "openapi_validate_parts": "path,nope" })).unwrap_err();
        assert_eq!(err, "openapi_validate_parts: unknown part 'nope'");
        let err = parse_directives(&json!({ "openapi_validate_mode": "warn" })).unwrap_err();
        assert_eq!(err, "openapi_validate_mode: expected 'block' or 'audit'");
    }

    #[test]
    fn validation_is_off_by_default() {
        let out = run(json!({}), "GET", "/nowhere", &[], "");
        assert_eq!(out["outcome"], "skipped");
        assert_eq!(out["variables"]["oav_status"], "-");
    }

    #[test]
    fn valid_request_passes() {
        let out = run(on(), "GET", "/api/pets?status=available&limit=10", &[], "");
        assert_eq!(out["outcome"], "passed", "{out}");
        assert_eq!(out["variables"]["oav_status"], "valid");
    }

    #[test]
    fn block_mode_uses_most_specific_status() {
        let out = run(on(), "GET", "/api/unknown", &[], "");
        assert_eq!(out["outcome"], "blocked");
        assert_eq!(out["http_status"], 404);
        let out = run(
            on(),
            "POST",
            "/api/pets",
            &[("Content-Type", "text/plain")],
            "hello",
        );
        assert_eq!(out["http_status"], 415);
    }

    #[test]
    fn audit_mode_logs_and_passes() {
        let mut d = on();
        d["openapi_validate_mode"] = "audit".into();
        let out = run(d, "GET", "/api/pets", &[], "");
        assert_eq!(out["outcome"], "logged");
        assert!(out["http_status"].is_null());
        let line = out["error_log"][0].as_str().unwrap();
        assert!(line.starts_with("openapi_validate [audit]: "), "{line}");
    }

    #[test]
    fn body_part_off_means_body_is_not_read() {
        let json = [("Content-Type", "application/json")];
        let out = run(on(), "POST", "/api/pets", &json, "not json");
        assert_eq!(out["outcome"], "blocked", "{out}");

        let mut d = on();
        d["openapi_validate_parts"] = "path,method".into();
        let out = run(d, "POST", "/api/pets", &json, "not json");
        assert_eq!(out["outcome"], "passed", "{out}");
    }

    #[test]
    fn uri_is_normalized_like_nginx() {
        assert_eq!(
            normalize_path("/api//pets/./x/..").as_deref(),
            Some("/api/pets/")
        );
        assert_eq!(normalize_path("/api/%70ets").as_deref(), Some("/api/pets"));
        assert_eq!(normalize_path("/").as_deref(), Some("/"));
        assert_eq!(normalize_path("/.."), None);
        assert_eq!(normalize_path("/a%2"), None);
        assert_eq!(normalize_path("/a%00"), None);
    }

    #[test]
    fn part_names_flag_defaults() {
        let names: Value = serde_json::from_str(&part_names()).unwrap();
        let find = |n: &str| {
            names
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["name"] == n)
                .unwrap()["default"]
                .clone()
        };
        assert_eq!(find("body"), true);
        assert_eq!(find("cookie_params"), false);
        assert_eq!(find("disallow_additional_query_params"), false);
    }
}
