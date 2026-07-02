//! OpenAI to Gemini request translator
//!
//! Converts OpenAI Chat Completions format to Gemini API format.

use serde_json::Value;
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
    "propertyNames",
    "patternProperties",
    "enumDescriptions",
    // Complex schema keywords (handled by flattenAnyOfOneOf/mergeAllOf)
    "anyOf",
    "oneOf",
    "allOf",
    "not",
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
                let new_type = if non_null.is_empty() {
                    Value::String("string".to_string())
                } else {
                    non_null[0].clone()
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
fn ensure_object_type(obj: &mut Value) {
    match obj {
        Value::Object(map) => {
            if map.contains_key("properties") && !map.contains_key("type") {
                map.insert("type".to_string(), Value::String("object".to_string()));
            }
            for (_key, val) in map.iter_mut() {
                if val.is_object() || val.is_array() {
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
    flatten_anyof_oneof(schema);
    flatten_type_arrays(schema);

    // Phase 2.5: Infer missing type=object when properties exist (Gemini requirement)
    ensure_object_type(schema);

    // Phase 3: Remove all unsupported keywords at ALL levels
    remove_unsupported_keywords(schema, UNSUPPORTED_SCHEMA_CONSTRAINTS);

    // Phase 4: Cleanup required fields recursively
    cleanup_required(schema);

    // Phase 5: Add placeholder for empty object schemas (Antigravity requirement)
    add_placeholders(schema);
}

/// Clean JSON Schema for Gemini/Antigravity compatibility (legacy function, now delegates to clean_json_schema_for_antigravity).
/// Made `pub(crate)` so the Antigravity executor can use it for tool schema cleaning.
pub(crate) fn clean_json_schema(schema: &Value) -> Value {
    let mut result = schema.clone();
    clean_json_schema_for_antigravity(&mut result);
    result
}

/// Default thinking signature for antigravity (mirrors DEFAULT_THINKING_AG_SIGNATURE in JS).
pub const DEFAULT_THINKING_AG_SIGNATURE: &str = "EuwGCukGAXLI2nxwZIq54WWSoL/YN0P3TsDZ7zRnLi8g0S4aVr2HUGxvaHKySuY6HAVzcE0GPGjXrytLIldxthSvfxgUlJh6Qa9Z+Oj5QZBlYdg6HaJ6yuY5R7waE6rdwBsRf7Ft2j3DJ9rMi9qhWFqApewYtPhls3VHtuvND3l8Rm09+lbAXQs6KKWEWrxNLKTBkfpMgXhRERc/TQRMZu1twAablm6/Zk1tsYRvfWKLsNbeKF+CCojJdXJKvnR/8Ouuoa+Y2Ti20hcW7aZIIjZDFYPU//k6Ybmhg69J/imbFai2ckhfLaisqdDkdoIiBJScTOUvYqP6AE9d4MsydSC+UlhIMk4hoP76R8vUSCZRMkjOaDXstf/QoVZKbt94wyRZgAJ1G0BqI8L5ow86kLpA4wJEtxsRGymOE4bKUvApveBakYDNM9APkf+LbtbzWSseGjoZcSlycF9iN8Q2XNYKRrHbv3Lr5Y8JjdH/5y/6SHkNehTEZugaeGnSPSyCTWto1kQgHpxdWmhkLfJGNUGLmue7Mesj4TSms4J33mRpYVhNB/J333FCqIP0hr/E7BkkjEn7yZ4X7SQlh+xKPurapsnHRwiKmtsilmEFrnTE9iQr+pMr6M29qqFNv1tr5yumbaJw8JW9sB15tNsRv+dW6BjNanbsKz7HCgKUBc8tGy+7YuhXzAfViyRefcjK7eZW0Fbyt7AbybJTKz78W8NH7ye6LAwzOebXpeZ4D43fNIt8bKh26qgduSQv/7o+pAflkuqHZ99YWgHQ8h8OkZFi3eOiSYjsjhdZ/czWOdoPI/OnqIldzMPF5YlrKBLFX8VhRKVmqgsmWf5PHGulHhMkVlS+XG2UIseGy69ARa93D78Gsa+1n1kJr7EEB7Rh+27vUMxVYLdz1yMSvE5nalTAlg/ZeG8+XQ0cHuAI3KbQpHW2Q++RdXfm5JzD5WdJZUU+Zn8t8UUn85BH4RxZLeE0qJikgSsKoYVBc6YhiMjhPgkR95ReimY4Z0xCJdRo1gjexOFeODZMpQF6Yxnoic7IrdgsFA3iePTbFnPp3IAM1fAThWhXJUn3QInUOTd5o1qmTmn6REbL15g/JQNl+dqUoPkhleeb2V3kjqp1okmO3wMZbPknR3S1LZNmlS72/iBQUm+n2b/RCn4PjmM2";

/// Default thinking signature for gemini-cli (mirrors DEFAULT_THINKING_GEMINI_CLI_SIGNATURE in JS).
pub const DEFAULT_THINKING_GEMINI_CLI_SIGNATURE: &str = "CiQBjz1rX/AlslZWMe5RgBt4Tv9j4+YNZTTez+JH2/+5oAlICygKXgGPPWtf7/Sux9eLYap/bmYAdPqFThLXj+l7o0DLu/hdgU98MA9ZrlRDNHXx+T0tuY8AcnjPZbiDyOq2bE11Fjhsk6p5axqayaapC/Pt9GczcgIQf1z15WTxCeKWAPYKYQGPPWtfDYj0nlNFNoTlU39RC91Z16xFKJ2MLEmkm+NvimsoOJ6be3g2BssNPtJ/9BKDXRA5cVs17tBeeW72lH8TMB5999udtxHM2SiUsnWsrHlfVuGSCpNQQ+5REw8HNvEKkgEBjz1rXzBNWrqZGbjun55K+vgYPBhJO2qZ67uRWXUA5/qcU12U/mbi5XoA3swoxYE8LEXfZvFFC9WG/W28QNCA0Qd4Trk/WkWiAwZmB8a84Fs14rkv3wqyxwFavPkJorqurAfd2XzGiFy0sB0ITCOPYi1HzDGV5WfXk6b9k+jT66/RuzGa8EcSOWo/QtC3Bkhgowo4AY89a1/f/tw8A02zjIoK7JVDAbf8WUfmbApJJhwXIiGtu1M0JItObx7g2reYqT+HHL2Q/R4VDc=";

/// Core: Convert OpenAI request to Gemini format.
fn openai_to_gemini_base(model: &str, body: &Value, stream: bool, signature: &str) -> Value {
    let mut result = serde_json::json!({
        "model": model,
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

    // Generation config
    if let Some(temp) = body.get("temperature") {
        result["generationConfig"]["temperature"] = temp.clone();
    }
    if let Some(top_p) = body.get("top_p") {
        result["generationConfig"]["topP"] = top_p.clone();
    }
    if let Some(top_k) = body.get("top_k") {
        result["generationConfig"]["topK"] = top_k.clone();
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

    // Build tool responses cache
    let mut tool_responses: HashMap<String, String> = HashMap::new();
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
                if let (Some(tool_call_id), Some(content)) = (
                    msg.get("tool_call_id").and_then(|v| v.as_str()),
                    msg.get("content"),
                ) {
                    tool_responses.insert(
                        tool_call_id.to_string(),
                        serde_json::to_string(content).unwrap_or_default(),
                    );
                }
            }
        }
    }

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
                    let mut tool_call_ids: Vec<String> = Vec::new();
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
                        let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");

                        parts.push(serde_json::json!({
                            "thoughtSignature": signature,
                            "functionCall": {
                                "id": id,
                                "name": sanitize_gemini_function_name(name),
                                "args": args
                            }
                        }));
                        if !id.is_empty() {
                            tool_call_ids.push(id.to_string());
                        }
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

                    // Check if there are actual tool responses
                    let has_actual_responses = tool_call_ids
                        .iter()
                        .any(|fid| tool_responses.contains_key(fid));
                    if has_actual_responses {
                        let mut tool_parts = Vec::new();
                        for fid in &tool_call_ids {
                            if let Some(resp_str) = tool_responses.get(fid) {
                                let name = tc_id_to_name.get(fid).cloned().unwrap_or_else(|| {
                                    let id_parts: Vec<&str> = fid.split('-').collect();
                                    if id_parts.len() > 2 {
                                        id_parts[..id_parts.len() - 2].join("-")
                                    } else {
                                        fid.clone()
                                    }
                                });
                                let parsed_resp = try_parse_json(resp_str);
                                let final_resp =
                                    if parsed_resp.is_object() || parsed_resp.is_array() {
                                        parsed_resp
                                    } else {
                                        serde_json::json!({"result": parsed_resp})
                                    };
                                tool_parts.push(serde_json::json!({
                                    "functionResponse": {
                                        "id": fid,
                                        "name": sanitize_gemini_function_name(&name),
                                        "response": {"result": final_resp}
                                    }
                                }));
                            }
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
            "include_thoughts": true
        });
    }

    // Thinking config from Claude format
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
                gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": budget,
                    "include_thoughts": true
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
    let mut gemini =
        openai_to_gemini_base(model, body, stream, DEFAULT_THINKING_AG_SIGNATURE);

    // Add thinking config from reasoning_effort
    if let Some(reasoning_effort) = body.get("reasoning_effort").and_then(|v| v.as_str()) {
        let budget = match reasoning_effort {
            "low" => 1024,
            "high" => 32768,
            _ => 8192, // medium
        };
        gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
            "thinkingBudget": budget,
            "include_thoughts": true
        });
    }

    // Thinking config from Claude format
    if let Some(thinking) = body.get("thinking") {
        if thinking.get("type").and_then(|v| v.as_str()) == Some("enabled") {
            if let Some(budget) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
                gemini["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": budget,
                    "include_thoughts": true
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

    // Wrap in Cloud Code envelope
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
}
