//! Model capabilities — port of 9router `open-sse/providers/capabilities.js`.
//!
//! Fallback order (first match wins), result merged over DEFAULT:
//!   1. PROVIDER_CAPABILITIES[provider][model]  — provider-specific override
//!   2. MODEL_CAPABILITIES[model]               — canonical exact id
//!   3. PATTERN_CAPABILITIES                    — glob, ordered specific → generic
//!   4. DEFAULT_CAPABILITIES                    — safe floor
//!
//! Pattern semantics match JS matchPattern: case-insensitive, `*` = wildcard,
//! anchored to the full model id.
//!
//! Used by combo reordering: hard capabilities (vision/pdf/audioInput/
//! videoInput) MUST be satisfied; soft ones only rank.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Capability keys that gate model selection (JS HARD_CAPS).
pub const HARD_CAPS: &[&str] = &["vision", "pdf", "audioInput", "videoInput"];

/// The safe floor every resolved result is merged over (JS DEFAULT_CAPABILITIES).
#[derive(Debug, Clone)]
pub struct ModelCapabilities {
    pub vision: bool,
    pub pdf: bool,
    pub audio_input: bool,
    pub video_input: bool,
    pub image_output: bool,
    pub audio_output: bool,
    pub search: bool,
    pub tools: bool,
    pub reasoning: bool,
    /// JS thinkingFormat: openai | claude-adaptive | claude-budget |
    /// gemini-level | gemini-budget | zai | qwen | deepseek | kimi | minimax
    /// | hunyuan | step — None derives from transport format.
    pub thinking_format: Option<&'static str>,
    pub thinking_can_disable: bool,
    /// { min, max } budget clamp for budget formats.
    pub thinking_range: Option<(i64, i64)>,
    pub context_window: u64,
    pub max_output: u64,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            vision: false,
            pdf: false,
            audio_input: false,
            video_input: false,
            image_output: false,
            audio_output: false,
            search: false,
            tools: true,
            reasoning: false,
            thinking_format: None,
            thinking_can_disable: true,
            thinking_range: None,
            context_window: 200_000,
            max_output: 64_000,
        }
    }
}

impl ModelCapabilities {
    fn from_value(v: &Value) -> Self {
        let mut caps = Self::default();
        let Some(obj) = v.as_object() else {
            return caps;
        };
        if let Some(b) = obj.get("vision").and_then(Value::as_bool) {
            caps.vision = b;
        }
        if let Some(b) = obj.get("pdf").and_then(Value::as_bool) {
            caps.pdf = b;
        }
        if let Some(b) = obj.get("audioInput").and_then(Value::as_bool) {
            caps.audio_input = b;
        }
        if let Some(b) = obj.get("videoInput").and_then(Value::as_bool) {
            caps.video_input = b;
        }
        if let Some(b) = obj.get("imageOutput").and_then(Value::as_bool) {
            caps.image_output = b;
        }
        if let Some(b) = obj.get("audioOutput").and_then(Value::as_bool) {
            caps.audio_output = b;
        }
        if let Some(b) = obj.get("search").and_then(Value::as_bool) {
            caps.search = b;
        }
        if let Some(b) = obj.get("tools").and_then(Value::as_bool) {
            caps.tools = b;
        }
        if let Some(b) = obj.get("reasoning").and_then(Value::as_bool) {
            caps.reasoning = b;
        }
        if let Some(f) = obj.get("thinkingFormat").and_then(Value::as_str) {
            // Leak is fine for the static table; runtime strings are not stored.
            caps.thinking_format = match f {
                "openai" => Some("openai"),
                "claude-adaptive" => Some("claude-adaptive"),
                "claude-budget" => Some("claude-budget"),
                "gemini-level" => Some("gemini-level"),
                "gemini-budget" => Some("gemini-budget"),
                "zai" => Some("zai"),
                "qwen" => Some("qwen"),
                "deepseek" => Some("deepseek"),
                "kimi" => Some("kimi"),
                "minimax" => Some("minimax"),
                "hunyuan" => Some("hunyuan"),
                "step" => Some("step"),
                _ => None,
            };
        } else if obj.contains_key("thinkingFormat")
            && obj.get("thinkingFormat") == Some(&Value::Null)
        {
            caps.thinking_format = None;
        }
        if let Some(b) = obj.get("thinkingCanDisable").and_then(Value::as_bool) {
            caps.thinking_can_disable = b;
        }
        if let Some(range) = obj.get("thinkingRange") {
            caps.thinking_range = match range {
                Value::Object(o) => Some((
                    o.get("min").and_then(Value::as_i64).unwrap_or(0),
                    o.get("max").and_then(Value::as_i64).unwrap_or(0),
                )),
                _ => None,
            };
        }
        if let Some(n) = obj.get("contextWindow").and_then(Value::as_u64) {
            caps.context_window = n;
        }
        if let Some(n) = obj.get("maxOutput").and_then(Value::as_u64) {
            caps.max_output = n;
        }
        caps
    }

    /// Whether this capability set satisfies one capability name.
    pub fn has(&self, cap: &str) -> bool {
        match cap {
            "vision" => self.vision,
            "pdf" => self.pdf,
            "audioInput" => self.audio_input,
            "videoInput" => self.video_input,
            "imageOutput" => self.image_output,
            "audioOutput" => self.audio_output,
            "search" => self.search,
            "tools" => self.tools,
            "reasoning" => self.reasoning,
            _ => false,
        }
    }
}

static PROVIDER_CAPABILITIES: LazyLock<HashMap<&'static str, HashMap<&'static str, Value>>> =
    LazyLock::new(|| {
        let mut table: HashMap<&'static str, HashMap<&'static str, Value>> = HashMap::new();
        table.insert("nvidia", HashMap::from([
            ("minimaxai/minimax-m2.7", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 131072 })),
            ("minimaxai/minimax-m3", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 512000, "maxOutput": 131072 })),
            ("z-ai/glm-5.2", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 128000 })),
            ("deepseek-ai/deepseek-v4-pro", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 65536 })),
            ("deepseek-ai/deepseek-v4-flash", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 65536 })),
        ]));
        table.insert("codex", HashMap::from([
            ("gpt-5.6-sol", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 372000, "maxOutput": 128000 })),
            ("gpt-5.6-sol-review", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 372000, "maxOutput": 128000 })),
            ("gpt-5.6-terra", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-terra-review", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-luna", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-luna-review", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
        ]));
        table.insert("kiro", HashMap::from([
            ("gpt-5.6-sol", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-sol-thinking", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-sol-agentic", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-sol-thinking-agentic", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-terra", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-terra-thinking", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-terra-agentic", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-terra-thinking-agentic", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-luna", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-luna-thinking", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-luna-agentic", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-5.6-luna-thinking-agentic", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
        ]));
        table.insert("codebuddy-cn", HashMap::from([
            ("glm-5.2", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 48000 })),
            ("glm-5.1", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 48000 })),
            ("glm-5.0", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 48000 })),
            ("glm-5.0-turbo", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 48000 })),
            ("glm-5v-turbo", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 38000 })),
            ("glm-4.7", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 48000 })),
            ("minimax-m3", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 512000, "maxOutput": 48000 })),
            ("minimax-m2.7", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 48000 })),
            ("kimi-k2.7", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 256000, "maxOutput": 32000 })),
            ("kimi-k2.6", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 256000, "maxOutput": 32000 })),
            ("kimi-k2.5", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 164000, "maxOutput": 32000 })),
            ("hy3-preview", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 192000, "maxOutput": 64000 })),
            ("deepseek-v4-pro", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 50000 })),
            ("deepseek-v4-flash", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 50000 })),
            ("deepseek-v3-2-volc", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 96000, "maxOutput": 32000 })),
        ]));
        table.insert("poolside", HashMap::from([
            ("laguna-s-2.1", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 32000 })),
            ("laguna-xs-2.1", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 32000 })),
        ]));
        table
    });

static MODEL_CAPABILITIES: LazyLock<HashMap<&'static str, Value>> = LazyLock::new(|| {
    HashMap::from([
        (
            "claude-opus-5",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-5-thinking",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-5-agentic",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-5-thinking-agentic",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.6",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.7",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-7",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.8",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-6",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-8",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.8-thinking",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-8-thinking",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-4.6",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-4-6",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5-thinking",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5-agentic",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5-thinking-agentic",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "gpt-image-1",
            serde_json::json!({ "imageOutput": true, "tools": false }),
        ),
        (
            "glm-4.6v",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "zai", "contextWindow": 128000 }),
        ),
        (
            "vision-model",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000 }),
        ),
        (
            "coder-model",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000 }),
        ),
        (
            "kimi-k3",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "k3",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "kimi-for-coding",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 65536 }),
        ),
        (
            "kimi-for-coding-highspeed",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 65536 }),
        ),
        (
            "kimi-k2.7-code",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 65536 }),
        ),
        (
            "kimi-k2.7-code-highspeed",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 65536 }),
        ),
    ])
});

static PATTERN_CAPABILITIES: LazyLock<Vec<(&'static str, Value)>> = LazyLock::new(|| {
    vec![
        (
            "*claude*opus-5*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*claude*opus-4.6*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*opus-4.7*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*opus-4.8*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*sonnet-4.6*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*sonnet-4.7*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*haiku*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*claude*opus*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*claude*sonnet*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*claude*fable*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*claude*mythos*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        ("*claude-3*", serde_json::json!({ "vision": true })),
        (
            "*claude*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*gemini*image*",
            serde_json::json!({ "vision": true, "imageOutput": true, "contextWindow": 1048576 }),
        ),
        (
            "*gemini-3.7*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "search": true, "thinkingFormat": "gemini-level", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 65536 }),
        ),
        (
            "*gemini-3*pro*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "search": true, "thinkingFormat": "gemini-level", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 65535 }),
        ),
        (
            "*gemini-3*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "search": true, "thinkingFormat": "gemini-level", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 65536 }),
        ),
        (
            "*gemini-2*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "search": true, "contextWindow": 1048576, "maxOutput": 65536 }),
        ),
        (
            "*gemini*",
            serde_json::json!({ "vision": true, "search": true, "contextWindow": 1048576 }),
        ),
        (
            "*gemma*",
            serde_json::json!({ "vision": true, "contextWindow": 128000 }),
        ),
        (
            "*nanobanana*",
            serde_json::json!({ "vision": true, "imageOutput": true }),
        ),
        ("*gpt-5*image*", serde_json::json!({ "imageOutput": true })),
        (
            "*gpt-5*codex*",
            serde_json::json!({ "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 400000, "maxOutput": 128000 }),
        ),
        (
            "*gpt-5*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 400000, "maxOutput": 128000 }),
        ),
        (
            "*gpt-4o*",
            serde_json::json!({ "vision": true, "search": true, "contextWindow": 128000, "maxOutput": 16384 }),
        ),
        (
            "*gpt-4.1*",
            serde_json::json!({ "vision": true, "contextWindow": 1000000, "maxOutput": 32768 }),
        ),
        (
            "*gpt-4-turbo*",
            serde_json::json!({ "vision": true, "contextWindow": 128000 }),
        ),
        ("*gpt-4*", serde_json::json!({ "contextWindow": 128000 })),
        (
            "*gpt-3.5*",
            serde_json::json!({ "contextWindow": 16385, "maxOutput": 4096 }),
        ),
        (
            "*gpt-oss*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 128000 }),
        ),
        (
            "*o1-mini*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 128000 }),
        ),
        (
            "*o1*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 100000 }),
        ),
        (
            "*o3*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 100000 }),
        ),
        (
            "*o4*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 100000 }),
        ),
        ("*grok*image*", serde_json::json!({ "imageOutput": true })),
        (
            "*grok-code*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 256000 }),
        ),
        (
            "*grok-4.5*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 500000, "maxOutput": 64000 }),
        ),
        (
            "*grok-4*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 256000 }),
        ),
        (
            "*grok-3*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 131072 }),
        ),
        (
            "*grok*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 256000 }),
        ),
        (
            "*qwen*vl*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 262144 }),
        ),
        (
            "*qwen*omni*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 262144, "maxOutput": 65536 }),
        ),
        (
            "*qwen*coder*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000 }),
        ),
        (
            "*qwen*max*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000, "maxOutput": 65536 }),
        ),
        (
            "*qwen3.5*",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000, "maxOutput": 65536 }),
        ),
        (
            "*qwen3.6*",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000, "maxOutput": 65536 }),
        ),
        (
            "*qwen3.7*",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000, "maxOutput": 65536 }),
        ),
        (
            "*qwen*plus*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000, "maxOutput": 65536 }),
        ),
        (
            "*qwen*235b*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 262144 }),
        ),
        (
            "*qwq*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "qwen", "thinkingCanDisable": false, "contextWindow": 131072 }),
        ),
        (
            "*qwen*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 262144 }),
        ),
        (
            "*kimi*k3*",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "*kimi*for-coding*",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 65536 }),
        ),
        (
            "*kimi*k2.7*code*",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 65536 }),
        ),
        (
            "*kimi*k2*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "kimi", "contextWindow": 262144, "maxOutput": 262144 }),
        ),
        (
            "*kimi*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "kimi", "contextWindow": 262144 }),
        ),
        (
            "*glm-5*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "contextWindow": 200000, "maxOutput": 128000 }),
        ),
        (
            "*glm-4.7*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "contextWindow": 200000, "maxOutput": 128000 }),
        ),
        (
            "*glm-4*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "contextWindow": 200000 }),
        ),
        (
            "*glm*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "contextWindow": 200000 }),
        ),
        (
            "*deepseek-v4*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 1000000, "maxOutput": 384000 }),
        ),
        (
            "*reasoner*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "deepseek", "thinkingCanDisable": false, "contextWindow": 128000 }),
        ),
        (
            "*deepseek-r*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "deepseek", "thinkingCanDisable": false, "contextWindow": 128000 }),
        ),
        (
            "*deepseek-chat*",
            serde_json::json!({ "contextWindow": 128000 }),
        ),
        (
            "*deepseek*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 128000 }),
        ),
        (
            "*minimax*image*",
            serde_json::json!({ "imageOutput": true }),
        ),
        (
            "*minimax-m3*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "minimax", "contextWindow": 1048576, "maxOutput": 512000 }),
        ),
        (
            "*minimax-m2.7*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "minimax", "thinkingCanDisable": false, "contextWindow": 204800, "maxOutput": 131072 }),
        ),
        (
            "*minimax*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "minimax", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 131072 }),
        ),
        (
            "*mimo*v2.5*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "*mimo*omni*",
            serde_json::json!({ "vision": true, "audioInput": true, "contextWindow": 262144, "maxOutput": 131072 }),
        ),
        (
            "*mimo*",
            serde_json::json!({ "vision": true, "contextWindow": 262144, "maxOutput": 131072 }),
        ),
        (
            "*llama-4*",
            serde_json::json!({ "vision": true, "contextWindow": 1000000 }),
        ),
        ("*llama*", serde_json::json!({ "contextWindow": 128000 })),
        (
            "*codestral*",
            serde_json::json!({ "contextWindow": 256000 }),
        ),
        (
            "*mistral-large*",
            serde_json::json!({ "vision": true, "contextWindow": 256000 }),
        ),
        ("*mistral*", serde_json::json!({ "contextWindow": 128000 })),
        (
            "*command-a-vision*",
            serde_json::json!({ "vision": true, "contextWindow": 128000 }),
        ),
        ("*command*", serde_json::json!({ "contextWindow": 128000 })),
        (
            "*sonar*",
            serde_json::json!({ "search": true, "contextWindow": 128000 }),
        ),
        (
            "*pplx*",
            serde_json::json!({ "search": true, "contextWindow": 128000 }),
        ),
        (
            "*perplexity*",
            serde_json::json!({ "search": true, "contextWindow": 128000 }),
        ),
        (
            "*laguna-s-2.1*free*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 32000 }),
        ),
        (
            "*laguna-s-2.1*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 32000 }),
        ),
        (
            "*laguna*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 32000 }),
        ),
        (
            "*hunyuan*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "hunyuan", "contextWindow": 262144, "maxOutput": 262144 }),
        ),
        (
            "hy3*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "hunyuan", "contextWindow": 262144, "maxOutput": 262144 }),
        ),
        (
            "*step-*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "step", "contextWindow": 128000 }),
        ),
        (
            "*nemotron*",
            serde_json::json!({ "reasoning": true, "contextWindow": 128000 }),
        ),
        (
            "*ling-*",
            serde_json::json!({ "reasoning": true, "contextWindow": 128000 }),
        ),
    ]
});

/// JS matchPattern: `*` wildcards, anchored to the full model id,
/// case-insensitive.
fn match_pattern(pattern: &str, model: &str) -> bool {
    let model_lower = model.to_ascii_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut cursor = 0usize;
    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let part_lower = part.to_ascii_lowercase();
        if idx == 0 {
            if !model_lower.starts_with(part_lower.as_str()) {
                return false;
            }
            cursor = part.len();
        } else if idx == parts.len() - 1 {
            let tail_start = model_lower.len().saturating_sub(part.len());
            if tail_start < cursor || !model_lower[tail_start..].starts_with(part_lower.as_str()) {
                return false;
            }
        } else {
            match model_lower[cursor.min(model_lower.len())..].find(part_lower.as_str()) {
                Some(pos) => cursor += pos + part.len(),
                None => return false,
            }
        }
    }
    true
}

pub fn get_capabilities_for_model(provider: &str, model: &str) -> ModelCapabilities {
    if model.is_empty() {
        return ModelCapabilities::default();
    }
    let base_model = model.rsplit('/').next().unwrap_or(model);

    // 1. Provider-specific override.
    if !provider.is_empty() {
        if let Some(table) = PROVIDER_CAPABILITIES.get(provider) {
            if let Some(entry) = table.get(model).or_else(|| table.get(base_model)) {
                return ModelCapabilities::from_value(entry);
            }
        }
    }

    // 2. Canonical exact.
    if let Some(entry) = MODEL_CAPABILITIES
        .get(base_model)
        .or_else(|| MODEL_CAPABILITIES.get(model))
    {
        return ModelCapabilities::from_value(entry);
    }

    // 3. Pattern (first match wins).
    for (pattern, caps) in PATTERN_CAPABILITIES.iter() {
        if match_pattern(pattern, base_model) || match_pattern(pattern, model) {
            return ModelCapabilities::from_value(caps);
        }
    }

    // 4. Floor.
    ModelCapabilities::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_wildcards_and_case() {
        assert!(match_pattern("*claude*opus*", "anthropic/claude-opus-4.7"));
        assert!(match_pattern("*CLAUDE*", "my-claude-x"));
        assert!(match_pattern("hy3*", "hy3-preview"));
        assert!(!match_pattern("*claude*opus*", "anthropic/claude-sonnet"));
        // Anchored: prefix segments must align from position 0.
        assert!(!match_pattern("claude*", "anthropic/claude"));
    }

    #[test]
    fn exact_model_overrides_win() {
        let caps = get_capabilities_for_model("", "claude-opus-4.7");
        assert_eq!(caps.context_window, 1_000_000);
        assert_eq!(caps.thinking_format, Some("claude-adaptive"));
        // Vendor prefix stripped for canonical lookup.
        let prefixed = get_capabilities_for_model("", "anthropic/claude-opus-4.7");
        assert_eq!(prefixed.context_window, 1_000_000);
    }

    #[test]
    fn provider_override_beats_exact() {
        // codex gpt-5.6-sol has 372k vs generic *gpt-5* 400k — proves the
        // provider table was consulted (different window than the pattern).
        let sol = get_capabilities_for_model("codex", "gpt-5.6-sol");
        assert_eq!(sol.context_window, 372_000);
        let terra = get_capabilities_for_model("codex", "gpt-5.6-terra");
        assert_eq!(terra.context_window, 272_000);
    }

    #[test]
    fn pattern_fallback_and_floor() {
        // Unknown family → floor defaults.
        let unknown = get_capabilities_for_model("", "totally-unknown-model");
        assert_eq!(unknown.context_window, 200_000);
        assert!(!unknown.vision);
        // Known family via pattern.
        let gem = get_capabilities_for_model("", "google/gemini-3-pro");
        assert!(gem.vision);
        assert_eq!(gem.thinking_format, Some("gemini-level"));
    }

    #[test]
    fn reorder_floats_capable_models_to_front() {
        use super::super::reorder_by_capabilities;
        let models = vec![
            "openai/gpt-3.5-turbo".to_string(), // text-only per pattern
            "google/gemini-3-pro".to_string(),  // full multimodal
            "anthropic/claude-3-haiku".to_string(),
        ];
        let mut required = std::collections::HashSet::new();
        required.insert("vision".to_string());
        let ordered = reorder_by_capabilities(&models, &required);
        assert_eq!(ordered[0], "google/gemini-3-pro");
    }
}
