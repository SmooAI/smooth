//! Generate a typed Smoo AI API client from the vendored
//! `openapi.json` (a snapshot pulled from
//! `https://api.smoo.ai/openapi.json`).
//!
//! Uses progenitor (https://github.com/oxidecomputer/progenitor) so
//! the generated code is plain reqwest underneath — no async-trait
//! gymnastics, no runtime reflection. The output lands at
//! `$OUT_DIR/codegen.rs` and is `include!`d from `src/lib.rs` under
//! the `pb` module.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let spec_path = PathBuf::from(&manifest).join("openapi.json");
    println!("cargo:rerun-if-changed={}", spec_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    let raw = fs::read_to_string(&spec_path).unwrap_or_else(|e| panic!("read {}: {e}", spec_path.display()));
    let mut spec: serde_json::Value = serde_json::from_str(&raw).expect("parse openapi.json");
    synthesize_operation_ids(&mut spec);
    drop_multipart_endpoints(&mut spec);
    inject_missing_path_params(&mut spec);
    normalize_responses_to_json_only(&mut spec);
    keep_only_success_responses(&mut spec);
    let spec: openapiv3::OpenAPI = serde_json::from_value(spec).expect("deserialize OpenAPI");

    let mut generator = progenitor::Generator::default();
    let tokens = generator.generate_tokens(&spec).expect("progenitor codegen");
    let file: syn::File = syn::parse2(tokens).expect("parse generated tokens");
    let pretty = prettyplease::unparse(&file);

    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR");
    let out_path = PathBuf::from(out_dir).join("codegen.rs");
    fs::write(&out_path, pretty).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
}

/// Add `operationId` to every path-method that's missing one, derived
/// from `<METHOD>_<path>` (sanitized to snake_case). Progenitor uses
/// operationId to name generated methods and refuses to codegen
/// without it. The smooai backend doesn't set it consistently yet.
fn synthesize_operation_ids(spec: &mut serde_json::Value) {
    let Some(paths) = spec.get_mut("paths").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let methods = ["get", "post", "put", "patch", "delete", "options", "head", "trace"];
    for (path, item) in paths.iter_mut() {
        let Some(item_obj) = item.as_object_mut() else {
            continue;
        };
        for method in methods {
            let Some(op) = item_obj.get_mut(method).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            if op.contains_key("operationId") {
                continue;
            }
            let id = derive_operation_id(method, path);
            op.insert("operationId".into(), serde_json::Value::String(id));
        }
    }
}

/// Remove path-operations whose requestBody uses `multipart/form-data`.
/// Progenitor 0.10 can't codegen those (file uploads); we'll hand-roll
/// them when needed. Currently this removes `POST
/// /organizations/.../upload-icon` and `POST /profile/upload-picture`
/// — both file-upload endpoints, both fine to wrap manually later.
fn drop_multipart_endpoints(spec: &mut serde_json::Value) {
    let Some(paths) = spec.get_mut("paths").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let methods = ["get", "post", "put", "patch", "delete", "options", "head", "trace"];
    let mut to_remove_paths: Vec<String> = Vec::new();
    for (path, item) in paths.iter_mut() {
        let Some(item_obj) = item.as_object_mut() else {
            continue;
        };
        let to_remove: Vec<String> = methods
            .iter()
            .filter(|method| {
                item_obj
                    .get(**method)
                    .and_then(|op| op.get("requestBody"))
                    .and_then(|rb| rb.get("content"))
                    .and_then(|c| c.as_object())
                    .is_some_and(|content| content.contains_key("multipart/form-data"))
            })
            .map(|s| (*s).to_string())
            .collect();
        for m in to_remove {
            item_obj.remove(&m);
        }
        if item_obj.is_empty() || item_obj.keys().all(|k| k == "parameters" || k == "summary" || k == "description") {
            to_remove_paths.push(path.clone());
        }
    }
    for p in to_remove_paths {
        paths.remove(&p);
    }
}

/// Some smooai routes have `{org_id}` in the path template but don't
/// declare the parameter in `parameters` — zod-to-openapi doesn't
/// always emit them. Progenitor refuses to codegen those endpoints.
/// Walk every path-template variable; if missing from both the
/// path-level and op-level `parameters` lists, inject a string param
/// at the op level.
fn inject_missing_path_params(spec: &mut serde_json::Value) {
    let Some(paths) = spec.get_mut("paths").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let methods = ["get", "post", "put", "patch", "delete", "options", "head", "trace"];
    for (path, item) in paths.iter_mut() {
        let template_vars: Vec<String> = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')).map(str::to_string))
            .collect();
        if template_vars.is_empty() {
            continue;
        }
        let item_obj = match item.as_object_mut() {
            Some(o) => o,
            None => continue,
        };
        let path_level_declared: std::collections::HashSet<String> = item_obj
            .get("parameters")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        let obj = p.as_object()?;
                        if obj.get("in").and_then(|i| i.as_str()) == Some("path") {
                            obj.get("name").and_then(|n| n.as_str()).map(str::to_string)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        for method in methods {
            let Some(op) = item_obj.get_mut(method).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            let mut op_declared: std::collections::HashSet<String> = path_level_declared.clone();
            if let Some(arr) = op.get("parameters").and_then(|v| v.as_array()) {
                for p in arr {
                    if let Some(obj) = p.as_object() {
                        if obj.get("in").and_then(|i| i.as_str()) == Some("path") {
                            if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
                                op_declared.insert(name.to_string());
                            }
                        }
                    }
                }
            }
            let missing: Vec<&String> = template_vars.iter().filter(|v| !op_declared.contains(*v)).collect();
            if missing.is_empty() {
                continue;
            }
            let entry = op.entry("parameters").or_insert_with(|| serde_json::Value::Array(Vec::new()));
            let Some(arr) = entry.as_array_mut() else { continue };
            for name in missing {
                arr.push(serde_json::json!({
                    "name": name,
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string" },
                }));
            }
        }
    }
}

/// Strip alternate response content types so each status has at most
/// `application/json`. Progenitor 0.10 asserts one response type per
/// status; zod-to-openapi sometimes emits both `application/json` and
/// `text/plain` (the error-message branch). We always prefer the JSON
/// branch.
fn normalize_responses_to_json_only(spec: &mut serde_json::Value) {
    let Some(paths) = spec.get_mut("paths").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let methods = ["get", "post", "put", "patch", "delete", "options", "head", "trace"];
    for (_, item) in paths.iter_mut() {
        let Some(item_obj) = item.as_object_mut() else {
            continue;
        };
        for method in methods {
            let Some(op) = item_obj.get_mut(method).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            let Some(responses) = op.get_mut("responses").and_then(|v| v.as_object_mut()) else {
                continue;
            };
            for (_status, resp) in responses.iter_mut() {
                let Some(content) = resp.get_mut("content").and_then(|v| v.as_object_mut()) else {
                    continue;
                };
                if content.contains_key("application/json") {
                    content.retain(|k, _| k == "application/json");
                } else if content.len() > 1 {
                    // No JSON branch — keep the first content type only.
                    let first = content.keys().next().cloned();
                    if let Some(first) = first {
                        content.retain(|k, _| k == &first);
                    }
                }
            }
        }
    }
}

/// Drop non-2xx responses from each operation. Progenitor 0.10
/// asserts `response_types.len() <= 1` across ALL responses for a
/// given op, treating different schemas as distinct types — so a
/// route that returns `Agent` on 200 and `ErrorResponse` on 400 trips
/// the assertion. The generated client surface is reqwest-error
/// anyway; consumers handle 4xx/5xx via the returned status from
/// `Result<ResponseValue<T>, Error<E>>`. We don't lose anything by
/// pruning the spec's error-schema definitions for codegen purposes —
/// they're still in `crate::pb::types` as schemas.
fn keep_only_success_responses(spec: &mut serde_json::Value) {
    let Some(paths) = spec.get_mut("paths").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let methods = ["get", "post", "put", "patch", "delete", "options", "head", "trace"];
    for (_, item) in paths.iter_mut() {
        let Some(item_obj) = item.as_object_mut() else {
            continue;
        };
        for method in methods {
            let Some(op) = item_obj.get_mut(method).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            let Some(responses) = op.get_mut("responses").and_then(|v| v.as_object_mut()) else {
                continue;
            };
            responses.retain(|k, _| k.starts_with('2') || k == "default");
        }
    }
}

fn derive_operation_id(method: &str, path: &str) -> String {
    // `GET /organizations/{org_id}/agents/{id}` -> `get_organizations_org_id_agents_id`
    let mut out = String::with_capacity(method.len() + path.len() + 4);
    out.push_str(method);
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        out.push('_');
        for c in segment.chars() {
            if c.is_alphanumeric() {
                out.extend(c.to_lowercase());
            } else if c == '_' {
                out.push('_');
            }
            // skip braces, dots, etc.
        }
    }
    out
}
