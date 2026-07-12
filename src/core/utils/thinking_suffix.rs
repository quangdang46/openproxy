//! Global `model(level)` / `model-level` thinking suffix helpers.
//!
//! Ports 9router `thinkingUnified` strip + apply:
//! - `stripThinkingSuffix` / `parseSuffix` level extraction
//! - `applyThinking` post-translate re-apply onto provider-native fields
//!
//! Levels: none|minimal|low|medium|high|xhigh|max.

use serde_json::{json, Map, Value};

use crate::core::translator::registry::Format;

/// Effort / thinking levels recognized in model suffixes.
pub const THINKING_LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// 9router LEVEL_TO_BUDGET for Claude-style budget_tokens.
pub fn level_to_budget(level: &str) -> Option<u32> {
    match level.to_ascii_lowercase().as_str() {
        "none" => Some(0),
        "minimal" => Some(512),
        "low" => Some(1024),
        "medium" => Some(8192),
        "high" => Some(24576),
        "xhigh" => Some(32768),
        "max" => Some(128_000),
        _ => None,
    }
}

/// Numeric budget → nearest discrete level (9router `budgetToLevel`).
pub fn budget_to_level(budget: u32) -> Option<&'static str> {
    if budget == 0 {
        return None;
    }
    if budget <= 768 {
        Some("minimal")
    } else if budget <= 4096 {
        Some("low")
    } else if budget <= 16384 {
        Some("medium")
    } else if budget <= 28672 {
        Some("high")
    } else {
        Some("xhigh")
    }
}

/// Parse trailing thinking level from a model id.
///
/// Supports:
/// - `foo-high` / `foo-medium` / …
/// - `foo(high)` / `foo (high)`
///
/// Returns `(upstream_model, Some(level))` when a suffix was stripped.
pub fn strip_thinking_suffix(model: &str) -> (&str, Option<&str>) {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return (model, None);
    }

    // Parenthetical: model(high) or model (high)
    for level in THINKING_LEVELS {
        let paren = format!("({level})");
        if let Some(idx) = trimmed.rfind(&paren) {
            // ensure suffix is at end (allow trailing whitespace already trimmed)
            if idx + paren.len() == trimmed.len() {
                let base = trimmed[..idx].trim_end();
                if !base.is_empty() {
                    return (base, Some(*level));
                }
            }
        }
    }

    // Hyphen suffix: model-high
    for level in THINKING_LEVELS {
        let suffix = format!("-{level}");
        if let Some(base) = trimmed.strip_suffix(&suffix) {
            if !base.is_empty() {
                return (base, Some(*level));
            }
        }
    }

    (trimmed, None)
}

/// Apply strip to an owned model string; returns (upstream, optional level).
pub fn strip_thinking_suffix_owned(model: &str) -> (String, Option<String>) {
    let (base, level) = strip_thinking_suffix(model);
    (base.to_string(), level.map(str::to_string))
}

/// Wire-format native thinking style (9router `thinkingFormat` / FORMAT_TO_NATIVE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingNative {
    OpenAi,
    ClaudeBudget,
    ClaudeAdaptive,
    GeminiBudget,
    GeminiLevel,
    Zai,
    Qwen,
    DeepSeek,
    Kimi,
    MiniMax,
    /// No wire rewrite (Kiro handles thinking via system prefix / model suffix).
    Noop,
}

/// Resolve native thinking wire format from target format + provider + model.
///
/// Mirrors 9router `resolveFormat` + capability heuristics without the full
/// capabilities matrix (provider/model exact overrides for high-traffic families).
pub fn resolve_thinking_native(
    target_format: Format,
    provider: &str,
    model: &str,
) -> ThinkingNative {
    let p = provider.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();

    // Provider-level thinkingFormat overrides (registry thinkingFormat)
    match p.as_str() {
        "glm" | "glm-cn" | "zai" | "zhipu" => return ThinkingNative::Zai,
        "qwen" | "qwen-code" | "dashscope" => return ThinkingNative::Qwen,
        "deepseek" | "ds" => return ThinkingNative::DeepSeek,
        "kimi" | "kimi-coding" | "moonshot" => return ThinkingNative::Kimi,
        "minimax" | "minimax-cn" => return ThinkingNative::MiniMax,
        "kiro" => return ThinkingNative::Noop,
        _ => {}
    }

    match target_format {
        Format::OpenAi | Format::OpenAiResponses | Format::OpenAiResponse | Format::Codex => {
            ThinkingNative::OpenAi
        }
        Format::Claude => {
            if is_claude_adaptive_model(&m) {
                ThinkingNative::ClaudeAdaptive
            } else {
                ThinkingNative::ClaudeBudget
            }
        }
        Format::Gemini | Format::Vertex => {
            if is_gemini_level_model(&m) {
                ThinkingNative::GeminiLevel
            } else {
                ThinkingNative::GeminiBudget
            }
        }
        Format::GeminiCli | Format::Antigravity => {
            if is_gemini_level_model(&m) {
                ThinkingNative::GeminiLevel
            } else {
                ThinkingNative::GeminiBudget
            }
        }
        Format::Kiro => ThinkingNative::Noop,
        // Cursor / Ollama / CommandCode: leave body alone (executors normalize).
        Format::Cursor | Format::Ollama | Format::CommandCode => ThinkingNative::Noop,
    }
}

fn is_claude_adaptive_model(model: &str) -> bool {
    // 9router MODEL_CAPABILITIES: opus/sonnet 4.6+ and sonnet-5 use claude-adaptive
    let m = model.to_ascii_lowercase();
    if m.contains("haiku") {
        return false;
    }
    m.contains("opus-4.6")
        || m.contains("opus-4-6")
        || m.contains("opus-4.7")
        || m.contains("opus-4-7")
        || m.contains("opus-4.8")
        || m.contains("opus-4-8")
        || m.contains("sonnet-4.6")
        || m.contains("sonnet-4-6")
        || m.contains("sonnet-5")
}

fn is_gemini_level_model(model: &str) -> bool {
    // Gemini 3.x uses thinkingLevel; 2.5 uses thinkingBudget.
    let m = model.to_ascii_lowercase();
    m.contains("gemini-3") || m.contains("gemini3")
}

/// True when the body already carries a client- or settings-provided thinking intent.
///
/// Used to avoid double-applying when `providerThinking` already set fields and
/// there is no model-suffix override.
pub fn body_has_thinking_intent(body: &Value) -> bool {
    if body
        .get("thinking")
        .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    {
        return true;
    }
    if body
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if body
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if body
        .get("enable_thinking")
        .and_then(Value::as_bool)
        .is_some()
    {
        return true;
    }
    if body.get("thinkingConfig").is_some()
        || body.pointer("/generationConfig/thinkingConfig").is_some()
        || body
            .pointer("/request/generationConfig/thinkingConfig")
            .is_some()
    {
        return true;
    }
    false
}

/// Strip all known thinking wire fields (9router `stripAll`).
fn strip_all_thinking_fields(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    obj.remove("thinking");
    obj.remove("reasoning_effort");
    obj.remove("reasoning");
    obj.remove("thinkingConfig");
    obj.remove("enable_thinking");
    obj.remove("thinking_budget");
    obj.remove("output_config");
    if let Some(gc) = obj
        .get_mut("generationConfig")
        .and_then(Value::as_object_mut)
    {
        gc.remove("thinkingConfig");
    }
    if let Some(req) = obj.get_mut("request").and_then(Value::as_object_mut) {
        if let Some(gc) = req
            .get_mut("generationConfig")
            .and_then(Value::as_object_mut)
        {
            gc.remove("thinkingConfig");
        }
    }
}

fn ensure_object<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    if !map.get(key).map(Value::is_object).unwrap_or(false) {
        map.insert(key.to_string(), Value::Object(Map::new()));
    }
    map.get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("just inserted object")
}

fn get_gemini_generation_config(body: &mut Value) -> Option<&mut Map<String, Value>> {
    let obj = body.as_object_mut()?;
    if obj.get("request").map(Value::is_object).unwrap_or(false) {
        let req = ensure_object(obj, "request");
        return Some(ensure_object(req, "generationConfig"));
    }
    Some(ensure_object(obj, "generationConfig"))
}

fn set_gemini_thinking(body: &mut Value, tc: Value) {
    if let Some(gc) = get_gemini_generation_config(body) {
        gc.insert("thinkingConfig".into(), tc);
    }
}

fn ensure_gemini_output_floor(body: &mut Value, floor: u32) {
    if let Some(gc) = get_gemini_generation_config(body) {
        let current = gc
            .get("maxOutputTokens")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        if current.map(|c| c < floor).unwrap_or(true) {
            gc.insert("maxOutputTokens".into(), json!(floor));
        }
    }
}

fn gemini_budget_output_floor(budget: i64) -> u32 {
    if budget < 0 {
        return 32768;
    }
    if budget <= 1024 {
        8192
    } else if budget <= 8192 {
        16384
    } else if budget <= 24576 {
        32768
    } else {
        65535
    }
}

fn gemini_level_output_floor(level: &str) -> u32 {
    match level {
        "minimal" => 4096,
        "low" => 8192,
        "medium" => 16384,
        "high" => 65535,
        _ => 65535,
    }
}

fn effort_to_gemini_thinking_level(level: &str) -> &str {
    // Gemini 3 enum: minimal|low|medium|high — clamp max/xhigh/none/auto
    match level {
        "none" | "off" => "minimal",
        "xhigh" | "max" | "auto" => "high",
        other => other,
    }
}

fn to_kimi_reasoning_effort(level: &str) -> Option<&'static str> {
    match level {
        "auto" => Some("high"),
        "minimal" => Some("low"),
        "xhigh" => Some("max"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "max" => Some("max"),
        "none" => None,
        _ => None,
    }
}

/// Apply a discrete thinking level onto the post-translate body in provider-native form.
///
/// Port of 9router `applyThinking` for the common case where the config is a
/// level override from `model(level)` / `model-level` suffix.
///
/// Call after request translation. When `level` is `None`, this is a no-op
/// (providerThinking / body fields already handled upstream).
pub fn apply_thinking_level(
    target_format: Format,
    provider: &str,
    model: &str,
    body: &mut Value,
    level: &str,
) {
    if !body.is_object() {
        return;
    }
    let level = level.to_ascii_lowercase();
    let native = resolve_thinking_native(target_format, provider, model);
    if native == ThinkingNative::Noop {
        return;
    }

    let none = level == "none" || level == "off";
    strip_all_thinking_fields(body);

    match native {
        ThinkingNative::OpenAi => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("reasoning_effort".into(), Value::String("none".into()));
                }
                return;
            }
            let effort = if level == "max" {
                "xhigh"
            } else {
                level.as_str()
            };
            if let Some(obj) = body.as_object_mut() {
                obj.insert("reasoning_effort".into(), Value::String(effort.into()));
            }
            // Codex / Responses: also set reasoning.effort when body looks like Responses API
            if matches!(
                target_format,
                Format::Codex | Format::OpenAiResponses | Format::OpenAiResponse
            ) || body.get("input").is_some()
            {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert(
                        "reasoning".into(),
                        json!({"effort": effort, "summary": "auto"}),
                    );
                }
            }
        }
        ThinkingNative::ClaudeBudget => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            let budget = level_to_budget(&level).unwrap_or(8192);
            if let Some(obj) = body.as_object_mut() {
                if budget == 0 {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                } else {
                    obj.insert(
                        "thinking".into(),
                        json!({"type": "enabled", "budget_tokens": budget}),
                    );
                }
            }
        }
        ThinkingNative::ClaudeAdaptive => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            let effort = if level == "xhigh" || level == "max" {
                "high"
            } else {
                level.as_str()
            };
            if let Some(obj) = body.as_object_mut() {
                obj.insert("output_config".into(), json!({"effort": effort}));
            }
        }
        ThinkingNative::GeminiBudget => {
            if none {
                set_gemini_thinking(body, json!({"thinkingBudget": 0, "includeThoughts": false}));
                return;
            }
            let budget = level_to_budget(&level).unwrap_or(8192) as i64;
            set_gemini_thinking(
                body,
                json!({"thinkingBudget": budget, "includeThoughts": true}),
            );
            ensure_gemini_output_floor(body, gemini_budget_output_floor(budget));
        }
        ThinkingNative::GeminiLevel => {
            let glevel = if none {
                "minimal"
            } else {
                effort_to_gemini_thinking_level(&level)
            };
            set_gemini_thinking(
                body,
                json!({
                    "thinkingLevel": glevel,
                    "includeThoughts": glevel != "minimal",
                }),
            );
            ensure_gemini_output_floor(body, gemini_level_output_floor(glevel));
        }
        ThinkingNative::Zai => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("enable_thinking".into(), Value::Bool(false));
                }
                return;
            }
            if let Some(obj) = body.as_object_mut() {
                obj.insert("thinking".into(), json!({"type": "enabled"}));
            }
        }
        ThinkingNative::Qwen => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("enable_thinking".into(), Value::Bool(false));
                }
                return;
            }
            if let Some(obj) = body.as_object_mut() {
                obj.insert("enable_thinking".into(), Value::Bool(true));
                if let Some(budget) = level_to_budget(&level) {
                    if budget > 0 {
                        obj.insert("thinking_budget".into(), json!(budget));
                    }
                }
            }
        }
        ThinkingNative::DeepSeek => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            let effort = if level == "xhigh" || level == "max" {
                "max"
            } else {
                "high"
            };
            if let Some(obj) = body.as_object_mut() {
                obj.insert("thinking".into(), json!({"type": "enabled"}));
                obj.insert("reasoning_effort".into(), Value::String(effort.into()));
            }
        }
        ThinkingNative::Kimi => {
            if none {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("thinking".into(), json!({"type": "disabled"}));
                }
                return;
            }
            if let Some(effort) = to_kimi_reasoning_effort(&level) {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("reasoning_effort".into(), Value::String(effort.into()));
                }
            }
        }
        ThinkingNative::MiniMax => {
            let t = if none { "disabled" } else { "adaptive" };
            if let Some(obj) = body.as_object_mut() {
                obj.insert("thinking".into(), json!({"type": t}));
            }
        }
        ThinkingNative::Noop => {}
    }
}

/// Post-translate re-apply entry point.
///
/// - When `suffix_level` is `Some`, always apply (model suffix is explicit override).
/// - When `suffix_level` is `None`, leave body alone so `providerThinking` /
///   client fields are not double-applied or wiped.
pub fn reapply_thinking_after_translate(
    target_format: Format,
    provider: &str,
    model: &str,
    body: &mut Value,
    suffix_level: Option<&str>,
) {
    if let Some(level) = suffix_level {
        apply_thinking_level(target_format, provider, model, body, level);
        return;
    }
    // No suffix override: respect existing body intent (providerThinking / client).
    // 9router would still normalize format via extractThinking, but OP already
    // maps reasoning_effort → thinking during openai→claude translate. Skipping
    // avoids wiping providerThinking-injected fields on passthrough/same-format.
    let _ = (body, provider, model, target_format);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_paren_high() {
        assert_eq!(
            strip_thinking_suffix("gpt-4o(high)"),
            ("gpt-4o", Some("high"))
        );
        assert_eq!(
            strip_thinking_suffix("gpt-4o (medium)"),
            ("gpt-4o", Some("medium"))
        );
    }

    #[test]
    fn strips_hyphen_effort() {
        assert_eq!(
            strip_thinking_suffix("grok-4.5-high"),
            ("grok-4.5", Some("high"))
        );
        assert_eq!(strip_thinking_suffix("o3-low"), ("o3", Some("low")));
    }

    #[test]
    fn leaves_plain_models() {
        assert_eq!(strip_thinking_suffix("gpt-4o"), ("gpt-4o", None));
        assert_eq!(
            strip_thinking_suffix("claude-sonnet-4"),
            ("claude-sonnet-4", None)
        );
    }

    #[test]
    fn budget_map_matches_9router() {
        assert_eq!(level_to_budget("low"), Some(1024));
        assert_eq!(level_to_budget("high"), Some(24576));
        assert_eq!(level_to_budget("medium"), Some(8192));
        assert_eq!(budget_to_level(1024), Some("low"));
        assert_eq!(budget_to_level(24576), Some("high"));
    }

    #[test]
    fn strip_apply_roundtrip_openai() {
        let (clean, level) = strip_thinking_suffix_owned("gpt-5(high)");
        assert_eq!(clean, "gpt-5");
        assert_eq!(level.as_deref(), Some("high"));

        let mut body = json!({"messages": [], "model": clean});
        reapply_thinking_after_translate(
            Format::OpenAi,
            "openai",
            &clean,
            &mut body,
            level.as_deref(),
        );
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn strip_apply_roundtrip_claude_budget() {
        let (clean, level) = strip_thinking_suffix_owned("claude-haiku-4.5-high");
        assert_eq!(clean, "claude-haiku-4.5");
        assert_eq!(level.as_deref(), Some("high"));

        let mut body = json!({"messages": [], "model": clean});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            &clean,
            &mut body,
            level.as_deref(),
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 24576);
    }

    #[test]
    fn strip_apply_claude_adaptive_uses_output_config() {
        let (clean, level) = strip_thinking_suffix_owned("claude-opus-4.7(medium)");
        assert_eq!(clean, "claude-opus-4.7");
        let mut body = json!({"messages": []});
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            &clean,
            &mut body,
            level.as_deref(),
        );
        assert_eq!(body["output_config"]["effort"], "medium");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn openai_max_clamps_to_xhigh() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "openai", "gpt-5", &mut body, "max");
        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn none_disables_claude() {
        let mut body =
            json!({"messages": [], "thinking": {"type": "enabled", "budget_tokens": 1000}});
        apply_thinking_level(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            "none",
        );
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn reapply_skips_when_no_suffix_and_body_has_intent() {
        let mut body = json!({
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 10000}
        });
        // providerThinking already set — no suffix → leave alone
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            None,
        );
        assert_eq!(body["thinking"]["budget_tokens"], 10000);
    }

    #[test]
    fn suffix_overrides_existing_thinking() {
        let mut body = json!({
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 10000}
        });
        reapply_thinking_after_translate(
            Format::Claude,
            "claude",
            "claude-haiku-4.5",
            &mut body,
            Some("low"),
        );
        assert_eq!(body["thinking"]["budget_tokens"], 1024);
    }

    #[test]
    fn glm_zai_format_enable_thinking() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "glm", "glm-4.6", &mut body, "high");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn deepseek_enabled_plus_effort() {
        let mut body = json!({"messages": []});
        apply_thinking_level(
            Format::OpenAi,
            "deepseek",
            "deepseek-v4-pro",
            &mut body,
            "low",
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        // deepseek maps low → high (only high/max supported)
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn kimi_maps_minimal_to_low() {
        let mut body = json!({"messages": []});
        apply_thinking_level(Format::OpenAi, "kimi", "kimi-k2", &mut body, "minimal");
        assert_eq!(body["reasoning_effort"], "low");
    }

    #[test]
    fn gemini_budget_sets_thinking_config() {
        let mut body = json!({"contents": []});
        apply_thinking_level(
            Format::Gemini,
            "gemini",
            "gemini-2.5-flash",
            &mut body,
            "high",
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            24576
        );
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn codex_sets_reasoning_object() {
        let mut body = json!({"input": []});
        apply_thinking_level(Format::Codex, "codex", "gpt-5", &mut body, "high");
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["reasoning"]["effort"], "high");
    }

    #[test]
    fn resolve_native_formats() {
        assert_eq!(
            resolve_thinking_native(Format::OpenAi, "openai", "gpt-5"),
            ThinkingNative::OpenAi
        );
        assert_eq!(
            resolve_thinking_native(Format::Claude, "claude", "claude-haiku-4.5"),
            ThinkingNative::ClaudeBudget
        );
        assert_eq!(
            resolve_thinking_native(Format::Claude, "claude", "claude-opus-4.7"),
            ThinkingNative::ClaudeAdaptive
        );
        assert_eq!(
            resolve_thinking_native(Format::OpenAi, "glm", "glm-4.6"),
            ThinkingNative::Zai
        );
        assert_eq!(
            resolve_thinking_native(Format::Kiro, "kiro", "amazon-nova"),
            ThinkingNative::Noop
        );
    }
}
