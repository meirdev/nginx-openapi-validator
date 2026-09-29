// Browser demo of the nginx OpenAPI validator module.
//
// All validation happens in crates/oav-wasm (compiled to pkg/). This file only
// wires the form to it: it turns the controls into directive values, sends one
// request through Spec.validate(), and renders the outcome.

import init, { Spec, partNames } from "./pkg/oav_wasm.js";

const $ = (id) => document.getElementById(id);
const STORAGE_KEY = "oav-demo-state-v1";

const REASONS = { 400: "Bad Request", 404: "Not Found", 405: "Method Not Allowed", 415: "Unsupported Media Type" };
const METHODS = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];

let spec = null;       // compiled Spec, or null while the document is invalid
let specDoc = null;    // parsed JSON, for the operation picker and samples
let parts = [];        // [{ name, default }] from the engine

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

async function main() {
  try {
    await init();
  } catch (e) {
    setPill("bad", "Engine failed to load");
    $("spec-status").textContent = `Could not load the WebAssembly module: ${e}`;
    return;
  }
  setPill("ok", "Engine ready");

  parts = JSON.parse(partNames());
  renderPartCheckboxes();

  const saved = loadState();
  if (saved) {
    applyState(saved);
  } else {
    $("spec").value = await fetchExampleSpec();
    setRequest({ method: "GET", uri: "/api/pets?status=available&limit=10", headers: "", body: "" });
  }

  bindEvents();
  compileSpec();
}

function bindEvents() {
  $("spec").addEventListener("input", debounce(compileSpec, 300));
  $("spec-reset").addEventListener("click", async () => {
    $("spec").value = await fetchExampleSpec();
    compileSpec();
  });
  $("spec-file").addEventListener("change", async (e) => {
    const file = e.target.files[0];
    if (!file) return;
    $("spec").value = await file.text();
    e.target.value = "";
    compileSpec();
  });

  for (const el of document.querySelectorAll("input[name=enabled], input[name=mode]")) {
    el.addEventListener("change", update);
  }
  $("parts").addEventListener("change", update);
  $("parts-default").addEventListener("click", () => {
    for (const p of parts) partBox(p.name).checked = p.default;
    update();
  });
  $("copy-conf").addEventListener("click", copyConf);

  const onRequestInput = debounce(update, 120);
  for (const id of ["method", "uri", "headers", "body"]) {
    $(id).addEventListener("input", onRequestInput);
  }
  $("operation").addEventListener("change", (e) => {
    if (e.target.value) fillFromOperation(e.target.value);
    e.target.value = "";
  });
}

async function fetchExampleSpec() {
  const res = await fetch("pkg/api-spec.json");
  return res.text();
}

// ---------------------------------------------------------------------------
// Spec
// ---------------------------------------------------------------------------

function compileSpec() {
  const text = $("spec").value;
  const status = $("spec-status");

  if (spec) {
    spec.free();
    spec = null;
  }
  try {
    specDoc = JSON.parse(text);
  } catch {
    specDoc = null;
  }

  try {
    spec = new Spec(text);
    const ops = listOperations(specDoc);
    status.className = "status ok";
    status.textContent = `Compiled: ${ops.length} operation${ops.length === 1 ? "" : "s"}, OpenAPI ${specDoc.openapi}`;
  } catch (e) {
    status.className = "status bad";
    // Same message nginx prints when `nginx -t` rejects the file.
    status.textContent = `openapi_spec: failed to load: ${errorText(e)}`;
  }

  renderOperationPicker();
  update();
}

function listOperations(doc) {
  const ops = [];
  if (!doc || typeof doc.paths !== "object" || !doc.paths) return ops;
  for (const [path, item] of Object.entries(doc.paths)) {
    if (!item || typeof item !== "object") continue;
    for (const method of METHODS) {
      if (item[method]) ops.push({ path, method, item, op: item[method] });
    }
  }
  return ops;
}

function renderOperationPicker() {
  const select = $("operation");
  select.length = 1;
  for (const [i, { path, method, op }] of listOperations(specDoc).entries()) {
    const label = `${method.toUpperCase()} ${path}${op.summary ? ` — ${op.summary}` : ""}`;
    select.add(new Option(label, String(i)));
  }
}

// ---------------------------------------------------------------------------
// Directives
// ---------------------------------------------------------------------------

function renderPartCheckboxes() {
  const box = $("parts");
  box.replaceChildren();
  for (const p of parts) {
    const label = document.createElement("label");
    const input = document.createElement("input");
    input.type = "checkbox";
    input.value = p.name;
    input.checked = p.default;
    const tag = document.createElement("span");
    tag.className = "tag";
    tag.textContent = p.default ? "default" : "opt-in";
    label.append(input, document.createTextNode(p.name), tag);
    box.append(label);
  }
}

const partBox = (name) => $("parts").querySelector(`input[value="${name}"]`);

// The shortest `openapi_validate_parts` value for the ticked boxes.
function partsDirective() {
  const on = parts.filter((p) => partBox(p.name).checked).map((p) => p.name);
  const defaults = parts.filter((p) => p.default).map((p) => p.name);
  if (defaults.every((d) => on.includes(d))) {
    return ["all", ...on.filter((n) => !defaults.includes(n))].join(",");
  }
  return on.join(",");
}

function directives() {
  return {
    openapi_validate: document.querySelector("input[name=enabled]:checked").value,
    openapi_validate_mode: document.querySelector("input[name=mode]:checked").value,
    openapi_validate_parts: partsDirective(),
  };
}

function renderConf(d) {
  const partsArg = d.openapi_validate_parts === "" ? '""' : d.openapi_validate_parts;
  $("conf").textContent = [
    "location /api/ {",
    "    openapi_spec /etc/nginx/api-spec.json;",
    `    openapi_validate ${d.openapi_validate};`,
    `    openapi_validate_mode ${d.openapi_validate_mode};`,
    `    openapi_validate_parts ${partsArg};`,
    "",
    "    proxy_pass http://backend;",
    "}",
  ].join("\n");
}

async function copyConf() {
  const btn = $("copy-conf");
  try {
    await navigator.clipboard.writeText($("conf").textContent);
    btn.textContent = "Copied";
  } catch {
    btn.textContent = "Copy failed";
  }
  setTimeout(() => (btn.textContent = "Copy"), 1200);
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

function parseHeaders(text) {
  const headers = [];
  for (const line of text.split("\n")) {
    const i = line.indexOf(":");
    if (i <= 0) continue;
    headers.push([line.slice(0, i).trim(), line.slice(i + 1).trim()]);
  }
  return headers;
}

function currentRequest() {
  return {
    method: $("method").value,
    uri: $("uri").value.trim() || "/",
    headers: parseHeaders($("headers").value),
    body: $("body").value,
  };
}

function setRequest({ method, uri, headers, body }) {
  $("method").value = method;
  $("uri").value = uri;
  $("headers").value = headers;
  $("body").value = body;
}

// ---------------------------------------------------------------------------
// Run and render
// ---------------------------------------------------------------------------

function update() {
  const d = directives();
  renderConf(d);
  saveState();

  const out = $("result");
  if (!spec) {
    out.replaceChildren(verdict("neutral", "—", "No spec loaded", "nginx would refuse to start: fix the spec first."));
    return;
  }

  let result;
  try {
    result = JSON.parse(spec.validate(JSON.stringify(d), JSON.stringify(currentRequest())));
  } catch (e) {
    out.replaceChildren(verdict("bad", "!", "Configuration error", errorText(e)));
    return;
  }
  renderResult(result);
}

function renderResult(r) {
  const nodes = [];

  switch (r.outcome) {
    case "skipped":
      nodes.push(verdict("neutral", "—", "Not validated", "openapi_validate is off, so the request continues to the upstream unchecked."));
      break;
    case "rejected_by_nginx":
      nodes.push(verdict("bad", "400", "Rejected by nginx", `${r.reason}; the module never runs.`));
      break;
    case "passed":
      nodes.push(verdict("ok", "✓", "Valid", "The request continues to the upstream."));
      break;
    case "blocked":
      nodes.push(verdict("bad", String(r.http_status), REASONS[r.http_status] ?? "Rejected", "Blocked: nginx returns this status and the upstream never sees the request."));
      break;
    case "logged":
      nodes.push(verdict("warn", "✓", "Invalid, passed through", "Audit mode logs the errors to error.log and lets the request continue."));
      break;
  }

  if (r.errors.length) {
    nodes.push(heading(`Validation errors (${r.errors.length})`));
    const list = document.createElement("ul");
    list.className = "errors";
    for (const e of r.errors) {
      const li = document.createElement("li");
      li.append(
        span("path", e.path), document.createTextNode(" "), span("kind", e.kind),
        document.createElement("br"), document.createTextNode(e.message),
      );
      list.append(li);
    }
    nodes.push(list);
  }

  if (r.error_log.length) {
    nodes.push(heading("error.log"));
    const pre = document.createElement("pre");
    pre.className = "code block";
    pre.textContent = r.error_log.map((l) => `[error] ${l}`).join("\n");
    nodes.push(pre);
  }

  nodes.push(heading("Variables"));
  const table = document.createElement("table");
  table.className = "vars";
  const rows = [];
  if (r.uri !== undefined) rows.push(["$uri", r.uri], ["$args", r.args ?? ""]);
  for (const k of ["oav_status", "oav_error_count", "oav_first_error", "oav_errors_json"]) {
    rows.push([`$${k}`, r.variables[k]]);
  }
  for (const [k, v] of rows) {
    const tr = table.insertRow();
    tr.insertCell().textContent = k;
    tr.insertCell().textContent = v === "" ? '""' : v;
  }
  nodes.push(table);

  $("result").replaceChildren(...nodes);
}

function verdict(tone, code, what, why) {
  const box = document.createElement("div");
  box.className = `verdict ${tone}`;
  const text = document.createElement("div");
  text.append(span("what", what), document.createElement("br"), span("why", why));
  box.append(span("code-num", code), text);
  return box;
}

function heading(text) {
  const h = document.createElement("h3");
  h.textContent = text;
  return h;
}

function span(cls, text) {
  const s = document.createElement("span");
  s.className = cls;
  s.textContent = text;
  return s;
}

// ---------------------------------------------------------------------------
// Sample requests generated from the spec
// ---------------------------------------------------------------------------

function fillFromOperation(index) {
  const { path, method, item, op } = listOperations(specDoc)[Number(index)];
  const params = [...(item.parameters ?? []), ...(op.parameters ?? [])].map(deref);

  let uri = path;
  const query = [];
  const headers = [];
  const cookies = [];
  for (const p of params) {
    if (!p || !p.name) continue;
    const value = paramSample(p);
    if (p.in === "path") uri = uri.replace(`{${p.name}}`, encodeURIComponent(value));
    else if (!p.required) continue;
    else if (p.in === "query") query.push(`${encodeURIComponent(p.name)}=${encodeURIComponent(value)}`);
    else if (p.in === "header") headers.push(`${p.name}: ${value}`);
    else if (p.in === "cookie") cookies.push(`${p.name}=${value}`);
  }
  if (cookies.length) headers.push(`Cookie: ${cookies.join("; ")}`);

  let body = "";
  const content = deref(op.requestBody)?.content;
  if (content) {
    const [mediaType, media] = Object.entries(content)[0] ?? [];
    if (mediaType) {
      headers.push(`Content-Type: ${mediaType}`);
      body = bodySample(mediaType, media);
    }
  }

  setRequest({
    method: method.toUpperCase(),
    uri: query.length ? `${uri}?${query.join("&")}` : uri,
    headers: headers.join("\n"),
    body,
  });
  update();
}

function paramSample(p) {
  if (p.example !== undefined) return scalar(p.example);
  const ex = p.examples && Object.values(p.examples)[0];
  if (ex && deref(ex).value !== undefined) return scalar(deref(ex).value);
  const value = sample(p.schema, 0);
  return Array.isArray(value) ? value.map(scalar).join(",") : scalar(value);
}

function bodySample(mediaType, media) {
  let value = media?.example;
  if (value === undefined && media?.examples) value = deref(Object.values(media.examples)[0])?.value;
  if (value === undefined) value = sample(media?.schema, 0);
  if (/json/.test(mediaType)) return JSON.stringify(value, null, 2);
  if (/x-www-form-urlencoded/.test(mediaType) && value && typeof value === "object") {
    return new URLSearchParams(Object.entries(value).map(([k, v]) => [k, scalar(v)])).toString();
  }
  return typeof value === "string" ? value : JSON.stringify(value, null, 2);
}

const scalar = (v) => (typeof v === "object" && v !== null ? JSON.stringify(v) : String(v));

// Follow a local `$ref` (possibly chained).
function deref(node) {
  for (let i = 0; i < 16 && node && typeof node.$ref === "string"; i++) {
    const ref = node.$ref;
    if (!ref.startsWith("#")) return {};
    node = ref.slice(1).split("/").slice(1)
      .map((s) => decodeURIComponent(s).replace(/~1/g, "/").replace(/~0/g, "~"))
      .reduce((n, key) => (n == null ? n : n[key]), specDoc);
  }
  return node;
}

// A value that should satisfy `schema`, good enough to start editing from.
function sample(schema, depth) {
  schema = deref(schema);
  if (!schema || typeof schema !== "object" || depth > 6) return null;
  if (schema.example !== undefined) return schema.example;
  if (Array.isArray(schema.examples) && schema.examples.length) return schema.examples[0];
  if (schema.default !== undefined) return schema.default;
  if (schema.const !== undefined) return schema.const;
  if (Array.isArray(schema.enum) && schema.enum.length) return schema.enum[0];
  if (schema.allOf) {
    return Object.assign({}, ...schema.allOf.map((s) => sample(s, depth + 1)).filter((v) => v && typeof v === "object"));
  }
  for (const key of ["oneOf", "anyOf"]) {
    if (Array.isArray(schema[key]) && schema[key].length) return sample(schema[key][0], depth + 1);
  }

  let type = schema.type;
  if (Array.isArray(type)) type = type.find((t) => t !== "null") ?? "null";
  if (!type) type = schema.properties ? "object" : schema.items ? "array" : "string";

  switch (type) {
    case "object": {
      const props = schema.properties ?? {};
      const keys = schema.required?.length ? schema.required : Object.keys(props);
      return Object.fromEntries(keys.map((k) => [k, sample(props[k] ?? {}, depth + 1)]));
    }
    case "array":
      return [sample(schema.items ?? {}, depth + 1)].slice(0, Math.max(schema.minItems ?? 1, 1));
    case "integer":
      return Math.max(schema.minimum ?? 1, 1);
    case "number":
      return schema.minimum ?? 1.5;
    case "boolean":
      return true;
    case "null":
      return null;
    default:
      return stringSample(schema);
  }
}

function stringSample(schema) {
  switch (schema.format) {
    case "date-time": return "2026-01-01T12:00:00Z";
    case "date": return "2026-01-01";
    case "email": return "user@example.com";
    case "uuid": return "3fa85f64-5717-4562-b3fc-2c963f66afa6";
    case "uri": return "https://example.com";
    case "ipv4": return "192.0.2.1";
  }
  const s = "string";
  return s.padEnd(schema.minLength ?? 0, "x").slice(0, schema.maxLength ?? s.length);
}

// ---------------------------------------------------------------------------
// Persistence (per browser; convenience only)
// ---------------------------------------------------------------------------

function saveState() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({
      spec: $("spec").value,
      directives: directives(),
      request: { method: $("method").value, uri: $("uri").value, headers: $("headers").value, body: $("body").value },
    }));
  } catch { /* storage unavailable */ }
}

function loadState() {
  try {
    return JSON.parse(localStorage.getItem(STORAGE_KEY));
  } catch {
    return null;
  }
}

function applyState(s) {
  $("spec").value = s.spec ?? "";
  if (s.request) setRequest(s.request);
  const d = s.directives ?? {};
  for (const [name, value] of [["enabled", d.openapi_validate], ["mode", d.openapi_validate_mode]]) {
    const el = document.querySelector(`input[name=${name}][value="${value}"]`);
    if (el) el.checked = true;
  }
  if (typeof d.openapi_validate_parts === "string") {
    const listed = d.openapi_validate_parts.split(",");
    for (const p of parts) {
      partBox(p.name).checked = listed.includes(p.name) || (listed.includes("all") && p.default);
    }
  }
}

// ---------------------------------------------------------------------------

function setPill(tone, text) {
  const pill = $("engine-status");
  pill.className = `pill ${tone}`;
  pill.textContent = text;
}

function errorText(e) {
  return e instanceof Error ? e.message : String(e);
}

function debounce(fn, ms) {
  let t;
  return (...args) => {
    clearTimeout(t);
    t = setTimeout(() => fn(...args), ms);
  };
}

main();
