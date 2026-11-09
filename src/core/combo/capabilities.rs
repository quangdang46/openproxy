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
    /// zai format only: model accepts a reasoning_effort level
    /// (GLM-5.2+; older GLM ignores it). JS `thinkingEffortSupported`.
    pub thinking_effort_supported: bool,
    /// JS `thinkingOffType`: alternate `thinking.type` written when the
    /// caller requests thinking off (`"between_tools"` for Sonnet 5.5).
    pub thinking_off_type: Option<&'static str>,
    /// JS `forcedToolChoice`: `false` forbids `tool_choice` `any`/`tool`
    /// (Claude Opus/Sonnet 5.5 reject them with 400).
    pub forced_tool_choice: bool,
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
            thinking_effort_supported: false,
            thinking_off_type: None,
            forced_tool_choice: true,
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
                "nous" => Some("nous"),
                "commandcode" => Some("commandcode"),
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
        if let Some(b) = obj.get("thinkingEffortSupported").and_then(Value::as_bool) {
            caps.thinking_effort_supported = b;
        }
        if let Some(f) = obj.get("thinkingOffType").and_then(Value::as_str) {
            // Only "between_tools" is defined upstream; unknown values are
            // ignored rather than stored.
            caps.thinking_off_type = match f {
                "between_tools" => Some("between_tools"),
                _ => None,
            };
        }
        if let Some(b) = obj.get("forcedToolChoice").and_then(Value::as_bool) {
            caps.forced_tool_choice = b;
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
            // Upstream v0.5.112 codex block: per-model Codex OAuth windows
            // (lower than the OpenAI API's 1.05M). gpt-6.1-terra is the only
            // 500k entry — codex-rs/models-manager/models.json reports
            // context_window 500000 for it on Codex.
            ("gpt-6-astra", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-6-sol", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-6.1-sol", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            ("gpt-6.1-terra", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 500000, "maxOutput": 128000 })),
            ("gpt-6-luna", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 272000, "maxOutput": 128000 })),
            // CODEX_EXTENDED_CAPS = CODEX_GPT_56_DEFAULT_CAPS @ 872000.
            ("gpt-6-astra[1m]", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 872000, "maxOutput": 128000 })),
            ("gpt-6-sol[1m]", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 872000, "maxOutput": 128000 })),
            ("gpt-6-luna[1m]", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 872000, "maxOutput": 128000 })),
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
            ("glm-5.2", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": true, "contextWindow": 1000000, "maxOutput": 131072 })),
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
            // GLM-5.3 line on the CN gateway: openai reasoning_effort shape and
            // thinking CAN be disabled here (unlike z.ai's own lines) —
            // upstream PROVIDER_CAPABILITIES["codebuddy-cn"].
            ("glm-5.3", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": true, "contextWindow": 1000000, "maxOutput": 131072 })),
            ("glm-5.3-flash", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": true, "contextWindow": 1000000, "maxOutput": 131072 })),
        ]));
        table.insert("poolside", HashMap::from([
            ("laguna-s-2.1", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 32000 })),
            ("laguna-xs-2.1", serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 32000 })),
        ]));
        // Upstream v0.5.111+ PROVIDER_CAPABILITIES["opencode"] (free lane,
        // /inference/v1): mimo-v2.6-flash-free takes the full OpenAI
        // reasoning_effort enum verbatim; deepseek-v4.1-flash-free's backend
        // rejects requests above ~330k prompt tokens (issue #5097), so the
        // published window is capped at 330000.
        table.insert("opencode", HashMap::from([
            ("mimo-v2.6-flash-free", serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": true, "contextWindow": 1048576, "maxOutput": 131072 })),
            ("deepseek-v4.1-flash-free", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 330000, "maxOutput": 128000 })),
        ]));
        // 9router v0.5.112 PROVIDER_CAPABILITIES["github"] (bead
        // openproxy-4d5h.45): Copilot routes Claude to its Anthropic-native
        // /v1/messages shim, which applies Anthropic's per-model thinking
        // rules — claude-haiku-5.5 is adaptive-only there (budget shape →
        // 400 "thinking.type.enabled" is not supported). Limits match the
        // synced catalog's github entry.
        table.insert("github", HashMap::from([
            ("claude-haiku-5.5", serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 })),
        ]));
        // Upstream PROVIDER_CAPABILITIES["opencode-go"]: the paid lane's
        // glm-5.3-flash backend rejects the z.ai `thinking` object (400
        // unknown field) and wants reasoning_effort; claude-haiku-5-5 is
        // adaptive-only on this gateway.
        table.insert("opencode-go", HashMap::from([
            ("glm-5.3-flash", serde_json::json!({ "vision": true, "videoInput": true, "pdf": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 131072 })),
            ("claude-haiku-5-5", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 })),
        ]));
        // Upstream PROVIDER_CAPABILITIES["commandcode"]: authoritative
        // per-model metadata; every other model falls through to the
        // commandcode default in hand_capabilities_for_model.
        table.insert("commandcode", HashMap::from([
            ("claude-haiku-5-5", serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "commandcode", "thinkingEffortSupported": true, "contextWindow": 1000000, "maxOutput": 128000 })),
        ]));
        // Upstream PROVIDER_CAPABILITIES["devin-cli"] (aliases dv/devin
        // resolve to this id): Devin CLI's registry declares a 200k context
        // window for these GPT variants.
        let devin_cli_gpt = serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 128000 });
        table.insert("devin-cli", HashMap::from([
            ("gpt-5.4-high", devin_cli_gpt.clone()),
            ("gpt-5.4-medium", devin_cli_gpt.clone()),
            ("gpt-5.4-low", devin_cli_gpt.clone()),
            ("gpt-5.5-xhigh", devin_cli_gpt.clone()),
            ("gpt-5.5-high", devin_cli_gpt.clone()),
            ("gpt-5.5-medium", devin_cli_gpt.clone()),
            ("gpt-5.5-low", devin_cli_gpt),
        ]));
        // Upstream PROVIDER_CAPABILITIES["minimax-code"] — MCode credits
        // lane (Anthropic messages on mavis; claude-adaptive effort).
        // minimax-code-global serves the same catalog upstream.
        let minimax_code_caps = |ctx: u64, vision: bool| {
            serde_json::json!({ "vision": vision, "reasoning": true, "thinkingFormat": "claude-adaptive", "thinkingCanDisable": false, "contextWindow": ctx, "maxOutput": 128000 })
        };
        let minimax_code_table = || {
            HashMap::from([
                ("MiniMax-M3.1-Flash-Preview", minimax_code_caps(512000, true)),
                ("MiniMax-M3", minimax_code_caps(512000, true)),
                ("MiniMax-M2.7", minimax_code_caps(200000, false)),
                ("MiniMax-M2.7-highspeed", minimax_code_caps(200000, false)),
            ])
        };
        table.insert("minimax-code", minimax_code_table());
        table.insert("minimax-code-global", minimax_code_table());
        // Upstream PROVIDER_CAPABILITIES["nous"] — free-tier models probed
        // live against inference-api.nousresearch.com.
        table.insert("nous", HashMap::from([
            ("meituan/longcat-2.0:free", serde_json::json!({ "tools": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 1048756, "maxOutput": 262144 })),
            ("meituan/longcat-2.5-preview:free", serde_json::json!({ "tools": true, "vision": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 1048576, "maxOutput": 131072 })),
            ("stepfun/step-5-preview:free", serde_json::json!({ "tools": true, "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 1000000, "maxOutput": 64000 })),
            ("upstage/solar-mini4:free", serde_json::json!({ "tools": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 524288, "maxOutput": 131072 })),
            ("stepfun/step-3.7-flash:free", serde_json::json!({ "tools": true, "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 262144, "maxOutput": 256000 })),
            ("poolside/laguna-s-2.1:free", serde_json::json!({ "tools": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 262144, "maxOutput": 131072 })),
            ("poolside/laguna-xs-2.1:free", serde_json::json!({ "tools": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 262144, "maxOutput": 32768 })),
            ("stealth/missingno", serde_json::json!({ "tools": true, "reasoning": true, "thinkingFormat": "nous", "contextWindow": 262144, "maxOutput": 131072 })),
        ]));
        // Upstream PROVIDER_CAPABILITIES["qoder"] — opaque model ids match
        // no name heuristic, so the table carries vision/reasoning/limits
        // verbatim from /algo/api/v2/model/list. qoder-cn shares it.
        let qoder_caps = |vision: bool, reasoning: bool, ctx: u64, out: u64| {
            serde_json::json!({ "vision": vision, "reasoning": reasoning, "contextWindow": ctx, "maxOutput": out })
        };
        let qoder_table = || {
            HashMap::from([
                // Tier aliases resolve to a variable target upstream; keep
                // them text-only on the default window.
                ("auto", qoder_caps(false, false, 200000, 32768)),
                ("efficient", qoder_caps(false, false, 200000, 32768)),
                ("lite", qoder_caps(false, false, 200000, 32768)),
                ("ultimate", qoder_caps(true, true, 200000, 65536)),
                ("performance", qoder_caps(true, false, 200000, 65536)),
                ("qmodel_38max", qoder_caps(true, true, 200000, 65536)),
                ("qmodel_latest", qoder_caps(true, false, 200000, 65536)),
                ("qmodel", qoder_caps(true, false, 200000, 65536)),
                ("qfmodel", qoder_caps(true, true, 200000, 65536)),
                ("kmodel_latest", qoder_caps(true, false, 200000, 65536)),
                ("kmodel", qoder_caps(true, false, 200000, 65536)),
                ("gmodel", qoder_caps(true, true, 200000, 65536)),
                ("gfmodel", qoder_caps(true, true, 200000, 65536)),
                ("dmodel", qoder_caps(true, true, 200000, 65536)),
                ("dfmodel", qoder_caps(true, true, 200000, 65536)),
                ("mmodel", qoder_caps(true, false, 200000, 65536)),
                // Frontier reasoning pair routed via static model_config.
                ("smodel", qoder_caps(true, true, 180000, 65536)),
                ("cmodel", qoder_caps(true, true, 180000, 65536)),
            ])
        };
        table.insert("qoder", qoder_table());
        table.insert("qoder-cn", qoder_table());
        // Upstream PROVIDER_CAPABILITIES["ollama"] — the generic
        // *deepseek-v4* pattern misses the vision badge + 1M window the
        // library page publishes for the cloud id.
        table.insert("ollama", HashMap::from([
            ("deepseek-v4.1-flash:cloud", serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 1000000, "maxOutput": 384000 })),
        ]));
        table
    });

static MODEL_CAPABILITIES: LazyLock<HashMap<&'static str, Value>> = LazyLock::new(|| {
    HashMap::from([
        // 9router v0.5.112 capabilities.js MODEL_CAPABILITIES — SenseNova
        // 6.8 Flash Lite (bead openproxy-4d5h.40).
        (
            "sensenova-6.8-flash-lite",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        // Claude Fable 5.1 — 1M context + adaptive thinking, thinking cannot
        // be disabled (upstream capabilities.js exact entry).
        (
            "claude-fable-5-1",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        // Claude Opus/Sonnet 5.5 (bead openproxy-4d5h.45): dotted ids serve
        // Kiro preview, GitHub Copilot and Token Harbor; dashed ids are the
        // same models under the level-suffixed spellings. Opus 5.5 rejects
        // thinking.type "disabled" and forced tool_choice (any/tool);
        // Sonnet 5.5 takes "between_tools" in place of "disabled" and also
        // rejects forced tool_choice.
        (
            "claude-opus-5.5",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5.5-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5.5-agentic",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5.5-thinking-agentic",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-sonnet-5.5",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingOffType": "between_tools", "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5-5",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5-5-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5-5-high",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5-5-medium",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5-5-low",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingCanDisable": false, "forcedToolChoice": false }),
        ),
        (
            "claude-sonnet-5-5",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingOffType": "between_tools", "forcedToolChoice": false }),
        ),
        (
            "claude-sonnet-5-5-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingOffType": "between_tools", "forcedToolChoice": false }),
        ),
        (
            "claude-sonnet-5-5-high",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingOffType": "between_tools", "forcedToolChoice": false }),
        ),
        (
            "claude-sonnet-5-5-medium",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingOffType": "between_tools", "forcedToolChoice": false }),
        ),
        (
            "claude-sonnet-5-5-low",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000, "thinkingOffType": "between_tools", "forcedToolChoice": false }),
        ),
        (
            "claude-opus-5",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-5-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-5-agentic",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-5-thinking-agentic",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.6",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.7",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-7",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.8",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-6",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-8",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4.8-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-opus-4-8-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-4.6",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-4-6",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5-thinking",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5-agentic",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "claude-sonnet-5-thinking-agentic",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "gpt-image-1",
            serde_json::json!({ "imageOutput": true, "tools": false }),
        ),
        (
            "glm-4.6v",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "zai", "contextWindow": 128000, "maxOutput": 32768 }),
        ),
        (
            "glm-4.5v",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "zai", "contextWindow": 64000, "maxOutput": 16384 }),
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
        // DeepSeek V4.1-Flash is natively multimodal; upstream renamed the
        // served id — the API only accepts "deepseek-flash" and rejects
        // "deepseek-v4.1-flash" (#4940). "deepseek-flash" keeps the generic
        // 128K/64K limits; the v4.1 ids carry the vision-exp envelope.
        // "deepseek-v4-1-flash" (dash) is the Kenari alias (#4293).
        (
            "deepseek-v4.1-flash",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 1000000, "maxOutput": 384000 }),
        ),
        (
            "deepseek-v4-1-flash",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 1000000, "maxOutput": 384000 }),
        ),
        (
            "deepseek-flash",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 128000, "maxOutput": 64000 }),
        ),
        // DeepSeek's first V4 model with image input; text limits match V4-Flash.
        (
            "deepseek-v4-flash-vision-exp",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "contextWindow": 1000000, "maxOutput": 384000 }),
        ),
        // glm-5.3-flash (z.ai's own line): natively multimodal, 1M window.
        // thinkingCanDisable:false is REQUIRED — the *glm-5.3* pattern also
        // sets it, but the exact entry shadows the pattern and must repeat
        // every field it owns (upstream capabilities.js MODEL_CAPABILITIES).
        (
            "glm-5.3-flash",
            serde_json::json!({ "vision": true, "videoInput": true, "pdf": true, "reasoning": true, "thinkingFormat": "zai", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        // glm-5.3-flash-free (OpenCode free lane): the /inference/v1 backend
        // validates the body strictly — the zai `thinking` object answers 400
        // "unknown field thinking" while reasoning_effort is accepted, so it
        // resolves to the openai shape (upstream capabilities.js exact entry).
        (
            "glm-5.3-flash-free",
            serde_json::json!({ "vision": true, "videoInput": true, "pdf": true, "reasoning": true, "thinkingFormat": "openai", "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        // GLM-5.2 exact entry (upstream capabilities.js MODEL_CAPABILITIES):
        // shadows the *glm-5.2* pattern completely, so it must repeat
        // thinkingEffortSupported or reasoning_effort is dropped; z.ai's 5.2
        // line also rejects thinking.type "disabled".
        (
            "glm-5.2",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "thinkingEffortSupported": true, "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        // Kimi K3.1 Pro (commandcode + opencode-go, upstream 5cd4a756).
        (
            "kimi-k3.1-pro",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "kimi", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        // OpenCode free-tier ids (upstream MODEL_CAPABILITIES, 4c58e7f et al).
        (
            "union-alpha",
            serde_json::json!({ "vision": true, "contextWindow": 262144, "maxOutput": 131072 }),
        ),
        (
            "space-bunny-free",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 524288 }),
        ),
        (
            "space-bunny",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 524288 }),
        ),
        (
            "big-pickle",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 200000, "maxOutput": 32000 }),
        ),
        (
            "exo-free",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "fledge-alpha-free",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "longcat-2.0",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        (
            "longcat-2.0-free",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        (
            "longcat-2.5-preview-free",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        (
            "north-mini-code-free",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 256000, "maxOutput": 64000 }),
        ),
        (
            "ring-2.6-1t-free",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "openai", "contextWindow": 262000, "maxOutput": 66000 }),
        ),
        (
            "x-preview-f-free",
            serde_json::json!({ "vision": true, "videoInput": true, "reasoning": true, "thinkingFormat": "openai", "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
    ])
});

static PATTERN_CAPABILITIES: LazyLock<Vec<(&'static str, Value)>> = LazyLock::new(|| {
    vec![
        (
            "*claude*opus-5*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*claude*sonnet-5*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*claude*opus-4.6*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*opus-4.7*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*opus-4.8*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*sonnet-4.6*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        (
            "*claude*sonnet-4.7*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive" }),
        ),
        // Haiku 5.5 is adaptive-only (upstream v0.5.111 #5183 / v0.5.112 —
        // GitHub Copilot's Anthropic-native /v1/messages shim 400s on the
        // budget shape `*claude*haiku*` sends). Must precede the generic
        // haiku rule; dotted (Copilot `claude-haiku-5.5`) and dashed
        // (OpenCode Go `claude-haiku-5-5`) spellings both covered.
        (
            "*claude*haiku-5.5*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*claude*haiku-5-5*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-adaptive", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        // PDF input is supported by current Claude families. Keep
        // legacy/generic fallbacks conservative instead of inferring PDF
        // support from vision alone (upstream capabilities.js PATTERN_CAPABILITIES,
        // https://platform.claude.com/docs/en/build-with-claude/pdf-support).
        // The versioned budget rules precede the generic family globs so
        // claude-haiku-4-5 / claude-opus-4-1 / claude-sonnet-4-5 get pdf:true
        // while unversioned ids stay conservative.
        (
            "*claude*haiku-5*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*claude*haiku-4*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*claude*opus-4*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
        ),
        (
            "*claude*sonnet-4*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget" }),
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
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget", "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*claude*mythos*",
            serde_json::json!({ "vision": true, "pdf": true, "reasoning": true, "search": true, "thinkingFormat": "claude-budget", "contextWindow": 1000000, "maxOutput": 128000 }),
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
            "*gemini-4*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "search": true, "thinkingFormat": "gemini-level", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 65536 }),
        ),
        (
            "*gemini-3.8*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "search": true, "thinkingFormat": "gemini-level", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 65536 }),
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
            "*gemini-2.5*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "search": true, "thinkingFormat": "gemini-budget", "thinkingRange": { "min": 0, "max": 24576 }, "contextWindow": 1048576, "maxOutput": 65536 }),
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
        ("*gpt-6*image*", serde_json::json!({ "imageOutput": true })),
        // 1.05M is the API window for the whole gpt-6 family; gateway-specific
        // overrides (codex OAuth 272k/372k/500k, kiro 272k) live in
        // PROVIDER_CAPABILITIES and win over this pattern (upstream
        // capabilities.js PATTERN_CAPABILITIES).
        (
            "*gpt-6*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 1050000, "maxOutput": 128000 }),
        ),
        ("*gpt-5*image*", serde_json::json!({ "imageOutput": true })),
        (
            "*gpt-5*codex*",
            serde_json::json!({ "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 400000, "maxOutput": 128000 }),
        ),
        // gpt-5.4 is where the 1.05M window starts, but the mini and nano
        // tiers stayed at 400k — first match wins, so those two have to be
        // listed ahead of it (upstream capabilities.js PATTERN_CAPABILITIES,
        // 89ffac5). Gateway-specific overrides (codex OAuth, kiro) live in
        // PROVIDER_CAPABILITIES and win over these patterns.
        (
            "*gpt-5.4-mini*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 400000, "maxOutput": 128000 }),
        ),
        (
            "*gpt-5.4-nano*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 400000, "maxOutput": 128000 }),
        ),
        (
            "*gpt-5.4*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 1050000, "maxOutput": 128000 }),
        ),
        (
            "*gpt-5.5*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 1050000, "maxOutput": 128000 }),
        ),
        (
            "*gpt-5.6*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 1050000, "maxOutput": 128000 }),
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
            "*grok-4.7*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 500000, "maxOutput": 500000 }),
        ),
        (
            "*grok-4.6*",
            serde_json::json!({ "vision": true, "reasoning": true, "search": true, "thinkingFormat": "openai", "contextWindow": 500000, "maxOutput": 500000 }),
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
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "qwen", "contextWindow": 1000000, "maxOutput": 65536 }),
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
        // GLM-5.3 line cannot disable thinking — z.ai returns 400 code 1210
        // when `thinking.type` is `disabled` (upstream #4409). Set on the
        // pattern so both glm-5.3 and glm-5.3-flash inherit it while keeping
        // the zai shape and reasoning_effort support. `thinkingEffortSupported`
        // is stored verbatim for parity; ModelCapabilities does not model it.
        (
            "*glm-5.3*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "thinkingEffortSupported": true, "thinkingCanDisable": false, "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*glm-5.2*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "zai", "thinkingEffortSupported": true, "contextWindow": 1000000, "maxOutput": 128000 }),
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
        // Dotted v4.x ids (deepseek-v4.1-flash & friends) get vision + the
        // 128K output cap; must precede the generic *deepseek-v4* row
        // (upstream capabilities.js PATTERN_CAPABILITIES).
        (
            "*deepseek-v4.*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "thinkingEffortSupported": true, "contextWindow": 1000000, "maxOutput": 128000 }),
        ),
        (
            "*deepseek-v4*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "deepseek", "thinkingEffortSupported": true, "contextWindow": 1000000, "maxOutput": 384000 }),
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
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "minimax", "contextWindow": 1000000, "maxOutput": 131072 }),
        ),
        (
            "*minimax-m2.7*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "minimax", "thinkingCanDisable": false, "contextWindow": 204800, "maxOutput": 131072 }),
        ),
        (
            "*minimax-m2.5*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "minimax", "thinkingCanDisable": false, "contextWindow": 204800, "maxOutput": 131072 }),
        ),
        (
            "*minimax*",
            serde_json::json!({ "reasoning": true, "thinkingFormat": "minimax", "thinkingCanDisable": false, "contextWindow": 200000, "maxOutput": 131072 }),
        ),
        (
            "*mimo*v2.6*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "thinkingFormat": "deepseek", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "*mimo*v2.5*",
            serde_json::json!({ "vision": true, "audioInput": true, "videoInput": true, "reasoning": true, "thinkingFormat": "deepseek", "thinkingCanDisable": false, "contextWindow": 1048576, "maxOutput": 131072 }),
        ),
        (
            "*mimo*omni*",
            serde_json::json!({ "vision": true, "audioInput": true, "reasoning": true, "thinkingFormat": "deepseek", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 131072 }),
        ),
        (
            "*mimo*",
            serde_json::json!({ "vision": true, "reasoning": true, "thinkingFormat": "deepseek", "thinkingCanDisable": false, "contextWindow": 262144, "maxOutput": 131072 }),
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
pub(crate) fn match_pattern(pattern: &str, model: &str) -> bool {
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
    // Provider may arrive as a display alias ("ocg", "dv"); the tables are
    // keyed by canonical id — resolve once, exactly like upstream
    // `parseModel → resolveProviderAlias` before getCapabilitiesForModel.
    let resolved_provider;
    let provider = if provider.is_empty() {
        provider
    } else {
        resolved_provider = crate::core::model::resolve_provider_alias(provider);
        resolved_provider.as_str()
    };
    let base_model = model.rsplit('/').next().unwrap_or(model);

    // CommandCode wire is /alpha/generate for every model — family patterns
    // (deepseek-v4 → thinkingFormat:deepseek, vision:false) must not win
    // here (upstream capabilities.js commandcode/cmc branch).
    if provider == "commandcode" || provider == "cmc" {
        if let Some(table) = PROVIDER_CAPABILITIES.get("commandcode") {
            if let Some(entry) = table.get(model).or_else(|| table.get(base_model)) {
                return ModelCapabilities::from_value(entry);
            }
        }
        let mut caps = ModelCapabilities::default();
        caps.reasoning = true;
        caps.thinking_format = Some("commandcode");
        caps.thinking_effort_supported = true;
        caps.vision = !is_commandcode_text_only(model);
        caps.context_window = 1_000_000;
        caps.max_output = 384_000;
        return caps;
    }

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

/// Nested-combo recursion guard (upstream `_depth > 6`).
pub const COMBO_AGGREGATION_MAX_DEPTH: u32 = 6;

/// Capabilities of one combo seat: a `provider/model` id, or a bare model id
/// (provider `""` → pattern/exact tables only). The provider half is resolved
/// alias → provider id exactly like upstream `comboSeatCapabilities`
/// (`ALIAS_TO_PROVIDER_ID[alias] || alias`).
fn combo_seat_capabilities(full_id: &str) -> ModelCapabilities {
    let (provider, model) = match full_id.split_once('/') {
        Some((provider, model)) => (provider, model),
        None => ("", full_id),
    };
    let provider_id = if provider.is_empty() {
        String::new()
    } else {
        crate::core::model::resolve_provider_alias(provider)
    };
    get_capabilities_for_model(&provider_id, model)
}

/// Aggregate capabilities for a combo from its constituent `provider/model`
/// seats — port of 9router `aggregateComboCapabilities`
/// (capabilities.js:683-720, 5c217d3).
///
/// - Union:        vision, pdf, audioInput, videoInput, imageOutput,
///                 audioOutput, search
/// - Intersection: tools (every seat must support it)
/// - Primary:      reasoning / thinking* fields come from the first seat
/// - Conservative: contextWindow = min, maxOutput = max
///
/// `combo_lookup` maps combo name → member ids so a bare-name seat that is
/// itself a combo resolves recursively (depth-guarded at
/// [`COMBO_AGGREGATION_MAX_DEPTH`]). Returns `None` for empty input or when
/// the depth guard trips — upstream returns `null` there.
pub fn aggregate_combo_capabilities(
    combo_models: &[String],
    combo_lookup: Option<&HashMap<String, Vec<String>>>,
) -> Option<ModelCapabilities> {
    aggregate_combo_capabilities_at(combo_models, combo_lookup, 0)
}

fn aggregate_combo_capabilities_at(
    combo_models: &[String],
    combo_lookup: Option<&HashMap<String, Vec<String>>>,
    depth: u32,
) -> Option<ModelCapabilities> {
    if combo_models.is_empty() || depth > COMBO_AGGREGATION_MAX_DEPTH {
        return None;
    }

    let all_caps: Vec<ModelCapabilities> = combo_models
        .iter()
        .map(|full_id| {
            // Nested combo: bare name (no slash) present in the lookup —
            // recurse; on guard trip or missing entry fall back to treating
            // the id as a literal model (upstream `?? getCapabilitiesForModel`).
            if !full_id.contains('/') {
                if let Some(nested) = combo_lookup.and_then(|lookup| lookup.get(full_id)) {
                    return aggregate_combo_capabilities_at(nested, combo_lookup, depth + 1)
                        .unwrap_or_else(|| combo_seat_capabilities(full_id));
                }
            }
            combo_seat_capabilities(full_id)
        })
        .collect();

    let first = &all_caps[0];
    Some(ModelCapabilities {
        vision: all_caps.iter().any(|c| c.vision),
        pdf: all_caps.iter().any(|c| c.pdf),
        audio_input: all_caps.iter().any(|c| c.audio_input),
        video_input: all_caps.iter().any(|c| c.video_input),
        image_output: all_caps.iter().any(|c| c.image_output),
        audio_output: all_caps.iter().any(|c| c.audio_output),
        search: all_caps.iter().any(|c| c.search),
        tools: all_caps.iter().all(|c| c.tools),
        reasoning: first.reasoning,
        thinking_format: first.thinking_format,
        thinking_can_disable: first.thinking_can_disable,
        thinking_effort_supported: first.thinking_effort_supported,
        thinking_off_type: first.thinking_off_type,
        forced_tool_choice: all_caps.iter().all(|c| c.forced_tool_choice),
        thinking_range: first.thinking_range,
        context_window: all_caps
            .iter()
            .map(|c| c.context_window)
            .min()
            .unwrap_or(first.context_window),
        max_output: all_caps
            .iter()
            .map(|c| c.max_output)
            .max()
            .unwrap_or(first.max_output),
    })
}

/// The token limits a combo can *promise* on `/v1/models`
/// (`context_length` / `max_completion_tokens`) — port of 9router
/// `comboSeatLimits` (models/route.js:281-311). Unlike
/// [`aggregate_combo_capabilities`] (which reports `maxOutput` as the max so
/// the envelope shows what the combo *can* reach), this is the min across
/// every seat for BOTH fields: any seat can serve the request, so the only
/// window guaranteed is the smallest. Nested combos expand cycle-guarded.
pub fn combo_seat_limits(
    combo_models: &[String],
    combo_lookup: &HashMap<String, Vec<String>>,
) -> (Option<u64>, Option<u64>) {
    fn seat_limits(
        seat: &str,
        combo_lookup: &HashMap<String, Vec<String>>,
        visiting: &mut std::collections::HashSet<String>,
        ctx: &mut Option<u64>,
        out: &mut Option<u64>,
    ) {
        if !seat.contains('/') {
            if let Some(nested) = combo_lookup.get(seat) {
                // Cycle guard: a self-referential or looping combo publishes
                // no limits rather than recursing forever.
                if !visiting.insert(seat.to_string()) {
                    return;
                }
                for member in nested {
                    seat_limits(member, combo_lookup, visiting, ctx, out);
                }
                visiting.remove(seat);
                return;
            }
        }
        let caps = combo_seat_capabilities(seat);
        if caps.context_window > 0 {
            *ctx = Some(ctx.map_or(caps.context_window, |c| c.min(caps.context_window)));
        }
        if caps.max_output > 0 {
            *out = Some(out.map_or(caps.max_output, |o| o.min(caps.max_output)));
        }
    }

    let mut ctx = None;
    let mut out = None;
    let mut visiting = std::collections::HashSet::new();
    for seat in combo_models {
        seat_limits(seat, combo_lookup, &mut visiting, &mut ctx, &mut out);
    }
    (ctx, out)
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
    // Overlay keys are filed under canonical provider ids; requests may
    // arrive with a display alias ("ocg", "dv") — resolve once, same as the
    // hand lookup does.
    let resolved_provider;
    let provider = if provider.is_empty() {
        provider
    } else {
        resolved_provider = crate::core::model::resolve_provider_alias(provider);
        resolved_provider.as_str()
    };
    if let Some(modalities) =
        crate::core::model::catalog_overlay::catalog_modalities(provider, model)
    {
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

/// Mirrors Command Code CLI `isKnownTextOnlyModel` (no image input). New
/// models default to vision; only this denylist stays text-only
/// (upstream capabilities.js COMMANDCODE_TEXT_ONLY).
const COMMANDCODE_TEXT_ONLY: &[&str] = &[
    "deepseek/deepseek-v4-pro",
    "deepseek/deepseek-v4-flash",
    "deepseek/deepseek-v4-flash-fast",
    "zai-org/glm-5.3",
    "zai-org/glm-5.2",
    "zai-org/glm-5.2-fast",
    "zai-org/glm-5.1",
    "zai-org/glm-5",
    "minimaxai/minimax-m2.7",
    "minimax/minimax-m2.7-free",
    "minimaxai/minimax-m2.5",
    "xiaomi/mimo-v2.5-pro",
    "qwen/qwen3.6-max-preview",
    "qwen/qwen3.7-max",
    "meituan/longcat-2.0:free",
    "stepfun/step-3.5-flash",
    "tencent/hy4-preview",
    "tencent/hy3",
    "tencent/hy3-paid",
    "nvidia/nemotron-3-ultra-550b-a55b",
    "poolside/laguna-s-2.1-free",
    "inclusionai/ling-3.0-flash-free",
    "inclusionai/ling-3.0-flash-sante:free",
];

fn is_commandcode_text_only(model: &str) -> bool {
    let key = model.to_lowercase();
    if COMMANDCODE_TEXT_ONLY.contains(&key.as_str()) {
        return true;
    }
    COMMANDCODE_TEXT_ONLY.iter().any(|id| {
        let base = id.rsplit('/').next().unwrap_or(id);
        key == base || key.ends_with(&format!("/{base}"))
    })
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

/// Serialize a resolved [`ModelCapabilities`] to the camelCase JSON object
/// 9router emits on `/v1/models` entries and the dashboard caps feed
/// (`capabilities.js` resolved-caps shape, incl. `thinkingEffortSupported`).
pub fn capabilities_json(caps: &ModelCapabilities) -> Value {
    serde_json::json!({
        "vision": caps.vision,
        "pdf": caps.pdf,
        "audioInput": caps.audio_input,
        "videoInput": caps.video_input,
        "imageOutput": caps.image_output,
        "audioOutput": caps.audio_output,
        "search": caps.search,
        "tools": caps.tools,
        "reasoning": caps.reasoning,
        "thinkingFormat": caps.thinking_format,
        "thinkingCanDisable": caps.thinking_can_disable,
        "thinkingEffortSupported": caps.thinking_effort_supported,
        "thinkingOffType": caps.thinking_off_type,
        "forcedToolChoice": caps.forced_tool_choice,
        "thinkingRange": caps.thinking_range.map(|(min, max)| serde_json::json!({ "min": min, "max": max })),
        "contextWindow": caps.context_window,
        "maxOutput": caps.max_output,
    })
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
    fn codex_gpt_6_1_terra_capabilities() {
        // Upstream v0.5.112: gpt-6.1-terra is the 500k Codex OAuth window
        // (codex-rs models-manager context_window 500000), maxOutput 128k.
        let caps = get_capabilities_for_model("codex", "gpt-6.1-terra");
        assert_eq!(caps.context_window, 500_000);
        assert_eq!(caps.max_output, 128_000);
        assert!(caps.vision && caps.reasoning && caps.search);
        assert_eq!(caps.thinking_format, Some("openai"));

        // Sibling gpt-6.x rows + [1m] extended caps.
        for (model, window) in [
            ("gpt-6-astra", 272_000),
            ("gpt-6-sol", 272_000),
            ("gpt-6.1-sol", 272_000),
            ("gpt-6-luna", 272_000),
            ("gpt-6-astra[1m]", 872_000),
            ("gpt-6-sol[1m]", 872_000),
            ("gpt-6-luna[1m]", 872_000),
        ] {
            let caps = get_capabilities_for_model("codex", model);
            assert_eq!(caps.context_window, window, "{model}");
        }

        // Generic *gpt-6* pattern: 1.05M API window off the codex provider.
        let other = get_capabilities_for_model("openai", "gpt-6.1-terra");
        assert_eq!(other.context_window, 1_050_000);
        assert!(other.vision && other.reasoning);
    }

    #[test]
    fn haiku_5_5_resolves_adaptive_before_generic_haiku() {
        // #5183 / v0.5.112: claude-haiku-5.5 is adaptive-only — the dotted
        // (Copilot) and dashed (OpenCode Go) spellings must beat the generic
        // *claude*haiku* → claude-budget rule.
        for model in [
            "claude-haiku-5.5",
            "claude-haiku-5-5",
            "github/claude-haiku-5.5",
        ] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.thinking_format, Some("claude-adaptive"), "{model}");
            assert!(caps.vision, "{model}");
            assert!(caps.reasoning, "{model}");
        }
        // Older/unversioned haiku still resolves budget.
        for model in [
            "claude-haiku-4-5",
            "claude-haiku",
            "claude-sonnet-haiku-4.7",
        ] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.thinking_format, Some("claude-budget"), "{model}");
        }
    }

    #[test]
    fn gpt_6_image_variants_resolve_image_output() {
        // Upstream capabilities.js orders `*gpt-6*image*` before `*gpt-6*` so
        // image variants resolve imageOutput instead of chat caps.
        for model in ["gpt-6-sol-image", "gpt-6-astra-image", "cx/gpt-6-sol-image"] {
            let caps = get_capabilities_for_model("", model);
            assert!(caps.image_output, "{model}");
            assert!(!caps.vision, "{model}");
        }
        // Chat gpt-6 still resolves the generic family caps.
        let chat = get_capabilities_for_model("", "gpt-6-sol");
        assert!(chat.vision && !chat.image_output);
    }

    /// 9router capabilities.js PATTERN_CAPABILITIES `*gemini-4*` (commit
    /// 29fc880): Argon is multimodal with non-disableable gemini-level
    /// thinking, 1M context, 64K output.
    #[test]
    fn gemini_4_argon_capability_pattern() {
        for model in ["gemini-4-argon", "google/gemini-4-argon-high"] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.thinking_format, Some("gemini-level"), "{model}");
            assert!(!caps.thinking_can_disable, "{model}");
            assert_eq!(caps.context_window, 1_048_576, "{model}");
            assert_eq!(caps.max_output, 65_536, "{model}");
            assert!(caps.audio_input, "{model}");
            assert!(caps.video_input, "{model}");
        }
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
    fn gpt_5_4_plus_publish_1m_window_mini_nano_stay_400k() {
        // Upstream capabilities.js PATTERN_CAPABILITIES (89ffac5): the 1.05M
        // API window starts at gpt-5.4; mini/nano tiers stayed at 400k.
        for model in [
            "gpt-6-astra",
            "gpt-6.1-terra",
            "gpt-5.4",
            "gpt-5.5",
            "gpt-5.6",
            "openai/gpt-5.4-pro",
        ] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.context_window, 1_050_000, "{model}");
            assert_eq!(caps.max_output, 128_000, "{model}");
            assert!(caps.vision && caps.reasoning && caps.search, "{model}");
        }
        for model in ["gpt-5.4-mini", "gpt-5.4-nano", "gpt-5.4-mini-2026-01"] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.context_window, 400_000, "{model}");
        }
        // Older gpt-5.x and codex-suffixed ids keep the 400k generic window.
        for model in ["gpt-5", "gpt-5.1", "gpt-5.2-codex"] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.context_window, 400_000, "{model}");
        }
        // Gateway overrides still win: codex gpt-5.6-sol is 372k.
        assert_eq!(
            get_capabilities_for_model("codex", "gpt-5.6-sol").context_window,
            372_000
        );
    }

    #[test]
    fn aggregate_unions_modalities_and_intersects_tools() {
        // vision seat + plain text seat: union keeps vision; tools requires
        // every seat (default tools:true), so tools stays true here.
        let models = vec![
            "openai/gpt-4o".to_string(), // vision via *gpt-4o* pattern
            "openai/gpt-3.5-turbo".to_string(),
        ];
        let caps = aggregate_combo_capabilities(&models, None).unwrap();
        assert!(caps.vision);
        assert!(caps.tools);
        // contextWindow = min, maxOutput = max across seats.
        let ctx = models
            .iter()
            .map(|m| {
                let (p, id) = m.split_once('/').unwrap();
                get_capabilities_for_model(p, id).context_window
            })
            .min()
            .unwrap();
        assert_eq!(caps.context_window, ctx);
    }

    #[test]
    fn aggregate_intersects_tools_and_takes_primary_reasoning() {
        // gpt-image-1 has tools:false in MODEL_CAPABILITIES — a combo
        // containing it must report tools:false (intersection, not union).
        let models = vec![
            "openai/gpt-4o".to_string(),
            "openai/gpt-image-1".to_string(),
        ];
        let caps = aggregate_combo_capabilities(&models, None).unwrap();
        assert!(!caps.tools);
        // reasoning comes from the FIRST seat only.
        let first = get_capabilities_for_model("openai", "gpt-4o");
        assert_eq!(caps.reasoning, first.reasoning);
        assert_eq!(caps.thinking_format, first.thinking_format);
    }

    #[test]
    fn aggregate_resolves_nested_combos_and_guards_depth() {
        let mut lookup = HashMap::new();
        lookup.insert(
            "inner".to_string(),
            vec!["anthropic/claude-opus-4.8".to_string()],
        );
        lookup.insert(
            "outer".to_string(),
            vec!["inner".to_string(), "openai/gpt-4o".to_string()],
        );
        // A pure chain deeper than the guard: d0 contains only d1, …, d8
        // contains only d9 (a real model). Upstream `_depth > 6` makes the
        // nested call return null, and the `?? getCapabilitiesForModel`
        // fallback then treats the too-deep name as a literal model — the
        // top-level aggregate still resolves (floor caps), it just stops
        // recursing.
        for i in 0..9 {
            lookup.insert(format!("d{i}"), vec![format!("d{}", i + 1)]);
        }
        lookup.insert("d9".to_string(), vec!["openai/gpt-4o".to_string()]);

        let nested = vec!["inner".to_string(), "openai/gpt-4o".to_string()];
        let caps = aggregate_combo_capabilities(&nested, Some(&lookup)).unwrap();
        // claude seat contributes vision+reasoning+search; union with gpt-4o.
        assert!(caps.vision && caps.search && caps.reasoning);
        assert_eq!(caps.context_window, 128_000); // min(1M claude, 128k gpt-4o)
        assert_eq!(caps.max_output, 128_000); // max(128k, 16k)

        // Depth guard: recursion stops at depth 6 — d7 resolves as a literal
        // model id (floor caps) instead of expanding to gpt-4o's 128k window.
        let deep = vec!["d0".to_string()];
        let deep_caps = aggregate_combo_capabilities(&deep, Some(&lookup)).unwrap();
        assert_eq!(deep_caps.context_window, 200_000);

        // Empty input → None.
        assert!(aggregate_combo_capabilities(&[], Some(&lookup)).is_none());
    }

    #[test]
    fn combo_seat_limits_takes_min_of_both_fields() {
        // Upstream comboSeatLimits: BOTH context_length and
        // max_completion_tokens are the min across seats (any seat can serve).
        // gpt-4o: 128k ctx / 16k out; claude-opus-4.8: 1M ctx / 128k out.
        let models = vec![
            "openai/gpt-4o".to_string(),
            "anthropic/claude-opus-4.8".to_string(),
        ];
        let mut lookup = HashMap::new();
        let (ctx, out) = combo_seat_limits(&models, &lookup);
        assert_eq!(ctx, Some(128_000));
        assert_eq!(out, Some(16_384));

        // Nested combo expands; a self-referencing combo cannot loop forever.
        lookup.insert(
            "loop".to_string(),
            vec!["loop".to_string(), "openai/gpt-4o".to_string()],
        );
        let (ctx2, _) = combo_seat_limits(&["loop".to_string()], &lookup);
        assert_eq!(ctx2, Some(128_000));
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

    #[test]
    fn current_claude_models_report_pdf_input() {
        // Bead openproxy-9router-parity-v5104-sync-eidj.27: upstream marks
        // pdf:true on every current Claude family (adaptive 5.x/4.6+ and the
        // versioned budget rules haiku-4/5, opus-4, sonnet-4, fable, mythos).
        for model in [
            // Exact MODEL_CAPABILITIES entries.
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-opus-4.7",
            "claude-sonnet-4-6",
            // Adaptive + sonnet-5 patterns.
            "anthropic/claude-opus-5-20261001",
            "claude-sonnet-5-5",
            "claude-sonnet-5.5",
            // Versioned budget patterns.
            "claude-haiku-4-5",
            "claude-haiku-5",
            "claude-opus-4-1",
            "claude-sonnet-4-5",
            // fable/mythos budget patterns.
            "claude-fable-5",
            "claude-mythos-preview",
        ] {
            let caps = get_capabilities_for_model("", model);
            assert!(caps.pdf, "{model} should accept PDF input");
            assert!(caps.vision, "{model} should accept images");
        }
        // Unversioned legacy ids stay conservative (no pdf) per upstream.
        // hand_* bypasses the synced-catalog overlay so a models.dev delta
        // can't flip the assertion.
        let legacy = hand_capabilities_for_model("", "claude-haiku");
        assert!(!legacy.pdf);
        let generic = hand_capabilities_for_model("", "claude-3-opus");
        assert!(!generic.pdf);
    }

    #[test]
    fn glm_5_3_cannot_disable_thinking() {
        // Bead .27 / upstream #4409: z.ai returns 400 code 1210 when
        // thinking.type is "disabled" on the whole GLM-5.3 line.
        for model in ["glm-5.3", "glm-5.3-flash", "z-ai/glm-5.3"] {
            let caps = get_capabilities_for_model("", model);
            assert!(!caps.thinking_can_disable, "{model}");
            assert_eq!(caps.thinking_format, Some("zai"), "{model}");
            assert!(caps.reasoning, "{model}");
        }
        // The exact glm-5.3-flash entry is multimodal per z.ai.
        let flash = get_capabilities_for_model("", "glm-5.3-flash");
        assert!(flash.vision && flash.video_input && flash.pdf);
        assert_eq!(flash.context_window, 1_000_000);
        assert_eq!(flash.max_output, 131_072);
        // GLM-5.2 has its own exact entry upstream (1M window, zai shape,
        // thinking cannot be disabled); older lines keep the default.
        let glm52 = get_capabilities_for_model("", "glm-5.2");
        assert!(!glm52.thinking_can_disable);
        assert_eq!(glm52.thinking_format, Some("zai"));
        assert_eq!(glm52.context_window, 1_000_000);
        let glm51 = get_capabilities_for_model("", "glm-5.1");
        assert!(glm51.thinking_can_disable);
    }

    #[test]
    fn claude_5_5_dotted_and_dashed_rules() {
        // Bead openproxy-4d5h.45 / upstream v0.5.112 capabilities.js: Opus
        // 5.5 rejects thinking "disabled" (canDisable:false) and forced
        // tool_choice; Sonnet 5.5 substitutes "between_tools" and also bans
        // forced tool_choice. Dotted (Copilot/Kiro) and dashed (level-suffix)
        // spellings share the rules.
        for model in [
            "claude-opus-5.5",
            "claude-opus-5.5-thinking",
            "claude-opus-5.5-agentic",
            "claude-opus-5-5",
            "claude-opus-5-5-high",
        ] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.thinking_format, Some("claude-adaptive"), "{model}");
            assert!(!caps.thinking_can_disable, "{model}");
            assert!(!caps.forced_tool_choice, "{model}");
            assert_eq!(caps.thinking_off_type, None, "{model}");
            assert_eq!(caps.context_window, 1_000_000, "{model}");
        }
        for model in [
            "claude-sonnet-5.5",
            "claude-sonnet-5-5",
            "claude-sonnet-5-5-medium",
        ] {
            let caps = get_capabilities_for_model("", model);
            assert_eq!(caps.thinking_format, Some("claude-adaptive"), "{model}");
            assert_eq!(caps.thinking_off_type, Some("between_tools"), "{model}");
            assert!(!caps.forced_tool_choice, "{model}");
            // Sonnet 5.5 keeps thinkingCanDisable at the default (true).
            assert!(caps.thinking_can_disable, "{model}");
        }
        // claude-fable-5-1 exact entry is adaptive-only (upstream key is the
        // dashed spelling; the dotted github id falls through to the fable
        // pattern exactly as upstream does).
        let fable = get_capabilities_for_model("", "claude-fable-5-1");
        assert_eq!(fable.thinking_format, Some("claude-adaptive"));
        assert!(!fable.thinking_can_disable);
    }

    #[test]
    fn github_haiku_5_5_provider_override() {
        // Bead openproxy-4d5h.45: PROVIDER_CAPABILITIES["github"] pins
        // claude-haiku-5.5 to claude-adaptive on Copilot's Anthropic-native
        // /v1/messages shim (budget shape 400s upstream).
        let caps = get_capabilities_for_model("github", "claude-haiku-5.5");
        assert_eq!(caps.thinking_format, Some("claude-adaptive"));
        assert_eq!(caps.context_window, 1_000_000);
        assert_eq!(caps.max_output, 128_000);
    }

    #[test]
    fn sensenova_flash_lite_capabilities() {
        // Bead openproxy-4d5h.40 / upstream capabilities.js exact entry.
        let caps = get_capabilities_for_model("sensenova", "sensenova-6.8-flash-lite");
        assert!(caps.vision);
        assert!(caps.reasoning);
        assert_eq!(caps.thinking_format, Some("openai"));
        assert_eq!(caps.context_window, 1_000_000);
        assert_eq!(caps.max_output, 131_072);
    }
}
