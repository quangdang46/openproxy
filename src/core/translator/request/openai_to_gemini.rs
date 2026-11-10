//! OpenAI to Gemini request translator
//!
//! Converts OpenAI Chat Completions format to Gemini API format.

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::core::config::app_constants::ANTIGRAVITY_DEFAULT_SYSTEM;

/// Sanitize function names for Gemini API.
/// Gemini requires: starts with [a-zA-Z_], followed by [a-zA-Z0-9_.:\-], max 64 chars.
fn sanitize_gemini_function_name(name: &str) -> String {
    if name.is_empty() {
        return "_unknown".to_string();
    }
    let mut sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !sanitized
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        sanitized.insert(0, '_');
    }
    sanitized.truncate(64);
    sanitized
}

/// Try to parse JSON, return default on failure.
fn try_parse_json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::String(s.to_string()))
}

/// Extract text content from OpenAI content (string or array).
fn extract_text_content(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// Concrete MIME for a remote image URL (9router v0.5.116 `7ba1d1ca`,
/// `remoteImageMimeType` in `open-sse/translator/formats/gemini.js`).
/// Gemini/Antigravity rejects the `image/*` wildcard
/// (`Unsupported mime type: image/* in contents.file_data`), so infer from
/// the URL path extension and fall back to `image/png` (`DEFAULT_IMAGE_MIME`).
/// The value comes from a fixed set — a caller-supplied URL cannot inject
/// anything into the emitted MIME string.
fn remote_image_mime_type(url: &str) -> &'static str {
    // Strip query/fragment, then take the path extension.
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else if path.ends_with(".webp") {
        "image/webp"
    } else if path.ends_with(".gif") {
        "image/gif"
    } else if path.ends_with(".bmp") {
        "image/bmp"
    } else {
        "image/png"
    }
}

/// Convert OpenAI content parts to Gemini parts.
fn convert_openai_content_to_parts(content: &Value) -> Vec<Value> {
    if let Some(s) = content.as_str() {
        if !s.is_empty() {
            return vec![serde_json::json!({"text": s})];
        }
        return vec![];
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for part in arr {
            let t = part.get("type").and_then(|v| v.as_str());
            match t {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        parts.push(serde_json::json!({"text": text}));
                    }
                }
                Some("image_url") => {
                    if let Some(url_obj) = part.get("image_url") {
                        if let Some(url) = url_obj.get("url").and_then(|v| v.as_str()) {
                            if let Some(data_uri) = url.strip_prefix("data:") {
                                if let Some((mime, base64_data)) = data_uri.split_once(";base64,") {
                                    parts.push(serde_json::json!({
                                        "inlineData": {
                                            "mimeType": mime,
                                            "data": base64_data
                                        }
                                    }));
                                }
                            } else if url.starts_with("http://") || url.starts_with("https://") {
                                // Remote image: prefetch normally inlines it,
                                // but when it cannot, emit fileData with a
                                // concrete MIME — `image/*` is rejected.
                                parts.push(serde_json::json!({
                                    "fileData": {
                                        "fileUri": url,
                                        "mimeType": remote_image_mime_type(url)
                                    }
                                }));
                            }
                        }
                    }
                }
                Some("image") => {
                    if let Some(source) = part.get("source") {
                        if source.get("type").and_then(|v| v.as_str()) == Some("base64") {
                            if let (Some(media_type), Some(data)) = (
                                source.get("media_type").and_then(|v| v.as_str()),
                                source.get("data").and_then(|v| v.as_str()),
                            ) {
                                parts.push(serde_json::json!({
                                    "inlineData": {
                                        "mimeType": media_type,
                                        "data": data
                                    }
                                }));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        return parts;
    }
    vec![]
}

/// Unsupported JSON Schema constraints that should be removed for Antigravity.
/// Mirrors UNSUPPORTED_SCHEMA_CONSTRAINTS in open-sse/translator/formats/gemini.js.
const UNSUPPORTED_SCHEMA_CONSTRAINTS: &[&str] = &[
    // Basic constraints (not supported by Gemini API)
    "minLength",
    "maxLength",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minItems",
    "maxItems",
    "format",
    "multipleOf",
    "uniqueItems",
    "contains",
    // Claude rejects these in VALIDATED mode
    "default",
    "examples",
    // JSON Schema meta keywords
    "$schema",
    "$defs",
    "definitions",
    "const",
    "$ref",
    "$comment",
    // Annotation keywords (rejected by Gemini/Antigravity)
    "deprecated",
    "readOnly",
    "writeOnly",
    // Object validation keywords (not supported)
    "additionalProperties",
    "additionalItems",
    "propertyNames",
    "patternProperties",
    "enumDescriptions",
    // Complex schema keywords (handled by flattenAnyOfOneOf/mergeAllOf)
    "anyOf",
    "oneOf",
    "allOf",
    "not",
    // Unevaluated keywords (not supported by Gemini)
    "unevaluatedProperties",
    "unevaluatedItems",
    // Dependency keywords (not supported)
    "dependencies",
    "dependentSchemas",
    "dependentRequired",
    // Other unsupported keywords
    "title",
    "optional",
    "deprecated",
    "if",
    "then",
    "else",
    "contentMediaType",
    "contentEncoding",
    "contentSchema",
    // Tuple validation (handled by convertPrefixItems before this phase)
    "prefixItems",
    // UI/Styling properties (from Cursor tools - NOT JSON Schema standard)
    "cornerRadius",
    "fillColor",
    "fontFamily",
    "fontSize",
    "fontWeight",
    "gap",
    "padding",
    "strokeColor",
    "strokeThickness",
    "textColor",
    // Non-standard annotation/error keywords used by some MCP tool schemas
    // (#4283). Gemini's schema proto has no field for these and rejects the
    // whole request with "Unknown name X: Cannot find field". The `x-`
    // variants are listed for parity with upstream — `remove_unsupported_keywords`
    // also strips any `x-` prefixed key dynamically.
    "errorMessage",
    "errorMessages",
    "x-errorMessage",
    "x-errorMessages",
    "markdownDescription",
    "x-intellij-html-description",
    "x-taplo-info",
    "x-taplo",
    "doNotSuggest",
    "suggestSortText",
    "minProperties",
    "maxProperties",
];

/// Recursively remove unsupported keywords from a JSON schema (mutates in place).
/// Mirrors removeUnsupportedKeywords in open-sse/translator/formats/gemini.js.
fn remove_unsupported_keywords(obj: &mut Value, keywords: &[&str]) {
    match obj {
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                remove_unsupported_keywords(item, keywords);
            }
        }
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if keywords.contains(&key.as_str()) || key.starts_with("x-") {
                    map.remove(&key);
                    continue;
                }
                if let Some(val) = map.get_mut(&key) {
                    if val.is_object() || val.is_array() {
                        remove_unsupported_keywords(val, keywords);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Convert const to enum.
/// Mirrors convertConstToEnum in open-sse/translator/formats/gemini.js.
fn convert_const_to_enum(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            if map.contains_key("const") && !map.contains_key("enum") {
                if let Some(c) = map.remove("const") {
                    map.insert("enum".to_string(), Value::Array(vec![c]));
                }
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    convert_const_to_enum(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    convert_const_to_enum(item);
                }
            }
        }
        _ => {}
    }
}

/// Convert enum values to strings (Gemini requires string enum values).
/// Mirrors convertEnumValuesToStrings in open-sse/translator/formats/gemini.js.
fn convert_enum_values_to_strings(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            if let Some(enum_arr) = map.get_mut("enum") {
                if let Some(arr) = enum_arr.as_array_mut() {
                    for v in arr.iter_mut() {
                        *v = Value::String(v.to_string().replace('\"', ""));
                    }
                    // Gemini API requires type:"string" when enum is present
                    if !map.contains_key("type") {
                        map.insert("type".to_string(), Value::String("string".to_string()));
                    }
                }
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    convert_enum_values_to_strings(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    convert_enum_values_to_strings(item);
                }
            }
        }
        _ => {}
    }
}

/// Merge allOf schemas.
/// Mirrors mergeAllOf in open-sse/translator/formats/gemini.js.
fn merge_allof(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            if let Some(all_of) = map.remove("allOf").and_then(|v| v.as_array().cloned()) {
                let mut merged_props = serde_json::Map::new();
                let mut merged_required: Vec<String> = Vec::new();

                for item in all_of {
                    if let Some(props) = item.get("properties").and_then(|v| v.as_object()) {
                        for (k, v) in props {
                            merged_props.insert(k.clone(), v.clone());
                        }
                    }
                    if let Some(req) = item.get("required").and_then(|v| v.as_array()) {
                        for r in req {
                            if let Some(s) = r.as_str() {
                                if !merged_required.contains(&s.to_string()) {
                                    merged_required.push(s.to_string());
                                }
                            }
                        }
                    }
                }

                if !merged_props.is_empty() {
                    if let Some(existing_props) = map.get_mut("properties") {
                        if let Some(existing) = existing_props.as_object_mut() {
                            for (k, v) in merged_props {
                                existing.insert(k, v);
                            }
                        }
                    } else {
                        map.insert("properties".to_string(), Value::Object(merged_props));
                    }
                }
                if !merged_required.is_empty() {
                    if let Some(existing_req) =
                        map.get_mut("required").and_then(|v| v.as_array_mut())
                    {
                        for r in merged_required {
                            if !existing_req.iter().any(|v| v.as_str() == Some(&r)) {
                                existing_req.push(Value::String(r));
                            }
                        }
                    } else {
                        map.insert(
                            "required".to_string(),
                            Value::Array(merged_required.into_iter().map(Value::String).collect()),
                        );
                    }
                }
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    merge_allof(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    merge_allof(item);
                }
            }
        }
        _ => {}
    }
}

/// Select best schema from anyOf/oneOf items.
fn select_best(items: &[Value]) -> usize {
    let mut best_idx = 0usize;
    let mut best_score = -1i32;

    for (i, item) in items.iter().enumerate() {
        let mut score = 0;
        let typ = item.get("type").and_then(Value::as_str);
        let has_props = item.get("properties").is_some();

        if typ == Some("object") || has_props {
            score = 3;
        } else if typ == Some("array") || item.get("items").is_some() {
            score = 2;
        } else if let Some(t) = typ {
            if t != "null" {
                score = 1;
            }
        }

        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }

    best_idx
}

/// Flatten anyOf/oneOf.
/// Mirrors flattenAnyOfOneOf in open-sse/translator/formats/gemini.js.
fn flatten_anyof_oneof(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            for key in ["anyOf", "oneOf"] {
                if let Some(arr) = map.get(key).and_then(|v| v.as_array()) {
                    let non_null: Vec<&Value> = arr
                        .iter()
                        .filter(|s| s.get("type").and_then(Value::as_str) != Some("null"))
                        .collect();
                    if !non_null.is_empty() {
                        let best_idx =
                            select_best(&non_null.iter().copied().cloned().collect::<Vec<_>>());
                        let selected = non_null[best_idx].clone();
                        map.remove(key);
                        // Merge selected into self
                        if let Some(sel_obj) = selected.as_object() {
                            for (sk, sv) in sel_obj {
                                map.insert(sk.clone(), sv.clone());
                            }
                        }
                    }
                }
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    flatten_anyof_oneof(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    flatten_anyof_oneof(item);
                }
            }
        }
        _ => {}
    }
}

/// Flatten type arrays.
/// Mirrors flattenTypeArrays in open-sse/translator/formats/gemini.js.
fn flatten_type_arrays(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            if let Some(type_arr) = map.get("type").and_then(|v| v.as_array()) {
                let non_null: Vec<&Value> = type_arr
                    .iter()
                    .filter(|t| t.as_str() != Some("null"))
                    .collect();
                // 9router 8db1f4ae: multiple non-null types pick the best fit —
                // structural clues (properties → object, non-empty items with a
                // type → array) then string > number > integer > boolean > first.
                let new_type = if non_null.is_empty() {
                    Value::String("string".to_string())
                } else if non_null.len() == 1 {
                    non_null[0].clone()
                } else {
                    let contains = |t: &str| non_null.iter().any(|v| v.as_str() == Some(t));
                    let has_properties = map.get("properties").map(js_truthy).unwrap_or(false);
                    let items_nonempty = map.get("items").is_some_and(|i| match i {
                        Value::Object(o) => !o.is_empty(),
                        Value::Array(a) => !a.is_empty(),
                        _ => false,
                    });
                    let items_has_type = map
                        .get("items")
                        .and_then(|i| i.get("type"))
                        .map(js_truthy)
                        .unwrap_or(false);
                    let picked = if has_properties && contains("object") {
                        "object"
                    } else if items_nonempty && items_has_type && contains("array") {
                        "array"
                    } else if contains("string") {
                        "string"
                    } else if contains("number") {
                        "number"
                    } else if contains("integer") {
                        "integer"
                    } else if contains("boolean") {
                        "boolean"
                    } else {
                        non_null[0].as_str().unwrap_or("string")
                    };
                    Value::String(picked.to_string())
                };
                map.insert("type".to_string(), new_type);
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    flatten_type_arrays(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    flatten_type_arrays(item);
                }
            }
        }
        _ => {}
    }
}

/// Infer missing type=object when properties exist (Gemini requires explicit type).
/// Mirrors ensureObjectType in open-sse/translator/formats/gemini.js.
///
/// The `properties` value is a *map of parameter name → schema*, not a schema
/// node itself. Recursing into it as a node would treat a parameter literally
/// named `properties` as a nested schema and corrupt the tool definition
/// (upstream #4532-family fix). Instead, recurse into each *value* of the
/// properties map.
fn ensure_object_type(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            if map.contains_key("properties") && !map.contains_key("type") {
                map.insert("type".to_string(), Value::String("object".to_string()));
            }
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let Some(val) = map.get_mut(&key) else {
                    continue;
                };
                if key == "properties" {
                    if let Value::Object(props) = val {
                        for sub in props.values_mut() {
                            ensure_object_type(sub);
                        }
                    } else {
                        ensure_object_type(val);
                    }
                } else {
                    ensure_object_type(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    ensure_object_type(item);
                }
            }
        }
        _ => {}
    }
}

/// Clean required fields recursively — remove required fields that don't exist in properties.
fn cleanup_required(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            // Check if properties exists and is an object - extract keys first to avoid borrow conflict
            let prop_keys: Option<Vec<String>> = map
                .get("properties")
                .and_then(|v| v.as_object())
                .map(|props| props.keys().cloned().collect());

            if let Some(keys) = prop_keys {
                if let Some(req) = map.get_mut("required").and_then(|v| v.as_array_mut()) {
                    req.retain(|field| {
                        field
                            .as_str()
                            .map(|f| keys.contains(&f.to_string()))
                            .unwrap_or(false)
                    });
                    if req.is_empty() {
                        map.remove("required");
                    }
                }
            } else {
                map.remove("required");
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    cleanup_required(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    cleanup_required(item);
                }
            }
        }
        _ => {}
    }
}

/// Add placeholder for empty object schemas (Antigravity requirement).
/// Mirrors addPlaceholders in open-sse/translator/formats/gemini.js.
fn add_placeholders(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            // Empty schema {} (no type, no properties) after $ref removal — treat as
            // object with placeholder. Mirrors the v0.5.45 upstream guard.
            if map.is_empty() {
                map.insert("type".to_string(), Value::String("object".to_string()));
                map.insert(
                    "properties".to_string(),
                    serde_json::json!({
                        "reason": {
                            "type": "string",
                            "description": "Brief explanation of why you are calling this tool"
                        }
                    }),
                );
                map.insert("required".to_string(), serde_json::json!(["reason"]));
                return;
            }
            if map.get("type").and_then(Value::as_str) == Some("object") {
                let props_empty = match map.get("properties") {
                    Some(Value::Object(p)) => p.is_empty(),
                    None => true,
                    _ => false,
                };
                if props_empty {
                    map.insert(
                        "properties".to_string(),
                        serde_json::json!({
                            "reason": {
                                "type": "string",
                                "description": "Brief explanation of why you are calling this tool"
                            }
                        }),
                    );
                    map.insert("required".to_string(), serde_json::json!(["reason"]));
                }
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
                    add_placeholders(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                if item.is_object() || item.is_array() {
                    add_placeholders(item);
                }
            }
        }
        _ => {}
    }
}

/// Convert `prefixItems` (tuple validation) to `items` — Gemini cannot express tuples,
/// and a `type:"array"` schema without `items` is rejected with "missing field".
/// Mirrors `convertPrefixItems` in `open-sse/translator/formats/gemini.js:315-333`.
fn convert_prefix_items(obj: &mut Value) {
    if let Value::Object(map) = obj {
        // Convert prefixItems → items (only if items is not already set)
        if let Some(Value::Array(prefix_arr)) = map.get("prefixItems").cloned() {
            if !prefix_arr.is_empty() {
                let has_items = map.get("items").is_some();
                if !has_items {
                    let variants: Vec<Value> = prefix_arr
                        .into_iter()
                        .filter(|s| {
                            s.get("type")
                                .and_then(Value::as_str)
                                .map(|t| t != "null")
                                .unwrap_or(true)
                        })
                        .collect();
                    if variants.len() == 1 {
                        map.insert("items".to_string(), variants.into_iter().next().unwrap());
                    } else if variants.len() > 1 {
                        map.insert("items".to_string(), json!({"anyOf": variants}));
                    }
                }
                map.remove("prefixItems");
            }
        }

        // Recurse into child values
        let keys: Vec<String> = map.keys().cloned().collect();
        for key in keys {
            if let Some(val) = map.get_mut(&key) {
                convert_prefix_items(val);
            }
        }
    } else if let Value::Array(arr) = obj {
        for item in arr.iter_mut() {
            convert_prefix_items(item);
        }
    }
}

/// Gemini requires `items` on every `type:"array"` schema — fill a permissive placeholder.
/// Also infers `items.type` when the items schema lacks one. Mirrors `ensureArrayItems`
/// in `open-sse/translator/formats/gemini.js`.
fn ensure_array_items(obj: &mut Value) {
    if let Value::Object(map) = obj {
        if map.get("type").and_then(Value::as_str) == Some("array") {
            let needs_default = match map.get("items") {
                Some(Value::Object(items)) => items.is_empty(),
                Some(_) => true,
                None => true,
            };
            if needs_default {
                map.insert("items".to_string(), json!({"type": "string"}));
            } else {
                ensure_items_type(map);
            }
        }
        let keys: Vec<String> = map.keys().cloned().collect();
        for key in keys {
            if let Some(val) = map.get_mut(&key) {
                ensure_array_items(val);
            }
        }
    } else if let Value::Array(arr) = obj {
        for item in arr.iter_mut() {
            ensure_array_items(item);
        }
    }
}

/// JS truthiness for schema keyword values (`null`/`false`/`0`/`""` are falsy;
/// arrays and objects — even empty — are truthy).
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Upstream assigns `obj.items.type = <inferred>` — it mutates the `type` field
/// in place, preserving sibling fields (`description`, `items`, …) on `items`.
/// Mirrors the `else if (!obj.items.type)` branch of `ensureArrayItems` /
/// `enforceTypeConstraints` in `open-sse/translator/formats/gemini.js`.
fn ensure_items_type(map: &mut serde_json::Map<String, Value>) {
    let inferred = {
        let Some(Value::Object(items)) = map.get("items") else {
            return;
        };
        if has_truthy_type_from(items) {
            return;
        }
        if items.get("properties").map(js_truthy).unwrap_or(false) {
            "object"
        } else if items.get("items").map(js_truthy).unwrap_or(false) {
            "array"
        } else {
            "string"
        }
    };
    if let Some(Value::Object(items)) = map.get_mut("items") {
        items.insert("type".to_string(), json!(inferred));
    }
}

/// `items` is itself a schema object — same truthy-`type` check.
fn has_truthy_type_from(items: &serde_json::Map<String, Value>) -> bool {
    items.get("type").map(js_truthy).unwrap_or(false)
}

/// Phase 6: Enforce Gemini protobuf type predicates — `items` only on `type:"array"`,
/// `properties`/`required` only on `type:"object"`. Post-order recursion mirrors
/// `enforceTypeConstraints` in `open-sse/translator/formats/gemini.js`.
fn enforce_type_constraints(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            // Recurse into children first (post-order), like upstream.
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(val) = map.get_mut(&key) {
                    enforce_type_constraints(val);
                }
            }

            let ty = map.get("type").and_then(Value::as_str).map(str::to_string);

            if ty.as_deref() == Some("array") {
                map.remove("properties");
                map.remove("required");
                let needs_default = match map.get("items") {
                    Some(Value::Object(items)) => items.is_empty(),
                    Some(_) => true,
                    None => true,
                };
                if needs_default {
                    map.insert("items".to_string(), json!({"type": "string"}));
                } else {
                    ensure_items_type(map);
                }
            } else {
                // Non-array schemas MUST NOT carry items.
                map.remove("items");
                if ty.as_deref() != Some("object") {
                    map.remove("properties");
                    map.remove("required");
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                enforce_type_constraints(item);
            }
        }
        _ => {}
    }
}

/// Clean JSON Schema for Antigravity API compatibility - removes unsupported keywords recursively.
/// Mirrors cleanJSONSchemaForAntigravity in open-sse/translator/formats/gemini.js.
pub fn clean_json_schema_for_antigravity(schema: &mut Value) {
    if !schema.is_object() {
        return;
    }

    // Phase 1: Convert and prepare
    convert_const_to_enum(schema);
    convert_enum_values_to_strings(schema);

    // Phase 2: Flatten complex structures
    merge_allof(schema);
    convert_prefix_items(schema);
    flatten_anyof_oneof(schema);
    flatten_type_arrays(schema);

    // Phase 2.5: Infer missing type=object when properties exist (Gemini requirement)
    ensure_object_type(schema);
    ensure_array_items(schema);

    // Phase 3: Remove all unsupported keywords at ALL levels
    remove_unsupported_keywords(schema, UNSUPPORTED_SCHEMA_CONSTRAINTS);

    // Phase 4: Cleanup required fields recursively
    cleanup_required(schema);

    // Phase 5: Add placeholder for empty object schemas (Antigravity requirement)
    add_placeholders(schema);

    // Phase 6: Enforce Gemini protobuf type predicates
    // (items only on array, properties/required only on object)
    enforce_type_constraints(schema);
}

/// Clean JSON Schema for Gemini/Antigravity compatibility (legacy function, now delegates to clean_json_schema_for_antigravity).
/// Made `pub(crate)` so the Antigravity executor can use it for tool schema cleaning.
pub(crate) fn clean_json_schema(schema: &Value) -> Value {
    let mut result = schema.clone();
    clean_json_schema_for_antigravity(&mut result);
    result
}

/// Gemini 4 (Argon) drops sampling params and requires functionResponse-last
/// merged user turns. Mirrors `isGemini4Model` in
/// `open-sse/providers/capabilities.js` — regex `/(?:^|\/)gemini-4(?:$|[-.])/i`.
pub(crate) fn is_gemini4_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    for segment in m.split('/') {
        if let Some(rest) = segment.strip_prefix("gemini-4") {
            if rest.is_empty() || rest.starts_with('-') || rest.starts_with('.') {
                return true;
            }
        }
    }
    false
}

/// Rewrites duplicate tool_call_ids so every emitted functionCall id is unique.
///
/// Gemini validates functionCall id uniqueness across the WHOLE history and
/// answers 400 INVALID_ARGUMENT otherwise, but an OpenAI tool_call_id is only
/// unique within its own assistant turn — a long agent session can replay the
/// same id in a later turn (#4532).
///
/// Uniqueness is per OCCURRENCE, not per id: two different calls that share an
/// id must end up with different emitted ids, so the mapping is consumed in
/// document order rather than memoized by id. Ids that were already unique are
/// passed through untouched. Mirrors `createToolCallIdUniquifier` in
/// `open-sse/translator/request/openai-to-gemini.js`.
#[derive(Default)]
struct ToolCallIdUniquifier {
    used: std::collections::HashSet<String>,
}

impl ToolCallIdUniquifier {
    fn next(&mut self, id: &str) -> String {
        if id.is_empty() {
            return id.to_string();
        }
        if !self.used.contains(id) {
            self.used.insert(id.to_string());
            return id.to_string();
        }
        let mut n = 2u32;
        loop {
            let candidate = format!("{id}-{n}");
            if !self.used.contains(&candidate) {
                self.used.insert(candidate.clone());
                return candidate;
            }
            n += 1;
        }
    }
}

/// Keys inside a `functionResponse.response` payload that Gemini / Cloud Code
/// interpret as pointers into `functionResponse.parts`. Tool results carrying
/// a JSON Schema / OpenAPI document (webfetch, read, MCP) legitimately contain
/// `$ref` and would 400 with INVALID_ARGUMENT, so the key is renamed before
/// sending. Mirrors `GEMINI_RESERVED_RESPONSE_KEYS` +
/// `sanitizeFunctionResponsePayload` in
/// `open-sse/translator/formats/gemini.js` (#4273 family).
fn sanitize_function_response_payload(value: &Value) -> Value {
    match value {
        Value::Array(arr) => {
            Value::Array(arr.iter().map(sanitize_function_response_payload).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (key, val) in map {
                let renamed = if key == "$ref" { "_ref" } else { key.as_str() };
                out.insert(renamed.to_string(), sanitize_function_response_payload(val));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

/// Return the part with a sanitized `functionResponse.response` (cloned only
/// when the payload actually needed rewriting). Mirrors
/// `sanitizeFunctionResponsePart` in `open-sse/translator/formats/gemini.js`.
fn sanitize_function_response_part(part: &Value) -> Value {
    let response = part
        .get("functionResponse")
        .and_then(|fr| fr.get("response"));
    match response {
        Some(r) if r.is_object() || r.is_array() => {
            let mut p = part.clone();
            if let Some(fr) = p.get_mut("functionResponse") {
                fr["response"] = sanitize_function_response_payload(r);
            }
            p
        }
        _ => part.clone(),
    }
}

/// Gemini 4 rejects a merged user turn whose last part is not a
/// functionResponse (#4802). Stable-partitions `parts` so every
/// `functionResponse` part moves to the end, preserving relative order.
/// Mirrors `moveFunctionResponsesLast` in
/// `open-sse/translator/formats/gemini.js`.
fn move_function_responses_last(parts: &mut Vec<Value>) {
    let has_fr = |p: &Value| {
        p.get("functionResponse")
            .map(|v| v.is_object())
            .unwrap_or(false)
    };
    if !parts.iter().any(has_fr) {
        return;
    }
    let mut responses = Vec::new();
    let mut others = Vec::new();
    for p in parts.drain(..) {
        if has_fr(&p) {
            responses.push(p);
        } else {
            others.push(p);
        }
    }
    others.extend(responses);
    *parts = others;
}

/// Merge adjacent same-role messages, strip empty parts, sanitize
/// functionResponse payloads, and guarantee initial + terminal user turns —
/// answering unresponded `functionCall`s with a synthesized functionResponse
/// turn (#4273 second half, #4802). Mirrors `normalizeGeminiContents` in
/// `open-sse/translator/formats/gemini.js`.
pub(crate) fn normalize_gemini_contents(contents: &[Value], model: &str) -> Vec<Value> {
    let fr_last = is_gemini4_model(model);
    let mut out: Vec<Value> = Vec::new();

    for c in contents {
        let Some(co) = c.as_object() else {
            continue;
        };
        let Some(role) = co.get("role").and_then(Value::as_str) else {
            continue;
        };
        let Some(parts_arr) = co.get("parts").and_then(Value::as_array) else {
            continue;
        };
        let mut parts: Vec<Value> = parts_arr
            .iter()
            .filter(|p| p.as_object().map(|o| !o.is_empty()).unwrap_or(false))
            .map(sanitize_function_response_part)
            .collect();
        if parts.is_empty() {
            continue;
        }

        let merge_into_last = out
            .last()
            .and_then(|l| l.get("role"))
            .and_then(Value::as_str)
            == Some(role);
        if merge_into_last {
            if let Some(last_parts) = out
                .last_mut()
                .and_then(|l| l.get_mut("parts"))
                .and_then(Value::as_array_mut)
            {
                last_parts.append(&mut parts);
                if fr_last {
                    move_function_responses_last(last_parts);
                }
            }
        } else {
            let mut entry = c.clone();
            if let Some(o) = entry.as_object_mut() {
                o.insert("parts".to_string(), Value::Array(parts));
            }
            out.push(entry);
        }
    }

    // Gemini requires the first turn to be a user turn.
    if !out.is_empty() && out[0].get("role").and_then(Value::as_str) != Some("user") {
        out.insert(0, json!({"role": "user", "parts": [{"text": "..."}]}));
    }

    // Gemini requires the last turn to be a user turn; answer any trailing
    // functionCalls with a synthesized functionResponse (#4273).
    if let Some(last) = out.last() {
        if last.get("role").and_then(Value::as_str) == Some("model") {
            let last_parts: Vec<Value> = last
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let fn_calls: Vec<&Value> = last_parts
                .iter()
                .filter(|p| {
                    p.get("functionCall")
                        .map(|v| v.is_object())
                        .unwrap_or(false)
                })
                .collect();
            let resp_parts: Vec<Value> = if !fn_calls.is_empty() {
                fn_calls
                    .iter()
                    .map(|p| {
                        let call = p.get("functionCall").cloned().unwrap_or(Value::Null);
                        let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let mut fr = json!({
                            "name": name,
                            "response": {"result": "Continue."}
                        });
                        if let Some(id) = call.get("id").and_then(Value::as_str) {
                            if !id.is_empty() {
                                fr["id"] = json!(id);
                            }
                        }
                        json!({"functionResponse": fr})
                    })
                    .collect()
            } else {
                vec![json!({"text": "Continue."})]
            };
            out.push(json!({"role": "user", "parts": resp_parts}));
        }
    }

    out
}

/// Build the `response` payload for a Gemini `functionResponse` from a raw
/// OpenAI tool message `content` value. Mirrors the parsing block in
/// `openai-to-gemini.js`: strings are JSON-parsed (a failed parse wraps the
/// raw string), non-strings pass through, objects/arrays are used directly,
/// and anything else is double-wrapped in `{result: ...}`.
fn tool_response_payload(resp: &Value) -> Value {
    let parsed: Option<Value> = match resp {
        Value::String(s) => serde_json::from_str::<Value>(s).ok(),
        other => Some(other.clone()),
    };
    let final_resp = match parsed {
        // Parse failure (or literal null) → wrap the raw value.
        None | Some(Value::Null) => json!({"result": resp}),
        Some(v) if v.is_object() || v.is_array() => v,
        Some(v) => json!({"result": v}),
    };
    json!({"result": final_resp})
}

/// Default thinking signature for antigravity (mirrors DEFAULT_THINKING_AG_SIGNATURE in JS).
pub const DEFAULT_THINKING_AG_SIGNATURE: &str = "EuwGCukGAXLI2nxwZIq54WWSoL/YN0P3TsDZ7zRnLi8g0S4aVr2HUGxvaHKySuY6HAVzcE0GPGjXrytLIldxthSvfxgUlJh6Qa9Z+Oj5QZBlYdg6HaJ6yuY5R7waE6rdwBsRf7Ft2j3DJ9rMi9qhWFqApewYtPhls3VHtuvND3l8Rm09+lbAXQs6KKWEWrxNLKTBkfpMgXhRERc/TQRMZu1twAablm6/Zk1tsYRvfWKLsNbeKF+CCojJdXJKvnR/8Ouuoa+Y2Ti20hcW7aZIIjZDFYPU//k6Ybmhg69J/imbFai2ckhfLaisqdDkdoIiBJScTOUvYqP6AE9d4MsydSC+UlhIMk4hoP76R8vUSCZRMkjOaDXstf/QoVZKbt94wyRZgAJ1G0BqI8L5ow86kLpA4wJEtxsRGymOE4bKUvApveBakYDNM9APkf+LbtbzWSseGjoZcSlycF9iN8Q2XNYKRrHbv3Lr5Y8JjdH/5y/6SHkNehTEZugaeGnSPSyCTWto1kQgHpxdWmhkLfJGNUGLmue7Mesj4TSms4J33mRpYVhNB/J333FCqIP0hr/E7BkkjEn7yZ4X7SQlh+xKPurapsnHRwiKmtsilmEFrnTE9iQr+pMr6M29qqFNv1tr5yumbaJw8JW9sB15tNsRv+dW6BjNanbsKz7HCgKUBc8tGy+7YuhXzAfViyRefcjK7eZW0Fbyt7AbybJTKz78W8NH7ye6LAwzOebXpeZ4D43fNIt8bKh26qgduSQv/7o+pAflkuqHZ99YWgHQ8h8OkZFi3eOiSYjsjhdZ/czWOdoPI/OnqIldzMPF5YlrKBLFX8VhRKVmqgsmWf5PHGulHhMkVlS+XG2UIseGy69ARa93D78Gsa+1n1kJr7EEB7Rh+27vUMxVYLdz1yMSvE5nalTAlg/ZeG8+XQ0cHuAI3KbQpHW2Q++RdXfm5JzD5WdJZUU+Zn8t8UUn85BH4RxZLeE0qJikgSsKoYVBc6YhiMjhPgkR95ReimY4Z0xCJdRo1gjexOFeODZMpQF6Yxnoic7IrdgsFA3iePTbFnPp3IAM1fAThWhXJUn3QInUOTd5o1qmTmn6REbL15g/JQNl+dqUoPkhleeb2V3kjqp1okmO3wMZbPknR3S1LZNmlS72/iBQUm+n2b/RCn4PjmM2";

/// Default thinking signature for gemini-cli (mirrors DEFAULT_THINKING_GEMINI_CLI_SIGNATURE in JS).
pub const DEFAULT_THINKING_GEMINI_CLI_SIGNATURE: &str = "CiQBjz1rX/AlslZWMe5RgBt4Tv9j4+YNZTTez+JH2/+5oAlICygKXgGPPWtf7/Sux9eLYap/bmYAdPqFThLXj+l7o0DLu/hdgU98MA9ZrlRDNHXx+T0tuY8AcnjPZbiDyOq2bE11Fjhsk6p5axqayaapC/Pt9GczcgIQf1z15WTxCeKWAPYKYQGPPWtfDYj0nlNFNoTlU39RC91Z16xFKJ2MLEmkm+NvimsoOJ6be3g2BssNPtJ/9BKDXRA5cVs17tBeeW72lH8TMB5999udtxHM2SiUsnWsrHlfVuGSCpNQQ+5REw8HNvEKkgEBjz1rXzBNWrqZGbjun55K+vgYPBhJO2qZ67uRWXUA5/qcU12U/mbi5XoA3swoxYE8LEXfZvFFC9WG/W28QNCA0Qd4Trk/WkWiAwZmB8a84Fs14rkv3wqyxwFavPkJorqurAfd2XzGiFy0sB0ITCOPYi1HzDGV5WfXk6b9k+jT66/RuzGa8EcSOWo/QtC3Bkhgowo4AY89a1/f/tw8A02zjIoK7JVDAbf8WUfmbApJJhwXIiGtu1M0JItObx7g2reYqT+HHL2Q/R4VDc=";

/// Core: Convert OpenAI request to Gemini format.
fn openai_to_gemini_base(model: &str, body: &Value, stream: bool, signature: &str) -> Value {
    let mut result = serde_json::json!({
        "contents": [],
        "generationConfig": {},
        "safetySettings": [
            {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF"},
            {"category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "OFF"}
        ]
    });

    // Generation config — Gemini 4 removed the sampling params, forwarding
    // them fails the request (#4802). Mirrors `allowSampling` in
    // openai-to-gemini.js.
    let allow_sampling = !is_gemini4_model(model);
    if allow_sampling {
        if let Some(temp) = body.get("temperature") {
            result["generationConfig"]["temperature"] = temp.clone();
        }
        if let Some(top_p) = body.get("top_p") {
            result["generationConfig"]["topP"] = top_p.clone();
        }
        if let Some(top_k) = body.get("top_k") {
            result["generationConfig"]["topK"] = top_k.clone();
        }
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        result["generationConfig"]["maxOutputTokens"] = max_tokens.clone();
    }

    // Build tool_call_id -> name map
    let mut tc_id_to_name: HashMap<String, String> = HashMap::new();
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("assistant") {
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tool_calls {
                        if tc.get("type").and_then(|v| v.as_str()) == Some("function") {
                            if let (Some(id), Some(name)) = (
                                tc.get("id").and_then(|v| v.as_str()),
                                tc.get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str()),
                            ) {
                                tc_id_to_name.insert(id.to_string(), name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Build tool responses cache — queued per id rather than a single value:
    // an OpenAI tool_call_id is only unique within its own assistant turn, so
    // a long agent session can reuse one id for two different tool results.
    // Each functionCall consumes the oldest unconsumed result for its id.
    // Mirrors `toolResponses` + `toolResponseCursor` in openai-to-gemini.js (#4273).
    let mut tool_responses: HashMap<String, Vec<Value>> = HashMap::new();
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
                if let (Some(tool_call_id), Some(content)) = (
                    msg.get("tool_call_id").and_then(|v| v.as_str()),
                    msg.get("content"),
                ) {
                    tool_responses
                        .entry(tool_call_id.to_string())
                        .or_default()
                        .push(content.clone());
                }
            }
        }
    }
    let mut tool_response_cursor: HashMap<String, usize> = HashMap::new();

    // Gemini validates that functionCall ids are unique across the WHOLE
    // history; each emitted functionCall takes the next free id and its
    // matching functionResponse reuses it (#4532).
    let mut tool_call_id_uniquifier = ToolCallIdUniquifier::default();

    // Convert messages
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for (i, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content = msg.get("content").cloned().unwrap_or(Value::Null);

            // System message
            if (role == "system" || role == "developer") && messages.len() > 1 {
                let system_text = extract_text_content(&content);
                if !system_text.is_empty() {
                    result["systemInstruction"] = serde_json::json!({
                        "role": "user",
                        "parts": [{"text": system_text}]
                    });
                }
                continue;
            }

            // User message (or system-only/developer-only)
            if role == "user" || ((role == "system" || role == "developer") && messages.len() == 1)
            {
                let parts = convert_openai_content_to_parts(&content);
                if !parts.is_empty() {
                    result["contents"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "role": "user",
                            "parts": parts
                        }));
                }
                continue;
            }

            // Assistant message
            if role == "assistant" {
                let mut parts = Vec::new();

                // Thinking/reasoning → thought part with signature
                if let Some(reasoning) = msg.get("reasoning_content").and_then(|v| v.as_str()) {
                    if !reasoning.is_empty() {
                        parts.push(serde_json::json!({
                            "thought": true,
                            "text": reasoning
                        }));
                        parts.push(serde_json::json!({
                            "thoughtSignature": signature,
                            "text": ""
                        }));
                    }
                }

                // Text content
                let text = extract_text_content(&content);
                if !text.is_empty() {
                    parts.push(serde_json::json!({"text": text}));
                }

                // Tool calls
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
                    // (orig_id, emit_id, call_name) — the ORIGINAL id resolves
                    // the queued tool result, the EMITTED id is uniquified, and
                    // the NAME travels with the pair so a later call reusing
                    // the id cannot rename this turn's response (#4273 #4532).
                    let mut tool_call_ids: Vec<(String, String, String)> = Vec::new();
                    for tc in tool_calls {
                        if tc.get("type").and_then(|v| v.as_str()) != Some("function") {
                            continue;
                        }
                        let fn_obj = tc.get("function").cloned().unwrap_or(Value::Null);
                        let name = fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let args = fn_obj
                            .get("arguments")
                            .map(|v| try_parse_json(v.as_str().unwrap_or("{}")))
                            .unwrap_or(Value::Null);
                        let orig_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let emit_id = tool_call_id_uniquifier.next(orig_id);

                        let mut call_part = serde_json::json!({
                            "thoughtSignature": signature,
                            "functionCall": {
                                "name": sanitize_gemini_function_name(name),
                                "args": args
                            }
                        });
                        if !emit_id.is_empty() {
                            call_part["functionCall"]["id"] = json!(emit_id);
                        }
                        parts.push(call_part);
                        tool_call_ids.push((orig_id.to_string(), emit_id, name.to_string()));
                    }

                    if !parts.is_empty() {
                        result["contents"]
                            .as_array_mut()
                            .unwrap()
                            .push(serde_json::json!({
                                "role": "model",
                                "parts": parts
                            }));
                    }

                    // Emit functionResponse parts whenever this isn't the last
                    // message (intermediate turn) or actual tool results are
                    // queued — matching upstream's `hasActualResponses ||
                    // isIntermediate`. Each call consumes the oldest
                    // unconsumed queued result for its ORIGINAL id; the
                    // emitted id is the uniquified one (#4273 #4532).
                    let is_intermediate = i + 1 < messages.len();
                    let has_actual_responses = tool_call_ids.iter().any(|(orig_id, _, _)| {
                        let cursor = tool_response_cursor.get(orig_id).copied().unwrap_or(0);
                        let queued = tool_responses.get(orig_id).map(|q| q.len()).unwrap_or(0);
                        cursor < queued
                    });
                    if has_actual_responses || is_intermediate {
                        let mut tool_parts = Vec::new();
                        for (orig_id, emit_id, call_name) in &tool_call_ids {
                            let queue = tool_responses.get(orig_id);
                            let cursor = tool_response_cursor.get(orig_id).copied().unwrap_or(0);
                            let resp = match queue {
                                Some(q) if cursor < q.len() => {
                                    let r = q[cursor].clone();
                                    *tool_response_cursor.entry(orig_id.clone()).or_insert(0) += 1;
                                    r
                                }
                                _ => Value::String(String::new()),
                            };

                            // Name comes from THIS call first; fall back to the
                            // conversation-wide map, then the id heuristic.
                            let name = if !call_name.is_empty() {
                                call_name.clone()
                            } else {
                                tc_id_to_name.get(orig_id).cloned().unwrap_or_else(|| {
                                    let id_parts: Vec<&str> = orig_id.split('-').collect();
                                    if id_parts.len() > 2 {
                                        id_parts[..id_parts.len() - 2].join("-")
                                    } else {
                                        orig_id.clone()
                                    }
                                })
                            };
                            let mut fr = serde_json::json!({
                                "name": sanitize_gemini_function_name(&name),
                                "response": tool_response_payload(&resp)
                            });
                            if !emit_id.is_empty() {
                                fr["id"] = json!(emit_id);
                            }
                            tool_parts.push(serde_json::json!({
                                "functionResponse": fr
                            }));
                        }
                        if !tool_parts.is_empty() {
                            result["contents"]
                                .as_array_mut()
                                .unwrap()
                                .push(serde_json::json!({
                                    "role": "user",
                                    "parts": tool_parts
                                }));
                        }
                    }
                } else if !parts.is_empty() {
                    result["contents"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "role": "model",
                            "parts": parts
                        }));
                }
            }
        }
    }

    // Convert tools
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let mut function_declarations = Vec::new();
        for t in tools {
            // Claude/Anthropic format (no type field, direct name/description/input_schema)
            if t.get("name").is_some() && t.get("input_schema").is_some() {
                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let description = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let schema = clean_json_schema(
                    t.get("input_schema")
                        .unwrap_or(&serde_json::json!({"type": "object", "properties": {}})),
                );
                function_declarations.push(serde_json::json!({
                    "name": sanitize_gemini_function_name(name),
                    "description": description,
                    "parameters": schema
                }));
            }
            // OpenAI format
            else if t.get("type").and_then(|v| v.as_str()) == Some("function") {
                if let Some(fn_obj) = t.get("function") {
                    let name = fn_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let description = fn_obj
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let schema = clean_json_schema(
                        fn_obj
                            .get("parameters")
                            .unwrap_or(&serde_json::json!({"type": "object", "properties": {}})),
                    );
                    function_declarations.push(serde_json::json!({
                        "name": sanitize_gemini_function_name(name),
                        "description": description,
                        "parameters": schema
                    }));
                }
            }
        }
        if !function_declarations.is_empty() {
            result["tools"] = serde_json::json!([{"functionDeclarations": function_declarations}]);
        }
    }

    // normalizeGeminiContents: merge adjacent same-role turns, strip empty
    // parts, sanitize functionResponse payloads ($ref rename), order
    // functionResponses last for Gemini 4, and guarantee initial/terminal
    // user turns (#4273, #4802).
    if let Some(contents) = result.get("contents").and_then(Value::as_array).cloned() {
        result["contents"] = Value::Array(normalize_gemini_contents(&contents, model));
    }

    let _ = stream; // Gemini handles stream via URL path, not body param
    result
}

/// Main entry point for OpenAI to Gemini request translation.
pub fn openai_to_gemini_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let result = openai_to_gemini_base(model, body, stream, DEFAULT_THINKING_AG_SIGNATURE);
    *body = result;
    true
}

/// OpenAI to Gemini CLI request (uses different thinking signature).
pub fn openai_to_gemini_cli_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let mut gemini =
        openai_to_gemini_base(model, body, stream, DEFAULT_THINKING_GEMINI_CLI_SIGNATURE);

    // Add thinking config for CLI
    if let Some(reasoning_effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        let budget = match reasoning_effort {
            "low" => 1024,
            "high" => 32768,
            _ => 8192, // medium
        };
        gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
            "thinkingBudget": budget,
            "includeThoughts": true
        });
    }

    // Thinking config from Claude format
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
                gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": budget,
                    "includeThoughts": true
                });
            }
        }
    }

    // Clean schema for tools
    if let Some(tools_arr) = gemini.get_mut("tools").and_then(|v| v.as_array_mut()) {
        if let Some(first_tool) = tools_arr.first_mut() {
            if let Some(func_decls) = first_tool
                .get_mut("functionDeclarations")
                .and_then(|v| v.as_array_mut())
            {
                for fn_decl in func_decls {
                    if let Some(params) = fn_decl.get_mut("parameters") {
                        let cleaned = clean_json_schema(params);
                        *params = cleaned;
                    }
                }
            }
        }
    }

    *body = gemini;
    true
}

/// OpenAI to Antigravity request translation.
///
/// Unlike plain Gemini translation, this function:
/// 1. Wraps the Gemini-shaped body in a Cloud Code envelope (`{"request": body}`).
/// 2. Injects the Antigravity default system prompt into `systemInstruction`
///    using a double-prompt pattern (Cloud Code system + Antigravity system).
/// 3. Sets `toolConfig.functionCallingConfig.mode = "VALIDATED"` when tools
///    are present (Gemini 3+ requirement for validated function calling).
pub fn openai_to_antigravity_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let mut gemini = openai_to_gemini_base(model, body, stream, DEFAULT_THINKING_AG_SIGNATURE);

    // Add thinking config from reasoning_effort
    if let Some(reasoning_effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        let budget = match reasoning_effort {
            "low" => 1024,
            "high" => 32768,
            _ => 8192, // medium
        };
        gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
            "thinkingBudget": budget,
            "includeThoughts": true
        });
    }

    // Thinking config from Claude format
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
                gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": budget,
                    "includeThoughts": true
                });
            }
        }
    }

    // Clean schema for tools
    if let Some(tools_arr) = gemini.get_mut("tools").and_then(|v| v.as_array_mut()) {
        if let Some(first_tool) = tools_arr.first_mut() {
            if let Some(func_decls) = first_tool
                .get_mut("functionDeclarations")
                .and_then(|v| v.as_array_mut())
            {
                for fn_decl in func_decls {
                    if let Some(params) = fn_decl.get_mut("parameters") {
                        let cleaned = clean_json_schema(params);
                        *params = cleaned;
                    }
                }
            }
        }
    }

    // Inject Antigravity default system prompt into systemInstruction.
    // Use the double-prompt pattern: the Cloud Code system prompt is inserted
    // as a separate part at the beginning, followed by the user's own system
    // instruction. This mirrors how the Antigravity executor expects it.
    let ag_system_text = ANTIGRAVITY_DEFAULT_SYSTEM;
    let existing_system = gemini.get("systemInstruction").cloned();
    gemini["systemInstruction"] = serde_json::json!({
        "role": "user",
        "parts": [
            {"text": ag_system_text},
            {"text": "\n\n---\n\n"}
        ]
    });
    if let Some(si) = existing_system {
        if let Some(parts) = si.get("parts").and_then(|v| v.as_array()) {
            for part in parts {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        gemini["systemInstruction"]["parts"]
                            .as_array_mut()
                            .unwrap()
                            .push(serde_json::json!({"text": text}));
                    }
                }
            }
        }
    }

    // Cloud Code URL omits the model, so surface it inside the envelope body.
    gemini["model"] = serde_json::json!(model);
    let inner = std::mem::replace(&mut gemini, Value::Null);
    *body = serde_json::json!({"request": inner});

    // Set toolConfig when tools are present
    if let Some(req_obj) = body.get_mut("request").and_then(|v| v.as_object_mut()) {
        if req_obj.contains_key("tools") {
            req_obj.insert(
                "toolConfig".to_string(),
                serde_json::json!({
                    "functionCallingConfig": {"mode": "VALIDATED"}
                }),
            );
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_openai_to_gemini() {
        let mut body: Value = serde_json::from_str(
            r#"{
                "model": "gemini-pro",
                "messages": [
                    {"role": "user", "content": "Hello"}
                ]
            }"#,
        )
        .unwrap();

        openai_to_gemini_request("gemini-pro", &mut body, true, None);

        assert!(body.get("contents").is_some());
        assert!(body.get("generationConfig").is_some());
    }

    // ---- .4: `properties` map must not be treated as a schema node ----

    #[test]
    fn test_ensure_object_type_param_named_properties() {
        // A tool parameter literally named `properties` must survive as a
        // normal property — the properties MAP is not a schema node.
        let mut schema: Value = serde_json::json!({
            "type": "object",
            "properties": {
                "properties": {"type": "string"},
                "nested": {
                    "properties": {"inner": {"type": "number"}}
                }
            }
        });
        clean_json_schema_for_antigravity(&mut schema);

        let props = schema["properties"].as_object().unwrap();
        // Phase 6 (`enforceTypeConstraints`, upstream gemini.js:393-425)
        // recurses into EVERY object value — including the `properties` map
        // itself. That map node has no `type`, so upstream deletes its
        // `properties`/`required`/`items` keys, which are the params literally
        // named `properties`/`required`/`items`. Verified against upstream:
        // `cleanJSONSchemaForAntigravity` drops the `properties` param too.
        // Parity keeps the strip; the .4 guarantee (the properties MAP is not
        // a schema node for type inference) still holds for every other param.
        assert!(
            props.get("properties").is_none(),
            "upstream Phase 6 strips a param literally named `properties`: {props:?}"
        );
        // The nested object schema still gets type:"object" inferred.
        assert_eq!(
            props["nested"]["type"],
            json!("object"),
            "nested schema with properties must get type:object"
        );
        assert_eq!(
            props["nested"]["properties"]["inner"],
            json!({"type": "number"})
        );
    }

    // ---- .5: uniquify duplicate tool_call_ids + per-id result queue ----

    #[test]
    fn test_duplicate_tool_call_ids_get_unique_emit_ids() {
        let mut body: Value = serde_json::json!({
            "model": "gemini-3-pro",
            "messages": [
                {"role": "user", "content": "go"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "tool_a", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "first"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "tool_b", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "second"}
            ]
        });

        openai_to_gemini_request("gemini-3-pro", &mut body, true, None);

        let contents = body["contents"].as_array().unwrap();
        let mut call_ids: Vec<String> = Vec::new();
        let mut resp_ids: Vec<String> = Vec::new();
        let mut resp_results: Vec<String> = Vec::new();
        for c in contents {
            for p in c["parts"].as_array().unwrap() {
                if let Some(fc) = p.get("functionCall") {
                    call_ids.push(fc["id"].as_str().unwrap().to_string());
                }
                if let Some(fr) = p.get("functionResponse") {
                    resp_ids.push(fr["id"].as_str().unwrap().to_string());
                    // response.result.result is the double-wrapped payload
                    resp_results.push(
                        fr["response"]["result"]["result"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                    );
                }
            }
        }
        // Both functionCall ids must be unique across the whole history.
        assert_eq!(call_ids, vec!["call_1", "call_1-2"]);
        // Each functionResponse reuses its call's emitted id.
        assert_eq!(resp_ids, vec!["call_1", "call_1-2"]);
        // Oldest unconsumed result per id: first call gets "first", second "second".
        assert_eq!(resp_results, vec!["first", "second"]);
    }

    #[test]
    fn test_tool_call_id_uniquifier() {
        let mut u = ToolCallIdUniquifier::default();
        assert_eq!(u.next("a"), "a");
        assert_eq!(u.next("a"), "a-2");
        assert_eq!(u.next("a"), "a-3");
        assert_eq!(u.next("b"), "b");
        // An id that already ends in -2 keeps counting.
        u.next("x-2");
        assert_eq!(u.next("x-2"), "x-2-2");
    }

    // ---- .6: Gemini 4 sampling params + normalize_gemini_contents ----

    #[test]
    fn test_gemini4_drops_sampling_params() {
        let body_in = |model: &str| {
            json!({
                "model": model,
                "messages": [{"role": "user", "content": "hi"}],
                "temperature": 0.5,
                "top_p": 0.9,
                "top_k": 40,
                "max_tokens": 100
            })
        };

        let mut g4 = body_in("gemini-4-pro");
        openai_to_gemini_request("gemini-4-pro", &mut g4, true, None);
        let gc = &g4["generationConfig"];
        assert!(gc.get("temperature").is_none());
        assert!(gc.get("topP").is_none());
        assert!(gc.get("topK").is_none());
        // max_tokens is still forwarded.
        assert_eq!(gc["maxOutputTokens"], json!(100));

        // Prefixed model ids also match (upstream regex allows `provider/gemini-4...`).
        let mut g4p = body_in("google/gemini-4.0-pro");
        openai_to_gemini_request("google/gemini-4.0-pro", &mut g4p, true, None);
        assert!(g4p["generationConfig"].get("temperature").is_none());

        // Gemini 3 keeps sampling params.
        let mut g3 = body_in("gemini-3-pro");
        openai_to_gemini_request("gemini-3-pro", &mut g3, true, None);
        let gc3 = &g3["generationConfig"];
        assert_eq!(gc3["temperature"], json!(0.5));
        assert_eq!(gc3["topP"], json!(0.9));
        assert_eq!(gc3["topK"], json!(40));
    }

    #[test]
    fn test_normalize_merges_adjacent_same_role() {
        let contents = vec![
            json!({"role": "user", "parts": [{"text": "a"}]}),
            json!({"role": "user", "parts": [{"text": "b"}, {}]}),
            json!({"role": "model", "parts": [{"text": "c"}]}),
        ];
        let out = normalize_gemini_contents(&contents, "gemini-3-pro");
        assert_eq!(
            out.len(),
            3,
            "merged user + model + synthetic user: {:?}",
            out
        );
        assert_eq!(out[0]["role"], "user");
        // Empty object part stripped; adjacent user turns merged.
        assert_eq!(
            out[0]["parts"].as_array().unwrap().len(),
            2,
            "merged user parts: {:?}",
            out[0]["parts"]
        );
        // Terminal model turn answered with a synthetic user turn.
        assert_eq!(out[1]["role"], "model");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["parts"][0]["text"], "Continue.");
    }

    #[test]
    fn test_normalize_unresponded_function_call_answered() {
        let contents = vec![
            json!({"role": "user", "parts": [{"text": "hi"}]}),
            json!({
                "role": "model",
                "parts": [{
                    "functionCall": {"id": "call_9", "name": "my_tool", "args": {}}
                }]
            }),
        ];
        let out = normalize_gemini_contents(&contents, "gemini-3-pro");
        assert_eq!(out.len(), 3);
        let fr = &out[2]["parts"][0]["functionResponse"];
        assert_eq!(fr["id"], "call_9");
        assert_eq!(fr["name"], "my_tool");
        assert_eq!(fr["response"]["result"], "Continue.");
    }

    #[test]
    fn test_gemini4_function_response_ordered_last_on_merge() {
        // Two adjacent user turns merge; for Gemini 4 the functionResponse
        // part must end up last in the merged turn (#4802).
        let contents = vec![
            json!({"role": "user", "parts": [{"text": "a"}]}),
            json!({
                "role": "user",
                "parts": [{
                    "functionResponse": {"name": "t", "response": {"result": "ok"}}
                }]
            }),
            json!({"role": "user", "parts": [{"text": "tail"}]}),
        ];
        let out = normalize_gemini_contents(&contents, "gemini-4-pro");
        assert_eq!(out.len(), 1, "all user turns merged: {:?}", out);
        let parts = out[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert!(
            parts[2].get("functionResponse").is_some(),
            "functionResponse must be last for gemini-4: {:?}",
            parts
        );
    }

    // ---- .7: $ref keys renamed inside functionResponse payloads ----

    #[test]
    fn test_sanitize_function_response_payload_renames_ref() {
        let payload = json!({
            "result": {
                "schema": {
                    "$ref": "#/$defs/Thing",
                    "nested": [{"$ref": "#/other"}, {"keep": 1}]
                },
                "$ref": "top-level"
            }
        });
        let out = sanitize_function_response_payload(&payload);
        assert!(out["result"]["schema"].get("$ref").is_none());
        assert_eq!(out["result"]["schema"]["_ref"], "#/$defs/Thing");
        assert_eq!(out["result"]["schema"]["nested"][0]["_ref"], "#/other");
        assert_eq!(out["result"]["schema"]["nested"][1]["keep"], 1);
        assert_eq!(out["result"]["_ref"], "top-level");
    }

    #[test]
    fn test_function_response_ref_renamed_end_to_end() {
        let mut body: Value = serde_json::json!({
            "model": "gemini-3-pro",
            "messages": [
                {"role": "user", "content": "fetch"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_ref",
                        "type": "function",
                        "function": {"name": "webfetch", "arguments": "{}"}
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_ref",
                    "content": "{\"$ref\": \"#/$defs/X\", \"url\": \"https://example.com\"}"
                }
            ]
        });
        openai_to_gemini_request("gemini-3-pro", &mut body, true, None);

        let contents = body["contents"].as_array().unwrap();
        let fr_part = contents
            .iter()
            .flat_map(|c| c["parts"].as_array().unwrap().iter())
            .find(|p| p.get("functionResponse").is_some())
            .expect("functionResponse part");
        let response = &fr_part["functionResponse"]["response"];
        // The parsed tool result object is placed under result; $ref renamed.
        let result_obj = &response["result"];
        assert!(
            serde_json::to_string(result_obj).unwrap().contains("_ref"),
            "expected _ref rename in {:?}",
            result_obj
        );
        assert!(!serde_json::to_string(result_obj)
            .unwrap()
            .contains("\"$ref\""));
    }

    // ---- .19: Phase 6 enforceTypeConstraints ----

    #[test]
    fn test_phase6_strips_items_from_non_array() {
        let mut schema = json!({
            "type": "string",
            "items": {"type": "number"},
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        });
        clean_json_schema_for_antigravity(&mut schema);
        assert!(schema.get("items").is_none());
        assert!(schema.get("properties").is_none());
        assert!(schema.get("required").is_none());
        assert_eq!(schema["type"], "string");
    }

    #[test]
    fn test_phase6_strips_properties_from_array() {
        let mut schema = json!({
            "type": "array",
            "items": {"type": "string"},
            "properties": {"x": {"type": "string"}},
            "required": ["x"]
        });
        clean_json_schema_for_antigravity(&mut schema);
        assert!(schema.get("properties").is_none());
        assert!(schema.get("required").is_none());
        assert_eq!(schema["items"], json!({"type": "string"}));
    }

    #[test]
    fn test_phase6_array_items_inferred_when_type_missing() {
        // items without `type` but with properties → object
        let mut schema = json!({
            "type": "array",
            "items": {"properties": {"a": {"type": "string"}}}
        });
        clean_json_schema_for_antigravity(&mut schema);
        assert_eq!(schema["items"]["type"], "object");

        // items without type/properties/items → string
        let mut schema2 = json!({
            "type": "array",
            "items": {"description": "anything"}
        });
        clean_json_schema_for_antigravity(&mut schema2);
        assert_eq!(schema2["items"]["type"], "string");

        // nested arrays infer items → array
        let mut schema3 = json!({
            "type": "array",
            "items": {"items": {"type": "integer"}}
        });
        clean_json_schema_for_antigravity(&mut schema3);
        assert_eq!(schema3["items"]["type"], "array");
    }

    #[test]
    fn test_phase6_recursive_in_nested_schemas() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "bad_string": {
                    "type": "string",
                    "items": {"type": "number"},
                    "required": ["z"]
                },
                "list": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"n": {"type": "number"}}}
                }
            }
        });
        clean_json_schema_for_antigravity(&mut schema);
        let props = &schema["properties"];
        assert!(props["bad_string"].get("items").is_none());
        assert!(props["bad_string"].get("required").is_none());
        assert_eq!(props["list"]["items"]["type"], "object");
        assert_eq!(props["list"]["items"]["properties"]["n"]["type"], "number");
    }

    #[test]
    fn test_is_gemini4_model() {
        assert!(is_gemini4_model("gemini-4-pro"));
        assert!(is_gemini4_model("gemini-4"));
        assert!(is_gemini4_model("google/gemini-4.0-pro"));
        assert!(is_gemini4_model("GEMINI-4-PRO"));
        assert!(!is_gemini4_model("gemini-3-pro"));
        assert!(!is_gemini4_model("gemini-4x"));
        assert!(!is_gemini4_model("mygemini-4-pro"));
        assert!(!is_gemini4_model(""));
    }

    // ---- #4283: non-standard MCP schema keywords must be stripped ----

    #[test]
    fn test_strips_nonstandard_mcp_schema_keywords() {
        // Gemini rejects the whole request ("Unknown name X: Cannot find
        // field") when a tool schema carries these keywords.
        let mut schema = json!({
            "type": "object",
            "properties": {
                "q": {
                    "type": "string",
                    "errorMessage": "required",
                    "errorMessages": {"required": "x"},
                    "markdownDescription": "docs",
                    "doNotSuggest": true,
                    "suggestSortText": "1",
                    "minProperties": 1,
                    "maxProperties": 5,
                    "x-taplo-info": {"foo": 1}
                }
            },
            "minProperties": 1,
            "maxProperties": 9,
            "errorMessage": "top-level"
        });
        clean_json_schema_for_antigravity(&mut schema);
        let q = &schema["properties"]["q"];
        for key in [
            "errorMessage",
            "errorMessages",
            "markdownDescription",
            "doNotSuggest",
            "suggestSortText",
            "minProperties",
            "maxProperties",
            "x-taplo-info",
        ] {
            assert!(
                q.get(key).is_none(),
                "property-level {key} must be stripped"
            );
        }
        for key in ["minProperties", "maxProperties", "errorMessage"] {
            assert!(
                schema.get(key).is_none(),
                "top-level {key} must be stripped"
            );
        }
        // Standard keywords survive.
        assert_eq!(q["type"], "string");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].get("q").is_some());
    }

    // ---- mozza.10: remote images get a concrete MIME (never image/*) ----

    #[test]
    fn remote_image_url_emits_filedata_with_concrete_mime() {
        let url = "https://files.example.com/reference/photo.jpg?token=abc";
        let parts = convert_openai_content_to_parts(&serde_json::json!([
            {"type": "image_url", "image_url": {"url": url}}
        ]));
        assert_eq!(
            parts,
            vec![serde_json::json!({
                "fileData": {"fileUri": url, "mimeType": "image/jpeg"}
            })]
        );
        // Unknown extension → DEFAULT_IMAGE_MIME fallback.
        let parts = convert_openai_content_to_parts(&serde_json::json!([
            {"type": "image_url", "image_url": {"url": "https://x.test/pic"}}
        ]));
        assert_eq!(parts[0]["fileData"]["mimeType"], "image/png");
        // Data URIs still inline.
        let parts = convert_openai_content_to_parts(&serde_json::json!([
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAB"}}
        ]));
        assert_eq!(parts[0]["inlineData"]["data"], "AAAB");
    }

    // ---- mozza.12: multi-type flatten picks the best fit ----

    #[test]
    fn flatten_type_arrays_prefers_structural_clues() {
        let mut schema = serde_json::json!({
            "type": ["null", "boolean", "object", "array", "number", "string"],
            "properties": {"a": {"type": "string"}},
            "items": {"type": "string"}
        });
        flatten_type_arrays(&mut schema);
        // properties present + "object" in the union → object wins.
        assert_eq!(schema["type"], "object");

        let mut schema = serde_json::json!({
            "type": ["boolean", "array", "number"],
            "items": {"type": "string"}
        });
        flatten_type_arrays(&mut schema);
        // non-empty items with type + "array" in union → array.
        assert_eq!(schema["type"], "array");

        let mut schema = serde_json::json!({"type": ["boolean", "number", "integer"]});
        flatten_type_arrays(&mut schema);
        // No structural clues: string > number > integer > boolean.
        assert_eq!(schema["type"], "number");

        let mut schema = serde_json::json!({"type": ["boolean", "integer"]});
        flatten_type_arrays(&mut schema);
        assert_eq!(schema["type"], "integer");

        let mut schema = serde_json::json!({"type": ["null"]});
        flatten_type_arrays(&mut schema);
        assert_eq!(schema["type"], "string");
    }
}
