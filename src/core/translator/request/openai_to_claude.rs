use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

const CLAUDE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const DEFAULT_MAX_TOKENS: u32 = 64000;
const DEFAULT_MIN_TOKENS: u32 = 32000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolNameMap(HashMap<String, String>);

impl ToolNameMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }
    pub fn insert(&mut self, prefixed: String, original: String) {
        self.0.insert(prefixed, original);
    }
    pub fn get(&self, prefixed: &str) -> Option<&String> {
        self.0.get(prefixed)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Default for ToolNameMap {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct TransformResult {
    pub model: String,
    pub max_tokens: u32,
    pub stream: bool,
    pub messages: Vec<ClaudeMessage>,
    pub system: Vec<ClaudeSystemBlock>,
    pub tools: Vec<ClaudeTool>,
    pub tool_choice: Option<ClaudeToolChoice>,
    pub thinking: Option<ClaudeThinking>,
    pub _tool_name_map: Option<ToolNameMap>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub cache_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeMessage {
    pub role: String,
    pub content: Vec<ClaudeContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeSystemBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeToolChoice {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "any")]
    Any,
    #[serde(rename = "tool")]
    Tool { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeThinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

fn extract_text_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn try_parse_json<T: for<'de> Deserialize<'de>>(str: &str) -> Option<T> {
    serde_json::from_str(str).ok()
}

fn get_content_blocks_from_message(
    msg: &Value,
    tool_name_map: &mut ToolNameMap,
) -> Vec<ClaudeContentBlock> {
    let mut blocks = Vec::new();
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");

    if role == "tool" {
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let content = msg.get("content").map(|v| v.clone()).unwrap_or(Value::Null);
        blocks.push(ClaudeContentBlock::ToolResult {
            tool_use_id: tool_call_id,
            content,
            is_error: None,
        });
    } else if role == "user" {
        let content = msg.get("content");
        if let Some(Value::String(s)) = content {
            if !s.is_empty() {
                blocks.push(ClaudeContentBlock::Text { text: s.clone() });
            }
        } else if let Some(Value::Array(arr)) = content {
            for part in arr {
                let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match part_type {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                blocks.push(ClaudeContentBlock::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                    "tool_result" => {
                        let tool_use_id = part
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let content = part
                            .get("content")
                            .map(|v| v.clone())
                            .unwrap_or(Value::Null);
                        let is_error = part.get("is_error").and_then(|v| v.as_bool());
                        blocks.push(ClaudeContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        });
                    }
                    "image_url" => {
                        if let Some(url) = part
                            .get("image_url")
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                        {
                            if url.starts_with("data:") {
                                if let Some(comma_pos) = url.find(";base64,") {
                                    let prefix = &url[5..comma_pos];
                                    let data = &url[comma_pos + 8..];
                                    blocks.push(ClaudeContentBlock::Image {
                                        source: ImageSource {
                                            source_type: "base64".to_string(),
                                            media_type: Some(prefix.to_string()),
                                            data: Some(data.to_string()),
                                            url: None,
                                        },
                                    });
                                }
                            } else if url.starts_with("http://") || url.starts_with("https://") {
                                blocks.push(ClaudeContentBlock::Image {
                                    source: ImageSource {
                                        source_type: "url".to_string(),
                                        media_type: None,
                                        data: None,
                                        url: Some(url.to_string()),
                                    },
                                });
                            }
                        }
                    }
                    "image" => {
                        if let Some(source) = part.get("source") {
                            let source_type = source
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("base64")
                                .to_string();
                            let media_type = source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let data = source
                                .get("data")
                                .and_then(|v| v.as_str())
                                .map(String::from);
                            let url = source.get("url").and_then(|v| v.as_str()).map(String::from);
                            blocks.push(ClaudeContentBlock::Image {
                                source: ImageSource {
                                    source_type,
                                    media_type,
                                    data,
                                    url,
                                },
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    } else if role == "assistant" {
        if let Some(content_arr) = msg.get("content").and_then(|v| v.as_array()) {
            for part in content_arr {
                let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match part_type {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                blocks.push(ClaudeContentBlock::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                    "tool_use" => {
                        let id = part
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = part
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = part.get("input").map(|v| v.clone()).unwrap_or(Value::Null);
                        blocks.push(ClaudeContentBlock::ToolUse { id, name, input });
                    }
                    "thinking" => {
                        let thinking = part
                            .get("thinking")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        blocks.push(ClaudeContentBlock::Thinking {
                            thinking,
                            cache_control: None,
                        });
                    }
                    _ => {}
                }
            }
        } else if let Some(content_val) = msg.get("content") {
            let text = extract_text_content(content_val);
            if !text.is_empty() {
                blocks.push(ClaudeContentBlock::Text { text });
            }
        }

        if let Some(tool_calls) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let tc_type = tc.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if tc_type == "function" {
                    let func = tc.get("function");
                    let name = func
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = func
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .unwrap_or("{}");

                    let input: Value = try_parse_json(arguments).unwrap_or(Value::Null);
                    let id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    blocks.push(ClaudeContentBlock::ToolUse { id, name, input });
                }
            }
        }
    }

    blocks
}

fn convert_openai_tool_choice(choice: &Value) -> Option<ClaudeToolChoice> {
    if choice.is_null() {
        return Some(ClaudeToolChoice::Auto);
    }

    if let Some(s) = choice.as_str() {
        match s {
            "auto" | "none" => return Some(ClaudeToolChoice::Auto),
            "required" => return Some(ClaudeToolChoice::Any),
            _ => {}
        }
    }

    if let Some(obj) = choice.as_object() {
        if obj.contains_key("type") {
            let t = obj.get("type")?.as_str()?;
            match t {
                "auto" | "none" => Some(ClaudeToolChoice::Auto),
                "any" => Some(ClaudeToolChoice::Any),
                "function" => {
                    let name = obj
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())?;
                    Some(ClaudeToolChoice::Tool {
                        name: name.to_string(),
                    })
                }
                _ => Some(ClaudeToolChoice::Auto),
            }
        } else if obj.contains_key("function") {
            let name = obj
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())?;
            Some(ClaudeToolChoice::Tool {
                name: name.to_string(),
            })
        } else {
            Some(ClaudeToolChoice::Auto)
        }
    } else {
        Some(ClaudeToolChoice::Auto)
    }
}

fn adjust_max_tokens(body: &serde_json::Map<String, Value>) -> u32 {
    let mut max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_TOKENS as u64) as u32;

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        if !tools.is_empty() && max_tokens < DEFAULT_MIN_TOKENS {
            max_tokens = DEFAULT_MIN_TOKENS;
        }
    }

    if let Some(thinking) = body.get("thinking") {
        if let Some(budget_tokens) = thinking.get("budget_tokens").and_then(|v| v.as_u64()) {
            if max_tokens <= budget_tokens as u32 {
                max_tokens = budget_tokens as u32 + 1024;
            }
        }
    }

    max_tokens
}

pub fn openai_to_claude_request(
    model: &str,
    body: &mut Value,
    stream: bool,
    _credentials: Option<&Value>,
) -> bool {
    let Some(body_obj) = body.as_object_mut() else {
        return false;
    };

    let mut tool_name_map = ToolNameMap::new();
    let max_tokens = adjust_max_tokens(body_obj);

    let mut result_messages: Vec<ClaudeMessage> = Vec::new();
    let mut system_parts: Vec<String> = Vec::new();

    if let Some(messages) = body_obj.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|v| v.as_str()) == Some("system") {
                let content = msg.get("content");
                let text = match content {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => extract_text_content(v),
                    None => String::new(),
                };
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
        }

        let non_system_messages: Vec<&Value> = messages
            .iter()
            .filter(|m| m.get("role").and_then(|v| v.as_str()) != Some("system"))
            .collect();

        let mut current_role: Option<&str> = None;
        let mut current_parts: Vec<ClaudeContentBlock> = Vec::new();

        let flush_current_message =
            |current_role: &mut Option<&str>,
             current_parts: &mut Vec<ClaudeContentBlock>,
             result_messages: &mut Vec<ClaudeMessage>| {
                if let Some(role) = current_role.take() {
                    if !current_parts.is_empty() {
                        result_messages.push(ClaudeMessage {
                            role: role.to_string(),
                            content: std::mem::take(current_parts),
                        });
                    }
                }
            };

        for msg in non_system_messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let new_role = if role == "user" || role == "tool" {
                "user"
            } else {
                "assistant"
            };

            let blocks = get_content_blocks_from_message(msg, &mut tool_name_map);
            let has_tool_use = blocks
                .iter()
                .any(|b| matches!(b, ClaudeContentBlock::ToolUse { .. }));
            let has_tool_result = blocks
                .iter()
                .any(|b| matches!(b, ClaudeContentBlock::ToolResult { .. }));

            if has_tool_result {
                let tool_result_blocks: Vec<ClaudeContentBlock> = blocks
                    .iter()
                    .filter(|b| matches!(b, ClaudeContentBlock::ToolResult { .. }))
                    .cloned()
                    .collect();
                let other_blocks: Vec<ClaudeContentBlock> = blocks
                    .iter()
                    .filter(|b| !matches!(b, ClaudeContentBlock::ToolResult { .. }))
                    .cloned()
                    .collect();

                flush_current_message(&mut current_role, &mut current_parts, &mut result_messages);

                if !tool_result_blocks.is_empty() {
                    result_messages.push(ClaudeMessage {
                        role: "user".to_string(),
                        content: tool_result_blocks,
                    });
                }

                if !other_blocks.is_empty() {
                    current_role = Some(new_role);
                    current_parts.extend(other_blocks);
                }
                continue;
            }

            if current_role.map(|r| r != new_role).unwrap_or(true) {
                flush_current_message(&mut current_role, &mut current_parts, &mut result_messages);
                current_role = Some(new_role);
            }

            current_parts.extend(blocks);

            if has_tool_use {
                flush_current_message(&mut current_role, &mut current_parts, &mut result_messages);
            }
        }

        flush_current_message(&mut current_role, &mut current_parts, &mut result_messages);

        for i in (0..result_messages.len()).rev() {
            let msg = &result_messages[i];
            if msg.role == "assistant" && !msg.content.is_empty() {
                let valid_types: [&str; 4] = ["text", "tool_use", "tool_result", "image"];
                let valid_indices: Vec<usize> = msg
                    .content
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, block)| {
                        let block_type = match block {
                            ClaudeContentBlock::Text { .. } => "text",
                            ClaudeContentBlock::ToolUse { .. } => "tool_use",
                            ClaudeContentBlock::ToolResult { .. } => "tool_result",
                            ClaudeContentBlock::Image { .. } => "image",
                            ClaudeContentBlock::Thinking { .. } => return None,
                        };
                        if valid_types.contains(&block_type) {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .collect();

                if let Some(&j) = valid_indices.last() {
                    let new_block = match &result_messages[i].content[j] {
                        ClaudeContentBlock::Text { text } => {
                            ClaudeContentBlock::Text { text: text.clone() }
                        }
                        ClaudeContentBlock::Image { source } => ClaudeContentBlock::Image {
                            source: source.clone(),
                        },
                        ClaudeContentBlock::ToolUse { id, name, input } => {
                            ClaudeContentBlock::ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            }
                        }
                        ClaudeContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => ClaudeContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: content.clone(),
                            is_error: *is_error,
                        },
                        ClaudeContentBlock::Thinking {
                            thinking,
                            cache_control: _,
                        } => ClaudeContentBlock::Thinking {
                            thinking: thinking.clone(),
                            cache_control: Some(CacheControl {
                                cache_type: "ephemeral".to_string(),
                                ttl: None,
                            }),
                        },
                    };
                    result_messages[i].content[j] = new_block;
                }
                break;
            }
        }
    }

    let mut result_system: Vec<ClaudeSystemBlock> = vec![ClaudeSystemBlock::Text {
        text: CLAUDE_SYSTEM_PROMPT.to_string(),
        cache_control: None,
    }];

    if !system_parts.is_empty() {
        let system_text = system_parts.join("\n");
        result_system.push(ClaudeSystemBlock::Text {
            text: system_text,
            cache_control: Some(CacheControl {
                cache_type: "ephemeral".to_string(),
                ttl: Some("1h".to_string()),
            }),
        });
    }

    let mut result_tools: Vec<ClaudeTool> = Vec::new();
    if let Some(tools) = body_obj.get("tools").and_then(|v| v.as_array()) {
        for tool in tools {
            let tool_type = tool
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("function");

            if tool_type != "function" {
                if let Some(t_obj) = tool.as_object() {
                    result_tools.push(ClaudeTool {
                        name: t_obj
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        description: t_obj
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        input_schema: t_obj
                            .get("parameters")
                            .or_else(|| t_obj.get("input_schema"))
                            .cloned()
                            .unwrap_or(Value::Object(serde_json::Map::new())),
                        cache_control: None,
                    });
                }
                continue;
            }

            let func = tool.get("function");
            let original_name = func
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();

            let tool_name = original_name.clone();
            tool_name_map.insert(tool_name.clone(), original_name);

            let description = func
                .and_then(|f| f.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = func
                .and_then(|f| f.get("parameters"))
                .or_else(|| func.and_then(|f| f.get("input_schema")))
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));

            result_tools.push(ClaudeTool {
                name: tool_name,
                description,
                input_schema,
                cache_control: None,
            });
        }

        if let Some(last_tool) = result_tools.last_mut() {
            last_tool.cache_control = Some(CacheControl {
                cache_type: "ephemeral".to_string(),
                ttl: Some("1h".to_string()),
            });
        }
    }

    let tool_choice = body_obj
        .get("tool_choice")
        .and_then(|v| convert_openai_tool_choice(v));

    let mut reasoning_effort_thinking: Option<ClaudeThinking> = None;
    if body_obj.get("reasoning_effort").is_some() && body_obj.get("thinking").is_none() {
        if let Some(effort) = body_obj.get("reasoning_effort").and_then(|v| v.as_str()) {
            let effort_lower = effort.to_lowercase();
            let budget = match effort_lower.as_str() {
                "none" => Some(0u32),
                "low" => Some(4096u32),
                "medium" => Some(8192u32),
                "high" => Some(16384u32),
                "xhigh" => Some(32768u32),
                _ => None,
            };
            if let Some(b) = budget {
                if b > 0 {
                    reasoning_effort_thinking = Some(ClaudeThinking {
                        thinking_type: "enabled".to_string(),
                        budget_tokens: Some(b),
                        max_tokens: None,
                    });
                }
            }
        }
    }

    let thinking = body_obj.get("thinking").map(|t| {
        let thinking_type = t
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("enabled")
            .to_string();
        let budget_tokens = t
            .get("budget_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        let max_tokens = t
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);
        ClaudeThinking {
            thinking_type,
            budget_tokens,
            max_tokens,
        }
    });

    let mut result_obj = serde_json::Map::new();
    result_obj.insert("model".into(), Value::String(model.to_string()));
    result_obj.insert("max_tokens".into(), Value::Number(max_tokens.into()));
    result_obj.insert("stream".into(), Value::Bool(stream));
    result_obj.insert(
        "messages".into(),
        serde_json::to_value(&result_messages).unwrap(),
    );
    result_obj.insert(
        "system".into(),
        serde_json::to_value(&result_system).unwrap(),
    );

    if !result_tools.is_empty() {
        result_obj.insert("tools".into(), serde_json::to_value(&result_tools).unwrap());
    }

    if let Some(tc) = tool_choice {
        result_obj.insert("tool_choice".into(), serde_json::to_value(&tc).unwrap());
    }

    if let Some(th) = thinking {
        result_obj.insert("thinking".into(), serde_json::to_value(&th).unwrap());
    } else if let Some(rt) = reasoning_effort_thinking {
        result_obj.insert("thinking".into(), serde_json::to_value(&rt).unwrap());
    }

    if !tool_name_map.is_empty() {
        result_obj.insert(
            "_toolNameMap".into(),
            serde_json::to_value(&tool_name_map).unwrap(),
        );
    }

    if let Some(resp_format) = body_obj.get("response_format") {
        if let Some(fmt_obj) = resp_format.as_object() {
            let fmt_type = fmt_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

            if fmt_type == "json_schema" {
                if let Some(schema) = fmt_obj.get("json_schema").and_then(|s| s.get("schema")) {
                    let schema_json = serde_json::to_string_pretty(schema).unwrap_or_default();
                    let hint = format!(
                        "You must respond with valid JSON that strictly follows this JSON schema:\n```json\n{}\n```\nRespond ONLY with the JSON object, no other text.",
                        schema_json
                    );
                    if let Some(last) = result_system.last_mut() {
                        if let ClaudeSystemBlock::Text { text, .. } = last {
                            text.push_str(&format!("\n\n{}", hint));
                        }
                    }
                }
            } else if fmt_type == "json_object" {
                let hint = "You must respond with valid JSON. Respond ONLY with a JSON object, no other text.";
                if let Some(last) = result_system.last_mut() {
                    if let ClaudeSystemBlock::Text { text, .. } = last {
                        text.push_str(&format!("\n\n{}", hint));
                    }
                }
            }
        }
    }

    if let Some(temp) = body_obj.get("temperature") {
        result_obj.insert("temperature".into(), temp.clone());
    }

    *body = Value::Object(result_obj);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_message_translation() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        }"#,
        )
        .unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].get("role").unwrap().as_str().unwrap(), "user");
    }

    #[test]
    fn test_system_message_handling() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ]
        }"#,
        )
        .unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let system = body.get("system").unwrap().as_array().unwrap();
        assert!(system.len() >= 1);
        assert!(system[0]
            .get("text")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("Claude Code"));
    }

    #[test]
    fn test_tool_message_flow() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Use the tool"},
                {"role": "assistant", "content": "I'll use the tool", "tool_calls": [{"type": "function", "id": "call_abc", "function": {"name": "test_tool", "arguments": "{\"arg\": 1}"}}]},
                {"role": "tool", "tool_call_id": "call_abc", "content": "tool result"}
            ]
        }"#).unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        assert!(!messages.is_empty());
    }

    #[test]
    fn test_tool_declaration_conversion() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "my_tool",
                        "description": "A test tool",
                        "parameters": {"type": "object", "properties": {"arg": {"type": "string"}}}
                    }
                }
            ]
        }"#,
        )
        .unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let tools = body.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].get("name").unwrap().as_str().unwrap(), "my_tool");
        assert!(tools[0].get("cache_control").is_some());
    }

    #[test]
    fn test_image_content() {
        let mut body: Value = serde_json::from_str(r#"{
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": [{"type": "image_url", "image_url": {"url": "https://example.com/image.png"}}]}
            ]
        }"#).unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let messages = body.get("messages").unwrap().as_array().unwrap();
        let content = messages[0].get("content").unwrap().as_array().unwrap();
        assert!(content
            .iter()
            .any(|b| b.get("type").unwrap().as_str().unwrap() == "image"));
    }

    #[test]
    fn test_thinking_config() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "thinking": {"type": "enabled", "budget_tokens": 10000}
        }"#,
        )
        .unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let thinking = body.get("thinking").unwrap();
        assert_eq!(thinking.get("type").unwrap().as_str().unwrap(), "enabled");
        assert_eq!(
            thinking.get("budget_tokens").unwrap().as_u64().unwrap(),
            10000
        );
    }

    #[test]
    fn test_reasoning_effort_to_thinking() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "reasoning_effort": "high"
        }"#,
        )
        .unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let thinking = body.get("thinking").unwrap();
        assert_eq!(
            thinking.get("budget_tokens").unwrap().as_u64().unwrap(),
            16384
        );
    }

    #[test]
    fn test_tool_choice_conversion() {
        let mut body: Value = serde_json::from_str(
            r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "tool_choice": {"type": "function", "function": {"name": "my_tool"}}
        }"#,
        )
        .unwrap();

        openai_to_claude_request("claude-3", &mut body, false, None);

        let tool_choice = body.get("tool_choice").unwrap();
        assert_eq!(tool_choice.get("type").unwrap().as_str().unwrap(), "tool");
        assert_eq!(
            tool_choice.get("name").unwrap().as_str().unwrap(),
            "my_tool"
        );
    }
}
