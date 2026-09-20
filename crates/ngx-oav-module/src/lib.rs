use core::ffi::{c_char, c_void};
use core::ptr;
use std::sync::Arc;

use ngx::core::Status;
use ngx::ffi::{
    NGX_CONF_TAKE1, NGX_HTTP_LOC_CONF, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_MODULE,
    NGX_HTTP_VAR_NOCACHEABLE, NGX_LOG_EMERG, NGX_LOG_ERR, NGX_LOG_WARN, ngx_array_push,
    ngx_command_t, ngx_conf_t, ngx_http_add_variable, ngx_http_core_main_conf_t,
    ngx_http_handler_pt, ngx_http_module_t, ngx_http_phases_NGX_HTTP_ACCESS_PHASE,
    ngx_int_t, ngx_module_t, ngx_str_t, ngx_uint_t, ngx_variable_value_t,
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
use openapi_validator::error::ValidationResult;

// ---------------------------------------------------------------------------
// Per-request validation context (stored via set_module_ctx)
// ---------------------------------------------------------------------------

/// Stored in the request context after validation runs.
/// Variable get-handlers read from this.
///
/// All string fields are pre-computed so the variable getter just returns a pointer.
struct ValidationContext {
    status: &'static str,
    /// Pre-formatted count string (avoids allocation in the variable getter).
    error_count_str: String,
    first_error: String,
    /// Lazily computed JSON — only serialized if the variable is accessed.
    errors_json: once_cell::unsync::Lazy<String, Box<dyn FnOnce() -> String>>,
}

impl ValidationContext {
    fn valid() -> Self {
        Self {
            status: "valid",
            error_count_str: "0".to_string(),
            first_error: String::new(),
            errors_json: once_cell::unsync::Lazy::new(Box::new(|| "[]".to_string())),
        }
    }

    fn invalid(errors: &[openapi_validator::error::ValidationError]) -> Self {
        let first_error = errors
            .first()
            .map(|e| format!("{}: {}", e.path, e.message))
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
    let module = Module::module();
    let ctx = request.get_module_ctx::<ValidationContext>(module);

    let value: &[u8] = match ctx {
        Some(ctx) => match data {
            VAR_STATUS => ctx.status.as_bytes(),
            VAR_ERROR_COUNT => ctx.error_count_str.as_bytes(),
            VAR_FIRST_ERROR => ctx.first_error.as_bytes(),
            VAR_ERRORS_JSON => ctx.errors_json.as_bytes(),
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
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ModuleConfig {
    enabled: bool,
    enforcement: EnforcementMode,
    parts: ValidationParts,
    spec_path: Option<String>,
    compiled_spec: Option<Arc<CompiledSpec>>,
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
        if !self.enabled && prev.enabled {
            self.enabled = prev.enabled;
        }
        if self.enforcement == EnforcementMode::default() {
            self.enforcement = prev.enforcement;
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
// ---------------------------------------------------------------------------

http_request_handler!(oav_access_handler, oav_handler);

fn oav_handler(request: &mut http::Request) -> Status {
    let co = match Module::location_conf(request) {
        Some(c) => c,
        None => return Status::NGX_DECLINED,
    };

    if !co.enabled {
        return Status::NGX_DECLINED;
    }

    let compiled_spec = match &co.compiled_spec {
        Some(s) => s,
        None => return Status::NGX_DECLINED,
    };

    // Build RequestData from the nginx request.
    // We allocate one String for the URI and split it; method is a static &str.
    let method = request.method().as_str().to_string();

    let uri = request
        .unparsed_uri()
        .to_str()
        .unwrap_or("/")
        .to_string();
    let (path, query_string) = match uri.find('?') {
        Some(pos) => (uri[..pos].to_string(), Some(uri[pos + 1..].to_string())),
        None => (uri, None),
    };

    // Collect headers — only allocate strings for headers we actually need.
    let mut headers: Vec<(String, String)> = Vec::with_capacity(8);
    for (key, value) in request.headers_in_iterator() {
        if let (Ok(k), Ok(v)) = (key.to_str(), value.to_str()) {
            headers.push((k.to_string(), v.to_string()));
        }
    }

    // Pass config by reference — avoid cloning ValidationParts.
    let config = ValidationConfig {
        enabled: co.enabled,
        enforcement: co.enforcement,
        parts: co.parts.clone(),
    };

    // NOTE: Body validation requires async FFI (ngx_http_read_client_request_body).
    // For now we validate everything except body content.
    let req_data = openapi_validator::RequestData {
        method,
        path,
        query_string,
        headers,
        body: None,
    };

    let result = openapi_validator::validate_request(compiled_spec, &req_data, &config);

    // Store validation context for variable access in log_format.
    // The Box is freed when nginx cleans up the request pool (via module ctx cleanup).
    let ctx = match &result {
        ValidationResult::Valid => Box::new(ValidationContext::valid()),
        ValidationResult::Invalid(errors) => Box::new(ValidationContext::invalid(errors)),
    };
    request.set_module_ctx(Box::into_raw(ctx) as *mut c_void, Module::module());

    match result {
        ValidationResult::Valid => Status::NGX_DECLINED,
        ValidationResult::Invalid(ref errors) => match co.enforcement {
            EnforcementMode::Audit => {
                for err in errors {
                    ngx_log_error!(
                        NGX_LOG_ERR,
                        request.log(),
                        "openapi_validate [audit]: {} - {}",
                        err.path,
                        err.message
                    );
                }
                Status::NGX_DECLINED
            }
            EnforcementMode::Block => {
                let status_code = result.http_status().unwrap_or(400);
                match status_code {
                    404 => HTTPStatus::NOT_FOUND.into(),
                    405 => HTTPStatus::NOT_ALLOWED.into(),
                    415 => HTTPStatus::UNSUPPORTED_MEDIA_TYPE.into(),
                    _ => HTTPStatus::BAD_REQUEST.into(),
                }
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

extern "C" fn cmd_set_enable(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    unsafe {
        let conf = &mut *(conf as *mut ModuleConfig);
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "openapi_validate: invalid utf-8");
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        if val.eq_ignore_ascii_case("on") {
            conf.enabled = true;
        } else if val.eq_ignore_ascii_case("off") {
            conf.enabled = false;
        } else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "openapi_validate: expected 'on' or 'off'"
            );
            return ngx::core::NGX_CONF_ERROR;
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
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let path = match args[1].to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                ngx_conf_log_error!(NGX_LOG_EMERG, cf, "openapi_spec: invalid utf-8 in path");
                return ngx::core::NGX_CONF_ERROR;
            }
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
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_validate_mode: invalid utf-8"
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        if val.eq_ignore_ascii_case("block") {
            conf.enforcement = EnforcementMode::Block;
        } else if val.eq_ignore_ascii_case("audit") {
            conf.enforcement = EnforcementMode::Audit;
        } else {
            ngx_conf_log_error!(
                NGX_LOG_EMERG,
                cf,
                "openapi_validate_mode: expected 'block' or 'audit'"
            );
            return ngx::core::NGX_CONF_ERROR;
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
        let args: &[ngx_str_t] = (*(*cf).args).as_slice();

        let val = match args[1].to_str() {
            Ok(s) => s,
            Err(_) => {
                ngx_conf_log_error!(
                    NGX_LOG_EMERG,
                    cf,
                    "openapi_validate_parts: invalid utf-8"
                );
                return ngx::core::NGX_CONF_ERROR;
            }
        };

        // Parse comma-separated list of parts
        // Reset all parts to false, then enable only what's specified
        conf.parts = ValidationParts {
            path: false,
            method: false,
            path_params: false,
            query_params: false,
            header_params: false,
            cookie_params: false,
            content_type: false,
            body: false,
            disallow_additional_query_params: false,
        };

        for part in val.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match part.to_ascii_lowercase().as_str() {
                "path" => conf.parts.path = true,
                "method" => conf.parts.method = true,
                "path_params" => conf.parts.path_params = true,
                "query_params" => conf.parts.query_params = true,
                "header_params" => conf.parts.header_params = true,
                "cookie_params" => conf.parts.cookie_params = true,
                "content_type" => conf.parts.content_type = true,
                "body" => conf.parts.body = true,
                "disallow_additional_query_params" => {
                    conf.parts.disallow_additional_query_params = true;
                }
                "all" => {
                    conf.parts = ValidationParts::default();
                }
                _ => {
                    ngx_conf_log_error!(
                        NGX_LOG_WARN,
                        cf,
                        "openapi_validate_parts: unknown part"
                    );
                }
            }
        }
    }
    ngx::core::NGX_CONF_OK
}
