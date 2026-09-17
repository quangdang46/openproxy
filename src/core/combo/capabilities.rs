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
        // OpenCode Free Muse Spark — multimodal (text+image) via OpenAI
        // Responses input_image; reasoning up to xhigh. 9router acb5c34c.
        (
            "muse-spark-1.2-contributor-free",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "muse-spark-1.3-contributor-free",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 131072 }),
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
        // OpenCode Free Muse Spark (multimodal text+image per models.dev
        // meta/muse-spark, via OpenAI Responses input_image; reasoning up to
        // xhigh). 9router acb5c34c.
        (
            "*muse*spark*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 131072 }),
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
    refine_capabilities(
        hand_capabilities_for_model(provider, model),
        provider,
        model,
    )
}

/// Hand-written table lookup only (provider override → exact → pattern →
/// floor), WITHOUT the synced-catalog overlay or the vision heuristic.
///
/// This is the e6f5724b self-erasure guard: `collect_hand_baseline()` in
/// `catalog_overlay` must measure upstream deltas against the hand-written
/// tables ALONE. Going through `get_capabilities_for_model` (which applies
/// `refine()`, reading the synced file) would measure each delta against
/// the previous sync's output, so a value that still agrees with upstream
/// looks like "no change" and is dropped — the file erases itself over two
/// runs (JS observed `providers` 20→5). Mirrors `setCatalogSource(null)` +
/// restore-in-`finally` in sync.js; no restore needed here since the static
/// tables are never mutated.
pub fn hand_capabilities_for_model(provider: &str, model: &str) -> ModelCapabilities {
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

/// Apply the synced models.dev catalog overlay + the vision name heuristic
/// on top of a table-resolved result. Strictly additive: a capability
/// already true stays true, and a false one only flips when an outside
/// source positively declares support. 9router `capabilities.js refine()`
/// (0532f00d).
fn refine_capabilities(
    mut caps: ModelCapabilities,
    provider: &str,
    model: &str,
) -> ModelCapabilities {
    if let Some(modalities) = crate::core::model::catalog_overlay::catalog_modalities(model) {
        if modalities.vision {
            caps.vision = true;
        }
        if modalities.pdf {
            caps.pdf = true;
        }
        if modalities.audio_input {
            caps.audio_input = true;
        }
        if modalities.video_input {
            caps.video_input = true;
        }
    }
    if let Some(limits) = crate::core::model::catalog_overlay::catalog_limits(provider, model) {
        if limits.context_window > 0 {
            caps.context_window = limits.context_window;
        }
        if limits.max_output > 0 {
            caps.max_output = limits.max_output;
        }
    }
    if !caps.vision && looks_like_vision_model(model) {
        caps.vision = true;
    }
    caps
}

/// Name-based vision detection — last resort when neither the catalog file
/// nor the capability tables know a model. Vendors put the modality in the
/// id ("qwen3-vl-plus", "glm-4.6v", "deepseek-v4-flash-vision-exp"), so a
/// custom or freshly released model still gets image input instead of
/// silently dropping it. Only ever turns vision ON.
/// 9router `visionPatterns.js looksLikeVisionModel` (0532f00d).
fn looks_like_vision_model(model_id: &str) -> bool {
    if model_id.is_empty() {
        return false;
    }
    let id = model_id.to_lowercase();
    const SEP: &[char] = &['-', '_', '/', ':', '.'];
    // Split into separator-delimited segments once; most block terms only
    // match whole segments (mirrors the JS `(^|SEP)…(SEP|$)` anchoring).
    let segments: Vec<&str> = id.split(SEP).collect();
    let has_segment = |word: &str| segments.iter().any(|s| *s == word);
    // Image GENERATION, video generation, and non-chat models also carry
    // these words but take no image input — checked first so they never match.
    // 9router `visionPatterns.js NOT_VISION`: `(^|SEP)(image|img)(SEP|$)`,
    // `stable-image`, `gen[0-9]_image`, `nanobanana`, `imagine`, `t2v`,
    // `i2v`, `flux`, `dall`, `sdxl`, `diffusion`, `embed`, `rerank`,
    // `guard`, `moderation`, `tts`, `stt`, `whisper`, `voice`, `speech`,
    // `audio` (unanchored alternation tail in the JS regex).
    //
    // NOTE: the JS alternation mixes anchored heads (`image`, `img`,
    // `stable-image`, `gen[0-9]_image`, …) with an unanchored tail
    // (`embed`, `tts`, …). The port keeps that exact shape: segment match
    // for the anchored heads, substring for the tail. A former revision
    // blocked any id containing `"gen"` as a substring — overbroad
    // (fail-closed: `gen-vision-1`, `*-agentic` + a vision word, and
    // `imageslider-vl` were denied vision the JS grants). Only
    // `gen[0-9]_image` blocks now.
    if has_segment("image") || has_segment("img") || id.contains("stable-image") {
        return false;
    }
    if segments.iter().any(|seg| {
        // `gen[0-9]_image`: split on `_`/`-`/… turns `gen0_image` into
        // ["gen0", "image"] — match per-segment instead.
        let b = seg.as_bytes();
        b.len() >= 4
            && b[0] == b'g'
            && b[1] == b'e'
            && b[2] == b'n'
            && b[3].is_ascii_digit()
            && seg[4..]
                .split('-')
                .any(|p| p == "image" || p.starts_with("image"))
            || *seg == "image"
    }) {
        return false;
    }
    for w in [
        "nanobanana",
        "imagine",
        "t2v",
        "i2v",
        "flux",
        "dall",
        "sdxl",
        "diffusion",
        "embed",
        "rerank",
        "guard",
        "moderation",
        "tts",
        "stt",
        "whisper",
        "voice",
        "speech",
        "audio",
    ] {
        if id.contains(w) {
            return false;
        }
    }
    // Explicit modality words, plus the "<digit>v" suffix vendors use for
    // vision variants (glm-4.6v, glm-5v-turbo). The digit-v branch requires
    // a dotted version so the never-shipped `gpt-4v` cannot match.
    // (SEP/segments already defined above.)
    let sep_or_edge = |pos: usize, len: usize| pos == 0 || pos + len == id.len();
    let has_word = |word: &str| {
        id.split(SEP).any(|seg| seg == word)
            || (id.contains(word) && (sep_or_edge(id.find(word).unwrap_or(0), word.len())))
    };
    for word in ["vision", "vl", "vlm", "multimodal", "omni", "visual"] {
        if has_word(word) {
            return true;
        }
    }
    // `[0-9]\.[0-9]+v(SEP|$)` — dotted-version digit-v.
    let bytes = id.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let mut j = i + 1;
            if j < bytes.len() && bytes[j] == b'.' {
                j += 1;
                let digits_start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > digits_start
                    && j < bytes.len()
                    && (bytes[j] == b'v' || bytes[j] == b'V')
                    && (j + 1 == bytes.len() || SEP.contains(&(bytes[j + 1] as char)))
                {
                    return true;
                }
            }
        }
        i += 1;
    }
    // `(^|SEP)glm-[0-9]+v(SEP|$)` — GLM vision variants (glm-4.6v).
    // Note `glm-4.6v` splits into "glm" + "4.6v" on separators, so match on
    // the raw id: `glm-` + digits/dots + `v` + separator-or-end.
    if let Some(pos) = id.find("glm-") {
        let after = &id[pos + 4..];
        let mut chars = after.chars().peekable();
        let mut saw_digit = false;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                saw_digit = true;
                chars.next();
            } else if c == '.' && saw_digit {
                chars.next();
            } else {
                break;
            }
        }
        if saw_digit {
            if let Some(&'v') = chars.peek() {
                chars.next();
                if chars.peek().is_none_or(|&c| SEP.contains(&c)) {
                    return true;
                }
            }
        }
    }
    // Known open vision-model families.
    for fam in [
        "llava",
        "pixtral",
        "internvl",
        "cogvlm",
        "minicpm-v",
        "moondream",
        "idefics",
        "fuyu",
    ] {
        if id.contains(fam) {
            return true;
        }
    }
    false
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
