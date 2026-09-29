//! nginx module that validates requests against an OpenAPI document.
//!
//! Note on request bodies: when body validation is enabled the whole body is
//! read and buffered in the access phase, before the content handler runs.
//! In a validated location `proxy_request_buffering off` therefore has no
//! effect; the body is always buffered up to `client_max_body_size`.

use core::ffi::{c_char, c_void};
use core::ptr;
use std::cell::{Cell, OnceCell};
use std::fs::File;
use std::mem::ManuallyDrop;
use std::os::unix::fs::FileExt;
use std::os::unix::io::FromRawFd;
use std::sync::Arc;

use ngx::core::{NgxStr, Status};
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_DONE, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE,
    NGX_HTTP_SPECIAL_RESPONSE, NGX_HTTP_VAR_NOCACHEABLE, NGX_LOG_EMERG, NGX_LOG_ERR,
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_add_variable,
    ngx_http_core_main_conf_t, ngx_http_core_run_phases, ngx_http_finalize_request,
    ngx_http_handler_pt, ngx_http_module_t, ngx_http_phases_NGX_HTTP_ACCESS_PHASE,
    ngx_http_read_client_request_body, ngx_http_request_t, ngx_int_t, ngx_module_t, ngx_str_t,
    ngx_uint_t, ngx_variable_value_t,
};
use ngx::http::{
    self, HTTPStatus, HttpModule, HttpModuleLocationConf, HttpModuleMainConf, MergeConfigError,
    NgxHttpCoreModule,
};
use ngx::{
    http_request_handler, http_variable_get, ngx_conf_log_error, ngx_log_error, ngx_string,
};

use openapi_validator::compiled_spec::{
    CompiledSpec, EnforcementMode, ValidationConfig, ValidationParts,
};
use openapi_validator::error::{ValidationError, ValidationResult};
use openapi_validator::RequestData;

// ---------------------------------------------------------------------------
// Per-request context
//
// Allocated from the request pool (so nginx drops it with the request) and
// stored via set_module_ctx. It serves two purposes:
//   * remembering the phase-handler decision across the asynchronous body
//     read, and
//   * exposing the outcome to the $oav_* variables.
// ---------------------------------------------------------------------------

///
/// nginx only ever hands out shared references to module contexts, so the
/// fields use interior mutability. A request is serviced by one worker
/// thread, which makes `Cell` and `OnceCell` sufficient.
struct ValidationContext {
    /// Value the access handler returns once validation has completed.
    /// `NGX_DONE` while the request body is still being read.
    decision: Cell<ngx_int_t>,
    /// Variable values, empty until validation has run.
    vars: OnceCell<ValidationVars>,
}

struct ValidationVars {
    status: &'static str,
    /// Pre-formatted count string (avoids allocation in the variable getter).
    error_count_str: String,
    first_error: String,
    /// Lazily computed JSON — only serialized if the variable is accessed.
    errors_json: once_cell::unsync::Lazy<String, Box<dyn FnOnce() -> String>>,
}

impl ValidationContext {
    fn pending() -> Self {
        Self {
            decision: Cell::new(NGX_DONE as ngx_int_t),
            vars: OnceCell::new(),
        }
    }
}

impl ValidationVars {
    fn valid() -> Self {
        Self {
            status: "valid",
            error_count_str: "0".to_string(),
            first_error: String::new(),
            errors_json: once_cell::unsync::Lazy::new(Box::new(|| "[]".to_string())),
        }
    }

    fn invalid(errors: &[ValidationError]) -> Self {
        let first_error = errors
            .first()
            .map(|e| truncate(&format!("{}: {}", e.path, e.message), MAX_LOG_VALUE_LEN))
            .unwrap_or_default();
        let error_count_str = errors.len().to_string();
        // Clone errors for lazy JSON serialization
        let errors_owned = errors.to_vec();
        Self {
            status: "invalid",
            error_count_str,
            first_error,
            errors_json: once_cell::unsync::Lazy::new(Box::new(move || {
                serde_json::to_string(&errors_owned).unwrap_or_else(|_| "[]".to_string())
            })),
        }
    }
}

/// This module's context for the request, if validation has started.
fn ctx(request: &http::Request) -> Option<&ValidationContext> {
    request.get_module_ctx::<ValidationContext>(Module::module())
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

struct Module;

impl http::HttpModule for Module {
    fn module() -> &'static ngx_module_t {
        unsafe { &*ptr::addr_of!(ngx_http_oav_module) }
    }

    unsafe extern "C" fn preconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        unsafe {
            if register_variables(cf).is_err() {
                return Status::NGX_ERROR.into();
            }
            Status::NGX_OK.into()
        }
    }

    unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
        unsafe {
            let cmcf = match NgxHttpCoreModule::main_conf(&*cf) {
                Some(c) => c as *const ngx_http_core_main_conf_t as *mut ngx_http_core_main_conf_t,
                None => return Status::NGX_ERROR.into(),
            };

            let h = ngx_array_push(
                &raw mut (*cmcf).phases[ngx_http_phases_NGX_HTTP_ACCESS_PHASE as usize].handlers,
            ) as *mut ngx_http_handler_pt;
            if h.is_null() {
                return Status::NGX_ERROR.into();
            }
            *h = Some(oav_access_handler);

            Status::NGX_OK.into()
        }
    }
}

// ---------------------------------------------------------------------------
// Variable registration
// ---------------------------------------------------------------------------

/// Variable discriminant passed via the `data` field of each variable.
const VAR_STATUS: usize = 0;
const VAR_ERROR_COUNT: usize = 1;
const VAR_FIRST_ERROR: usize = 2;
const VAR_ERRORS_JSON: usize = 3;

unsafe fn register_variable(
    cf: *mut ngx_conf_t,
    name: &mut ngx_str_t,
    data: usize,
) -> Result<(), ()> {
    let var = ngx_http_add_variable(cf, name, NGX_HTTP_VAR_NOCACHEABLE as ngx_uint_t);
    if var.is_null() {
        return Err(());
    }
    (*var).get_handler = Some(oav_variable_handler);
    (*var).data = data;
    Ok(())
}

unsafe fn register_variables(cf: *mut ngx_conf_t) -> Result<(), ()> {
    register_variable(cf, &mut ngx_string!("oav_status"), VAR_STATUS)?;
    register_variable(cf, &mut ngx_string!("oav_error_count"), VAR_ERROR_COUNT)?;
    register_variable(cf, &mut ngx_string!("oav_first_error"), VAR_FIRST_ERROR)?;
    register_variable(cf, &mut ngx_string!("oav_errors_json"), VAR_ERRORS_JSON)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Variable get-handler
// ---------------------------------------------------------------------------

http_variable_get!(oav_variable_handler, oav_variable_get);

fn oav_variable_get(
    request: &mut http::Request,
    v: *mut ngx_variable_value_t,
    data: usize,
) -> Status {
    let vars = ctx(request).and_then(|ctx| ctx.vars.get());

    let value: &[u8] = match vars {
        Some(vars) => match data {
            VAR_STATUS => vars.status.as_bytes(),
            VAR_ERROR_COUNT => vars.error_count_str.as_bytes(),
            VAR_FIRST_ERROR => vars.first_error.as_bytes(),
            VAR_ERRORS_JSON => vars.errors_json.as_bytes(),
            _ => b"-",
        },
        None => b"-",
    };

    unsafe {
        (*v).set_len(value.len() as u32);
        (*v).set_valid(1);
        (*v).set_not_found(0);
        (*v).data = value.as_ptr() as *mut u8;
    }

    Status::NGX_OK
}

// ---------------------------------------------------------------------------
// Location configuration
//
// Every setting is optional so that "not set here" can be told apart from an
// explicit value, and inherited from the enclosing block on merge.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ModuleConfig {
    enabled: Option<bool>,
    enforcement: Option<EnforcementMode>,
    parts: Option<ValidationParts>,
    spec_path: Option<String>,
    compiled_spec: Option<Arc<CompiledSpec>>,
}

impl ModuleConfig {
    fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    fn enforcement(&self) -> EnforcementMode {
        self.enforcement.unwrap_or_default()
    }

    fn validation_config(&self) -> ValidationConfig {
        ValidationConfig {
            enabled: self.enabled(),
            enforcement: self.enforcement(),
            parts: self.parts.clone().unwrap_or_default(),
        }
    }
}

unsafe impl HttpModuleLocationConf for Module {
    type LocationConf = ModuleConfig;
}

impl http::Merge for ModuleConfig {
    fn merge(&mut self, prev: &ModuleConfig) -> Result<(), MergeConfigError> {
        if self.spec_path.is_none() {
            self.spec_path.clone_from(&prev.spec_path);
            self.compiled_spec.clone_from(&prev.compiled_spec);
        }
        if self.enabled.is_none() {
            self.enabled = prev.enabled;
        }
        if self.enforcement.is_none() {
            self.enforcement = prev.enforcement;
        }
        if self.parts.is_none() {
            self.parts.clone_from(&prev.parts);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Commands (nginx config directives)
// ---------------------------------------------------------------------------

static mut NGX_HTTP_OAV_COMMANDS: [ngx_command_t; 5] = [
    ngx_command_t {
        name: ngx_string!("openapi_validate"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(cmd_set_enable),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("openapi_spec"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(cmd_set_spec),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("openapi_validate_mode"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(cmd_set_mode),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_string!("openapi_validate_parts"),
        type_: (NGX_HTTP_LOC_CONF | NGX_CONF_TAKE1) as ngx_uint_t,
        set: Some(cmd_set_parts),
        conf: NGX_HTTP_LOC_CONF_OFFSET,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t::empty(),
];

// ---------------------------------------------------------------------------
// Module context and registration
// ---------------------------------------------------------------------------

static NGX_HTTP_OAV_MODULE_CTX: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(Module::preconfiguration),
    postconfiguration: Some(Module::postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: Some(Module::create_loc_conf),
    merge_loc_conf: Some(Module::merge_loc_conf),
};

#[cfg(feature = "export-modules")]
ngx::ngx_modules!(ngx_http_oav_module);

#[used]
#[allow(non_upper_case_globals)]
#[cfg_attr(not(feature = "export-modules"), unsafe(no_mangle))]
pub static mut ngx_http_oav_module: ngx_module_t = ngx_module_t {
    ctx: &raw const NGX_HTTP_OAV_MODULE_CTX as _,
    commands: unsafe { &raw mut NGX_HTTP_OAV_COMMANDS[0] },
    type_: NGX_HTTP_MODULE as _,
    ..ngx_module_t::default()
};

// ---------------------------------------------------------------------------
// Access phase handler
//
// Body handling follows the pattern of nginx's own mirror module: when a body
// is present, ask nginx to read it, return NGX_DONE, and let the body
// callback validate and then re-run the phase engine. On re-entry the handler
// finds the stored decision in the request context and returns it.
// ---------------------------------------------------------------------------

http_request_handler!(oav_access_handler, oav_handler);

fn oav_handler(request: &mut http::Request) -> Status {
    let co = match Module::location_conf(request) {
        Some(c) => c,
        None => return Status::NGX_DECLINED,
    };

    if !co.enabled() || co.compiled_spec.is_none() {
        return Status::NGX_DECLINED;
    }

    // Re-entry after the request body has been read.
    if let Some(ctx) = ctx(request) {
        return Status(ctx.decision.get());
    }

    // An error page is served through an internal redirect that clears the
    // module context. Validating the error page itself could reject it again
    // and loop until nginx gives up, so error responses pass through.
    let (has_body, serving_error_page) = {
        let raw = request.as_ref();
        (
            raw.headers_in.content_length_n > 0 || raw.headers_in.chunked() != 0,
            raw.err_status != 0,
        )
    };
    if serving_error_page {
        return Status::NGX_DECLINED;
    }

    let ctx_ptr = request.pool().allocate(ValidationContext::pending());
    if ctx_ptr.is_null() {
        return Status::NGX_ERROR;
    }
    request.set_module_ctx(ctx_ptr as *mut c_void, Module::module());

    if has_body && co.parts.as_ref().is_none_or(|p| p.body) {
        // From here on only the raw pointer touches the request; `request`
        // is not used again in this branch.
        let r = request.as_mut() as *mut ngx_http_request_t;
        let rc = unsafe { ngx_http_read_client_request_body(r, Some(oav_body_handler)) };
        if rc >= NGX_HTTP_SPECIAL_RESPONSE as ngx_int_t {
            return Status(rc);
        }
        // ngx_http_read_client_request_body took a reference on the request;
        // release it here. The body callback resumes the phase engine.
        unsafe { ngx_http_finalize_request(r, NGX_DONE as ngx_int_t) };
        return Status::NGX_DONE;
    }

    let decision = validate(request, co, None);
    if let Some(ctx) = ctx(request) {
        ctx.decision.set(decision.0);
    }
    decision
}

/// Called by nginx once the whole request body is available.
unsafe extern "C" fn oav_body_handler(r: *mut ngx_http_request_t) {
    // Everything that goes through the raw pointer happens before a
    // reference to the request exists, and the final writes go through that
    // reference, so no two live paths alias the request.
    let body = unsafe { read_request_body(r) };
    let request = unsafe { http::Request::from_ngx_http_request(r) };

    let decision = match (Module::location_conf(request), body) {
        (Some(co), Ok(body)) => validate(request, co, body),
        (Some(_), Err(())) => {
            ngx_log_error!(
                NGX_LOG_ERR,
                request.log(),
                "openapi_validate: failed to read request body"
            );
            HTTPStatus::INTERNAL_SERVER_ERROR.into()
        }
        (None, _) => Status::NGX_DECLINED,
    };

    if let Some(ctx) = ctx(request) {
        ctx.decision.set(decision.0);
    }

    let raw = request.as_mut();
    raw.set_preserve_body(1);
    raw.write_event_handler = Some(ngx_http_core_run_phases);
    let r = raw as *mut ngx_http_request_t;
    unsafe { ngx_http_core_run_phases(r) };
}

/// Collect the buffered request body into one contiguous byte vector.
///
/// nginx may keep the body in memory buffers, in a temporary file, or both.
unsafe fn read_request_body(r: *mut ngx_http_request_t) -> Result<Option<Vec<u8>>, ()> {
    let rb = (*r).request_body;
    if rb.is_null() {
        return Ok(None);
    }

    let mut out = Vec::new();
    let mut chain = (*rb).bufs;
    while !chain.is_null() {
        let buf = (*chain).buf;
        if !buf.is_null() {
            if (*buf).in_file() != 0 {
                let file = (*buf).file;
                if file.is_null() || (*buf).file_last < (*buf).file_pos {
                    return Err(());
                }
                let len = ((*buf).file_last - (*buf).file_pos) as usize;
                let start = out.len();
                out.resize(start + len, 0);
                // Borrow nginx's descriptor without taking ownership of it.
                let f = ManuallyDrop::new(File::from_raw_fd((*file).fd));
                f.read_exact_at(&mut out[start..], (*buf).file_pos as u64)
                    .map_err(|_| ())?;
            } else if !(*buf).pos.is_null() {
                if (*buf).last < (*buf).pos {
                    return Err(());
                }
                let len = (*buf).last.offset_from((*buf).pos) as usize;
                out.extend_from_slice(core::slice::from_raw_parts((*buf).pos, len));
            }
        }
        chain = (*chain).next;
    }

    Ok(if out.is_empty() { None } else { Some(out) })
}

/// Run validation, record the outcome for the `$oav_*` variables, and turn
/// it into the phase-handler return value.
fn validate(request: &mut http::Request, co: &ModuleConfig, body: Option<Vec<u8>>) -> Status {
    let Some(compiled_spec) = co.compiled_spec.as_ref() else {
        return Status::NGX_DECLINED;
    };

    let req_data = build_request_data(request, body);
    let config = co.validation_config();
    let result = openapi_validator::validate_request(compiled_spec, &req_data, &config);

    let vars = match &result {
        ValidationResult::Valid => ValidationVars::valid(),
        ValidationResult::Invalid(errors) => ValidationVars::invalid(errors),
    };
    if let Some(ctx) = ctx(request) {
        // Ignore the error: a second validation run (e.g. after an internal
        // redirect re-creates the context) cannot happen on the same context.
        let _ = ctx.vars.set(vars);
    }

    match result {
        ValidationResult::Valid => Status::NGX_DECLINED,
        ValidationResult::Invalid(ref errors) => match co.enforcement() {
            EnforcementMode::Audit => {
                for err in errors {
                    ngx_log_error!(
                        NGX_LOG_ERR,
                        request.log(),
                        "openapi_validate [audit]: {} - {}",
                        sanitize_for_log(&err.path),
                        sanitize_for_log(&err.message)
                    );
                }
                Status::NGX_DECLINED
            }
            EnforcementMode::Block => match result.http_status().unwrap_or(400) {
                404 => HTTPStatus::NOT_FOUND.into(),
                405 => HTTPStatus::NOT_ALLOWED.into(),
                415 => HTTPStatus::UNSUPPORTED_MEDIA_TYPE.into(),
                _ => HTTPStatus::BAD_REQUEST.into(),
            },
        },
    }
}

/// Build the framework-neutral request view the validator works on.
///
/// The path comes from `r->uri`, which nginx has already percent-decoded and
/// normalized (the same value it used to select this location). The query
/// string comes from `r->args`, so the validator never sees the raw URI.
fn build_request_data(request: &http::Request, body: Option<Vec<u8>>) -> RequestData {
    let raw = request.as_ref();

    let method = request.method().as_str().to_string();
    let path = request.path().to_string_lossy().into_owned();
    let query_string = {
        let args = unsafe { NgxStr::from_ngx_str(raw.args) };
        if args.is_empty() {
            None
        } else {
            Some(args.to_string_lossy().into_owned())
        }
    };

    // Header values are not required to be UTF-8; a lossy conversion keeps
    // the header visible to validation instead of silently dropping it.
    let mut headers: Vec<(String, String)> = Vec::with_capacity(16);
    for (key, value) in request.headers_in_iterator() {
        headers.push((
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        ));
    }

    RequestData {
        method,
        path,
        query_string,
        headers,
        body,
    }
}

/// Longest client-derived text placed in a log line or variable. Schema
/// errors quote instance values, which can be as large as the body.
const MAX_LOG_VALUE_LEN: usize = 512;

/// Strip control characters so client-controlled values cannot inject fake
/// lines into the error log, and bound the length.
fn sanitize_for_log(s: &str) -> String {
    truncate(s, MAX_LOG_VALUE_LEN)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Cut `s` to at most `max` bytes on a character boundary, marking the cut.
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

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

/// Read the single argument of a `TAKE1` directive as UTF-8.
unsafe fn directive_arg<'a>(cf: *mut ngx_conf_t) -> Option<&'a str> {
    let args: &[ngx_str_t] = (*(*cf).args).as_slice();
    args.get(1).and_then(|a| a.to_str().ok())
}

extern "C" fn cmd_set_enable(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        match directive_arg(cf) {
            Some(v) if v.eq_ignore_ascii_case("on") => conf.enabled = Some(true),
            Some(v) if v.eq_ignore_ascii_case("off") => conf.enabled = Some(false),
            _ => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_validate: expected 'on' or 'off'"
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        }
    }
    ngx::core::NGX_CONF_OK
}

extern "C" fn cmd_set_spec(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);

        let Some(arg) = directive_arg(cf) else {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "openapi_spec: invalid utf-8 in path");
            return ngx::core::NGX_CONF_ERROR;
        };

        // Relative paths are resolved against the nginx prefix, like other
        // file directives.
        let path = if arg.starts_with('/') {
            arg.to_string()
        } else {
            let prefix = NgxStr::from_ngx_str((*(*cf).cycle).prefix).to_string_lossy();
            format!("{prefix}{arg}")
        };

        // Parse and compile the spec at config time
        let spec_content = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_spec: failed to read {}: {}",
                    path,
                    e
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        let compiled = match CompiledSpec::from_json(&spec_content) {
            Ok(c) => c,
            Err(e) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_spec: failed to load {}: {}",
                    path,
                    e
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        conf.spec_path = Some(path);
        conf.compiled_spec = Some(Arc::new(compiled));
    }
    ngx::core::NGX_CONF_OK
}

extern "C" fn cmd_set_mode(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        match directive_arg(cf).map(str::parse::<EnforcementMode>) {
            Some(Ok(mode)) => conf.enforcement = Some(mode),
            _ => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_validate_mode: expected 'block' or 'audit'"
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        }
    }
    ngx::core::NGX_CONF_OK
}

extern "C" fn cmd_set_parts(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);

        let Some(val) = directive_arg(cf) else {
            ngx_conf_log_error!(NGX_LOG_EMERG, cf, "openapi_validate_parts: invalid utf-8");
            return ngx::core::NGX_CONF_ERROR;
        };

        match ValidationParts::parse(val) {
            Ok(parts) => conf.parts = Some(parts),
            Err(other) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_validate_parts: unknown part '{}'",
                    other
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        }
    }
    ngx::core::NGX_CONF_OK
}
