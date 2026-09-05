//! Port of `open-sse/translator/formats/claude.js`.
//!
//! Claude-specific request normalisation:
//!   - `prepare_claude_request` — normalise system prompt, thinking config,
//!     max_tokens handling, cache_control, tool dedup, and cloaking.
//!   - `normalize_claude_passthrough` — strip unsupported fields, align
//!     message format for Claude passthrough mode.

use serde_json::{json, Value};

use crate::core::config::runtime_config::DEFAULT_MAX_TOKENS;
use crate::core::utils::claude_cloaking::apply_cloaking;
use crate::core::utils::claude_header_cache::get_cached_claude_headers;

/// Default thinking signature injected when an `anthropic-compatible`
/// provider serves a thinking block without a valid signature.
const DEFAULT_THINKING_CLAUDE_SIGNATURE: &str = "EpwGCkYIChgCKkCzVUuRrg7CcglSUWEef4rH6o35g9UYS8ZPe0/VomQTBsFx6sttYNj5l8GqgW6ejuHyYqpFToxIbZl0bw17l5dJEgzCnqDO0Z8fRlMrNgsaDLS1cnCjC53KBqE0CCIwAADQdo1eO+7qPAmo8J4WR3JPmr92S97kmvr5K1iPMiOpkZNj8mEXW8uzBoOJs/9ZKoMFiqHJ3UObwaJDqFOW70E9oCwDoc6jesaWVAEdN5vWfKMpIkjFJjECdjIdkxyJNJ8Ib8yXVal3qwE7uThoPRqSZDdHB5mmwPEjWE/90cSYCbtX2YsJki1265CabBb8/QEkODXg4kgRrL+c8e8rRXz/dr1RswvaPuzEdGKHRNi9UooNUeOK4/ebx1KkP9YZttyohN9GWqlts36kOoW0Cfie/ABDgF9g534BPth/sstxDM6d79QlRmh6NxizyTF74DXJI34u0M4tTRchqE5pAq85SgdJaa+dix1yJPMji8m6nZkwJbscJb9rdc2MKyKWjz8QL2+rTSSuZ2F1k1qSsW0xNcI7qLcI12Vncfn/VqY6YOIZy/saZBR0ezXvN6g+UYbuIdyVg7AyIFZt3nbrO7/kmOEb2VKzygwklHGEIJHfFgMpH3JSrAzbZIowVHOF7VaJ+KXRFDCFin7hHTOiOsdg+1ij1mML9Z/x/9CP4b7OUcaQm1llDZPSHc6rZMNL3DdB+fW5YfmNgKU35S+7AMtA10nVILzDAk1UV4T2K9Do09JlI6rjOs9UuULlIN2Z0eE8YTlANR6uQcw7lMcdfqYE8tke4rDKc2dDiaS5vVe45VewICNpdXGN11yw8QqH7p27CR1HtN30e0tHXOR3bIwWk/Yb6O5fTaKG6Ri8e5ZCPvdD9HqepVi188nM0iTjJqL58F3ni04ECIhcbyaQWnuTes1Kw4CMwiZDLQkk8Hgz7HkUOf1btQTF/0nhD7ry0n0hAEg2PaDM3V6TjOjf4hEldRmeqERcQF1PfgKb6ZM12rlIIfUqKACczWJSzTV158+47HX36o0cgux6nFlv/DE+sEiRVxgB";

// ─── helpers ─────────────────────────────────────────────────────────

/// Check if a message has valid (non-empty / non-trivial) content.
fn has_valid_content(msg: &Value) -> bool {
    match msg.get("content") {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(arr)) => arr.iter().any(|block| {
            let t = block.get("type").and_then(Value::as_str).unwrap_or("");
            t == "text"
                && block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
                || t == "tool_use"
                || t == "tool_result"
                || t == "image"
                || t == "document"
        }),
        _ => false,
    }
}

/// Models that reject `thinking.type = "adaptive"` + `output_config.effort`.
fn is_adaptive_thinking_unsupported(model: &str) -> bool {
    model.to_lowercase().contains("haiku")
}

/// Providers whose quirks include dropping `output_config`.
fn provider_drops_output_config(provider: &str) -> bool {
    matches!(provider, "minimax" | "minimax-cn")
}

/// Find the index of the last tool that does NOT have `defer_loading: true`.
///
/// Anthropic rejects tools carrying both `defer_loading:true` and `cache_control`
/// ("Tools defer_loading cannot use prompt caching", #3567). MCP clients put
/// `defer_loading` on tools that should not anchor a cache breakpoint.
/// Mirrors `lastCacheableToolIndex` in `open-sse/translator/formats/claude.js:19-25`.
fn last_cacheable_tool_index(tools: &[Value]) -> Option<usize> {
    tools.iter().rposition(|tool| {
        !tool
            .get("defer_loading")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
}

/// Strip the `[1m]` context marker from a model name if present.
///
/// When the 1M-context beta is toggled on, Claude Code marks requests as
/// `claude-opus-5[1m]`. The marker is a client-side annotation, not a valid
/// model id — requests carrying it fail at model resolution. The actual 1M
/// capability travels via the `anthropic-beta` header, which is forwarded untouched.
/// Mirrors `CONTEXT_MARKER` in `open-sse/utils/modelMarkers.js:11`.
pub fn strip_model_context_marker(model: &str) -> (String, Option<&'static str>) {
    // Simple suffix check — avoids pulling in regex crate for a single pattern.
    if let Some(stripped) = model.strip_suffix("[1m]") {
        (stripped.to_string(), Some("1m"))
    } else if let Some(stripped) = model.strip_suffix("[1M]") {
        (stripped.to_string(), Some("1m"))
    } else {
        (model.to_string(), None)
    }
}

/// Regex pattern for valid Claude server_tool_use ids.
/// Must start with `srvtoolu_` followed by alphanumeric/underscore characters.
/// Anything else is considered a foreign id (from a non-Claude provider) and
/// must be dropped before sending to Claude to avoid 400 errors.
/// Mirrors `CLAUDE_SERVER_TOOL_USE_ID` in `open-sse/translator/formats/claude.js:124`.
fn is_valid_srvtoolu_id(id: &str) -> bool {
    // Mirrors CLAUDE_SERVER_TOOL_USE_ID = /^srvtoolu_[a-zA-Z0-9_]+$/ (+ requires ≥1 char after prefix)
    id.starts_with("srvtoolu_")
        && id.len() > 9
        && id[9..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ─── normalizeClaudePassthrough ─────────────────────────────────────

/// Normalize a native Claude passthrough body to match the Anthropic
/// Messages API spec.
///
/// Older Cowork / Claude Code clients emit beta-only shapes that OAuth
/// endpoints reject:
///   1. `thinking.type "adaptive"` — unsupported on Haiku → downgrade to
///      `enabled` with budget_tokens 10000.
///   2. `output_config.effort` — unsupported on Haiku → strip.
///   3. Mid-conversation `role: "system"` messages → hoist into the
///      top-level `system` field.
pub fn normalize_claude_passthrough(body: &mut Value, model: &str) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // 1. Downgrade adaptive thinking for models that don't support it
    if is_adaptive_thinking_unsupported(model) {
        if let Some(thinking) = obj.get_mut("thinking") {
            if thinking.get("type").and_then(Value::as_str) == Some("adaptive") {
                *thinking = json!({"type": "enabled", "budget_tokens": 10000});
            }
        }

        // 2. Strip effort param for models that don't support it (keep other output_config fields)
        // 9router parity: only delete effort field, then remove output_config only if empty
        if let Some(oc) = obj.get_mut("output_config") {
            if oc.get("effort").is_some() {
                if let Some(oc_obj) = oc.as_object_mut() {
                    oc_obj.remove("effort");
                    if oc_obj.is_empty() {
                        obj.remove("output_config");
                    }
                }
            }
        }
    }

    // 3. Hoist mid-conversation system messages into top-level system
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        let mut system_blocks: Vec<Value> = Vec::new();
        let mut kept = Vec::new();

        for msg in messages.drain(..) {
            if msg.get("role").and_then(Value::as_str) == Some("system") {
                let text = match msg.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|b| match b {
                            Value::String(s) => Some(s.clone()),
                            _ => b.get("text").and_then(Value::as_str).map(String::from),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                if !text.trim().is_empty() {
                    system_blocks.push(json!({"type": "text", "text": text}));
                }
            } else {
                kept.push(msg);
            }
        }

        if !system_blocks.is_empty() {
            // Prepend existing system if any
            let existing = match obj.remove("system") {
                Some(Value::Array(arr)) => arr,
                Some(Value::String(s)) if !s.trim().is_empty() => {
                    vec![json!({"type": "text", "text": s})]
                }
                _ => Vec::new(),
            };
            let merged: Vec<Value> = existing.into_iter().chain(system_blocks).collect();
            obj.insert("system".to_string(), Value::Array(merged));
        }

        obj.insert("messages".to_string(), Value::Array(kept));
    }

    // 4. Drop server_tool_use blocks with foreign ids + orphaned tool_result cleanup.
    // When combo/fallback routes through non-Claude providers first, the conversation
    // history may contain server_tool_use blocks with ids that don't match the
    // Claude `srvtoolu_*` prefix. Claude rejects these with 400, and any
    // tool_result referencing a dropped id must also be removed.
    // Mirrors claude.js:123-234 (hasForeignServerToolUseId + cleanup).
    let mut dropped_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        // Pass 1: detect and drop foreign server_tool_use in assistant messages
        for msg in messages.iter_mut() {
            if msg.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            let mut kept_blocks = Vec::with_capacity(content.len());
            for block in content.drain(..) {
                if block.get("type").and_then(Value::as_str) == Some("server_tool_use") {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                    if !is_valid_srvtoolu_id(id) {
                        dropped_ids.insert(id.to_string());
                        continue; // drop this block
                    }
                }
                kept_blocks.push(block);
            }
            *content = kept_blocks;
        }

        // Pass 2: remove orphaned tool_result / web_search_tool_result blocks
        if !dropped_ids.is_empty() {
            for msg in messages.iter_mut() {
                let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
                    continue;
                };
                let mut kept = Vec::with_capacity(content.len());
                for block in content.drain(..) {
                    let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
                    let is_result =
                        block_type == "tool_result" || block_type == "web_search_tool_result";
                    if is_result {
                        let ref_id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if dropped_ids.contains(ref_id) {
                            continue; // drop orphaned result
                        }
                    }
                    kept.push(block);
                }
                *content = kept;
            }
        }
    }
}

// ─── prepareClaudeRequest ───────────────────────────────────────────

/// Prepare a request body for a Claude-format endpoint.
///
/// - Drop `output_config` for providers with the quirk (MiniMax).
/// - Clamp `max_tokens` to [`DEFAULT_MAX_TOKENS`] (64k).
/// - Clean `cache_control` on system, messages, and tools.
/// - Filter empty messages; fix tool_use / tool_result ordering.
/// - Handle thinking blocks (signature validation for native Claude,
///   default-signature injection for `anthropic-compatible`).
/// - Apply cloaking for OAuth tokens.
pub fn prepare_claude_request(body: &mut Value, provider: &str, api_key: Option<&str>) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };

    // ── quirk: drop output_config for MiniMax ──
    if provider_drops_output_config(provider) {
        obj.remove("output_config");
    }

    // ── clamp max_tokens to model-aware ceiling ──
    let ceiling: u64 = {
        let model_lower = obj
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if model_lower.contains("opus") {
            128_000 // Opus: 128k max output (9router capabilities.js parity)
        } else if model_lower.contains("sonnet") {
            128_000
        } else {
            DEFAULT_MAX_TOKENS as u64 // 64_000 (haiku, unknown, non-Claude)
        }
    };
    if let Some(max_tokens) = obj.get("max_tokens").and_then(Value::as_u64) {
        if max_tokens > ceiling {
            obj.insert("max_tokens".to_string(), json!(ceiling));
        }
    }

    // ── thinking budget reconciliation (9router claude.js parity) ──
    // If the thinking budget is not strictly below max_tokens, Claude 400s —
    // raise max_tokens to budget + 1024 (capped at the ceiling) and shrink
    // the budget to max(1024, max_tokens - 1024).
    if obj
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(Value::as_str)
        == Some("enabled")
    {
        if let (Some(budget_tokens), Some(max_tokens)) = (
            obj.get("thinking")
                .and_then(|t| t.get("budget_tokens"))
                .and_then(Value::as_u64),
            obj.get("max_tokens").and_then(Value::as_u64),
        ) {
            if budget_tokens >= max_tokens {
                let new_max = (budget_tokens + 1024).min(ceiling);
                obj.insert("max_tokens".to_string(), json!(new_max));
                if budget_tokens >= new_max {
                    if let Some(v) = obj
                        .get_mut("thinking")
                        .and_then(Value::as_object_mut)
                        .and_then(|t| t.get_mut("budget_tokens"))
                    {
                        *v = json!(1024u64.max(new_max - 1024));
                    }
                }
            }
        }
    }

    // ── 1. System: strip all cache_control, add to last block ──
    if let Some(system) = obj.get_mut("system").and_then(Value::as_array_mut) {
        let len = system.len();
        for (i, block) in system.iter_mut().enumerate() {
            if let Some(block_obj) = block.as_object_mut() {
                block_obj.remove("cache_control");
                if i == len - 1 {
                    block_obj.insert(
                        "cache_control".to_string(),
                        json!({"type": "ephemeral", "ttl": "1h"}),
                    );
                }
            }
        }
    }

    // ── 2. Messages ──
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        let len = messages.len();

        // Pass 1: remove cache_control + filter empty messages
        let mut filtered: Vec<Value> = Vec::with_capacity(len);
        for (i, msg) in messages.drain(..).enumerate() {
            let mut msg = msg;
            // Remove cache_control from content blocks
            if let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) {
                for block in content.iter_mut() {
                    if let Some(block_obj) = block.as_object_mut() {
                        block_obj.remove("cache_control");
                    }
                }
            }

            // Keep final assistant even if empty, otherwise check valid content
            let is_final_assistant =
                i == len - 1 && msg.get("role").and_then(Value::as_str) == Some("assistant");
            if is_final_assistant || has_valid_content(&msg) {
                filtered.push(msg);
            }
        }

        // Pass 1.5: fix tool_use / tool_result ordering
        filtered = fix_tool_use_ordering(filtered);

        // Re-insert messages (we need to work with them for pass 2)
        *messages = filtered;
    }

    // Check if thinking is enabled AND last message is from user
    // (separate from the mutable borrow above)
    let thinking_enabled = obj
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(Value::as_str)
        == Some("enabled");
    let last_msg_is_user = obj
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|msgs| msgs.last())
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        == Some("user");
    let needs_thinking_tool_use = thinking_enabled && last_msg_is_user;

    // Pass 2 (reverse): cache_control on last assistant + thinking handling
    if let Some(messages) = obj.get_mut("messages").and_then(Value::as_array_mut) {
        let mut last_assistant_processed = false;
        let is_claude_native = provider == "claude" || provider == "anthropic";
        let is_anthropic_compatible =
            is_claude_native || provider.starts_with("anthropic-compatible");

        for msg in messages.iter_mut().rev() {
            if msg.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };

            // Add cache_control to last non-thinking block of first (from end) assistant with content
            if !last_assistant_processed && !content.is_empty() {
                for block in content.iter_mut().rev() {
                    let t = block.get("type").and_then(Value::as_str).unwrap_or("");
                    if t != "thinking" && t != "redacted_thinking" {
                        if let Some(block_obj) = block.as_object_mut() {
                            block_obj
                                .insert("cache_control".to_string(), json!({"type": "ephemeral"}));
                        }
                        break;
                    }
                }
                last_assistant_processed = true;
            }

            // Handle thinking blocks for Anthropic endpoint
            if is_anthropic_compatible {
                let mut has_tool_use = false;
                let mut has_thinking = false;

                let mut kept: Vec<Value> = Vec::with_capacity(content.len());
                for block in content.drain(..) {
                    let t = block.get("type").and_then(Value::as_str).unwrap_or("");
                    let is_thinking = t == "thinking" || t == "redacted_thinking";
                    if is_thinking {
                        has_thinking = true;
                        if is_claude_native {
                            // Preserve valid signatures, drop invalid blocks
                            let sig = block.get("signature").and_then(Value::as_str).unwrap_or("");
                            if !sig.is_empty() {
                                kept.push(block);
                            }
                            // else: drop invalid thinking blocks
                        } else {
                            // anthropic-compatible: replace with default signature
                            let mut b = block;
                            if let Some(block_obj) = b.as_object_mut() {
                                block_obj.insert(
                                    "signature".to_string(),
                                    Value::String(DEFAULT_THINKING_CLAUDE_SIGNATURE.to_string()),
                                );
                            }
                            kept.push(b);
                        }
                        continue;
                    }
                    if t == "tool_use" {
                        has_tool_use = true;
                    }
                    kept.push(block);
                }
                *content = kept;

                // Add thinking block if thinking enabled + has tool_use but no thinking
                if needs_thinking_tool_use && !has_thinking && has_tool_use {
                    content.insert(
                        0,
                        json!({
                            "type": "thinking",
                            "thinking": ".",
                            "signature": DEFAULT_THINKING_CLAUDE_SIGNATURE
                        }),
                    );
                }
            }
        }
    }

    // ── 3. Tools ──
    if let Some(tools) = obj.get_mut("tools").and_then(Value::as_array_mut) {
        // Filter built-in tools for non-Anthropic providers
        if provider != "claude" && provider != "anthropic" {
            *tools = tools
                .drain(..)
                .filter(|tool| {
                    let t = tool.get("type").and_then(Value::as_str).unwrap_or("");
                    t.is_empty() || t == "function"
                })
                .map(|tool| {
                    // Fold `function.{name,description,parameters}` → top-level Claude shape
                    if let Some(func) = tool.get("function") {
                        let name = func
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let description = func
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let input_schema = func
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
                        return json!({
                            "name": name,
                            "description": description,
                            "input_schema": input_schema
                        });
                    }
                    // Remove `type` field from native Claude tools
                    let mut t = tool;
                    if let Some(t_obj) = t.as_object_mut() {
                        t_obj.remove("type");
                    }
                    t
                })
                .collect();
        }

        // Rebuild tools with cache_control on the last cacheable one.
        // Skip tools with `defer_loading: true` — Anthropic rejects cache_control
        // on deferred tools (#3567). Mirrors lastCacheableToolIndex in claude.js:19-25.
        let cacheable_idx = last_cacheable_tool_index(tools);
        let mut rebuilt: Vec<Value> = Vec::with_capacity(tools.len());
        for (i, tool) in tools.drain(..).enumerate() {
            let mut t = tool;
            if let Some(t_obj) = t.as_object_mut() {
                t_obj.remove("cache_control");
                if Some(i) == cacheable_idx {
                    t_obj.insert(
                        "cache_control".to_string(),
                        json!({"type": "ephemeral", "ttl": "1h"}),
                    );
                }
            }
            rebuilt.push(t);
        }
        *tools = rebuilt;

        // Remove tools array and tool_choice if empty after filtering
        if tools.is_empty() {
            obj.remove("tools");
            obj.remove("tool_choice");
        }
    }

    // ── 4. Cloaking for OAuth tokens ──
    if let Some(api_key) = api_key {
        if (provider == "claude"
            || provider == "anthropic"
            || provider.starts_with("anthropic-compatible"))
            && !api_key.is_empty()
        {
            let session_id = get_cached_claude_headers()
                .and_then(|h| h.get("x-claude-code-session-id").cloned());
            // apply_cloaking takes &Value, returns Value — we need to rebuild
            let cloned = body.clone();
            let cloaked = apply_cloaking(&cloned, api_key, session_id.as_deref());
            *body = cloaked;
        }
    }
}

// ─── fixToolUseOrdering ──────────────────────────────────────────────

/// Fix tool_use / tool_result ordering for Claude API.
///
/// 1. Assistant message with tool_use: remove text blocks AFTER the first
///    tool_use (Claude doesn't allow text after tool_use).
/// 2. Merge consecutive same-role messages.
fn fix_tool_use_ordering(messages: Vec<Value>) -> Vec<Value> {
    if messages.is_empty() {
        return messages;
    }

    // Pass 1: Fix assistant messages — remove text after tool_use
    let mut fixed: Vec<Value> = Vec::with_capacity(messages.len());
    for msg in messages {
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let content = msg.get("content");
        let has_tool_use = content
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            })
            .unwrap_or(false);

        if role == "assistant" && has_tool_use {
            if let Some(Value::Array(arr)) = content {
                let mut new_content: Vec<Value> = Vec::new();
                let mut found_tool_use = false;
                for block in arr {
                    let t = block.get("type").and_then(Value::as_str).unwrap_or("");
                    if t == "tool_use" {
                        found_tool_use = true;
                        new_content.push(block.clone());
                    } else if t == "thinking" || t == "redacted_thinking" {
                        new_content.push(block.clone());
                    } else if !found_tool_use {
                        // Keep text blocks BEFORE tool_use
                        new_content.push(block.clone());
                    }
                    // Skip text blocks AFTER tool_use
                }
                let mut m = msg.clone();
                if let Some(m_obj) = m.as_object_mut() {
                    m_obj.insert("content".to_string(), Value::Array(new_content));
                }
                fixed.push(m);
            } else {
                fixed.push(msg);
            }
        } else {
            fixed.push(msg);
        }
    }

    // Pass 2: Merge consecutive same-role messages
    let mut merged: Vec<Value> = Vec::with_capacity(fixed.len());
    for msg in fixed {
        let role = msg
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let last = merged.last_mut();
        if let Some(last) = last {
            let last_role = last.get("role").and_then(Value::as_str).unwrap_or("");
            if last_role == role {
                // Merge content arrays
                let last_content = last
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let msg_content = msg
                    .get("content")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                let mut last_content = last_content;
                let mut msg_content = msg_content;

                // Put tool_result first, then other content
                let tool_results: Vec<Value> = last_content
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .cloned()
                    .chain(
                        msg_content
                            .iter()
                            .filter(|b| {
                                b.get("type").and_then(Value::as_str) == Some("tool_result")
                            })
                            .cloned(),
                    )
                    .collect();
                let other: Vec<Value> =
                    last_content
                        .into_iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) != Some("tool_result"))
                        .chain(msg_content.into_iter().filter(|b| {
                            b.get("type").and_then(Value::as_str) != Some("tool_result")
                        }))
                        .collect();

                let merged_content: Vec<Value> = tool_results.into_iter().chain(other).collect();
                if let Some(last_obj) = last.as_object_mut() {
                    last_obj.insert("content".to_string(), Value::Array(merged_content));
                }
                continue;
            }
        }
        // Ensure content is an array
        let content = msg
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                if let Some(s) = msg.get("content").and_then(Value::as_str) {
                    vec![json!({"type": "text", "text": s})]
                } else {
                    Vec::new()
                }
            });
        let mut m = msg.clone();
        if let Some(m_obj) = m.as_object_mut() {
            m_obj.insert("content".to_string(), Value::Array(content));
        }
        merged.push(m);
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── normalize_claude_passthrough ──────────────────────────────

    #[test]
    fn passthrough_downgrades_adaptive_thinking_on_haiku() {
        let mut body = json!({
            "model": "claude-sonnet-haiku-4.7",
            "thinking": {"type": "adaptive"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        normalize_claude_passthrough(&mut body, "claude-sonnet-haiku-4.7");
        assert_eq!(
            body["thinking"]["type"], "enabled",
            "adaptive → enabled on Haiku"
        );
        assert_eq!(body["thinking"]["budget_tokens"], 10000);
    }

    #[test]
    fn passthrough_strips_effort_on_haiku() {
        let mut body = json!({
            "model": "claude-sonnet-haiku-4.7",
            "output_config": {"effort": "high", "other": "x"},
            "messages": []
        });
        normalize_claude_passthrough(&mut body, "claude-sonnet-haiku-4.7");
        // 9router parity: only remove effort, keep other fields
        assert!(
            body.get("output_config").is_some(),
            "output_config kept when other fields remain"
        );
        assert!(
            body["output_config"].get("effort").is_none(),
            "effort field removed"
        );
        assert_eq!(
            body["output_config"]["other"], "x",
            "other fields preserved"
        );

        // Remove output_config entirely when only effort was present
        let mut body2 = json!({
            "output_config": {"effort": "high"},
            "messages": []
        });
        normalize_claude_passthrough(&mut body2, "claude-sonnet-haiku");
        assert!(
            body2.get("output_config").is_none(),
            "output_config removed when only effort"
        );

        // Keep output_config when present but no effort field
        let mut body3 = json!({
            "output_config": {"other": "x"},
            "messages": []
        });
        normalize_claude_passthrough(&mut body3, "claude-sonnet-haiku");
        assert!(
            body3.get("output_config").is_some(),
            "output_config kept when no effort"
        );
    }

    #[test]
    fn passthrough_hoists_system_messages() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "You are Claude."},
                {"role": "user", "content": "hi"}
            ]
        });
        normalize_claude_passthrough(&mut body, "claude-sonnet-4");
        assert!(body.get("system").is_some(), "system field should exist");
        assert_eq!(body["system"][0]["text"], "You are Claude.");
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn passthrough_hoists_system_messages_with_existing() {
        let mut body = json!({
            "system": [{"type": "text", "text": "Pre-existing."}],
            "messages": [
                {"role": "system", "content": "Inline system."},
                {"role": "user", "content": "hi"}
            ]
        });
        normalize_claude_passthrough(&mut body, "claude-sonnet-4");
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], "Pre-existing.");
        assert_eq!(sys[1]["text"], "Inline system.");
    }

    // ─── prepare_claude_request ────────────────────────────────────

    #[test]
    fn prepare_drops_output_config_for_minimax() {
        let mut body = json!({
            "output_config": {"effort": "high"},
            "messages": []
        });
        prepare_claude_request(&mut body, "minimax", None);
        assert!(body.get("output_config").is_none(), "output_config dropped");
    }

    #[test]
    fn prepare_clamps_max_tokens() {
        let mut body = json!({
            "max_tokens": 999999,
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "claude", None);
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn claude_format_reconciles_budget_after_clamp() {
        let mut body = json!({
            "model": "claude-sonnet-4.6",
            "max_tokens": 128000,
            "thinking": {"type": "enabled", "budget_tokens": 128000},
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "claude", None);
        // budget + 1024 capped at the sonnet ceiling (128000)
        assert_eq!(body["max_tokens"], 128000);
        // budget shrunk to max_tokens - 1024
        assert_eq!(body["thinking"]["budget_tokens"], 126976);
    }

    #[test]
    fn prepare_leaves_reasonable_max_tokens() {
        let mut body = json!({
            "max_tokens": 32000,
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "claude", None);
        assert_eq!(body["max_tokens"], 32000);
    }

    #[test]
    fn prepare_adds_cache_control_to_last_system_block() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "block1"},
                {"type": "text", "text": "block2"}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "claude", None);
        let sys = body["system"].as_array().unwrap();
        assert!(
            sys[0].get("cache_control").is_none(),
            "first block no cache_control"
        );
        assert!(
            sys[1].get("cache_control").is_some(),
            "last block has cache_control"
        );
        assert_eq!(sys[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn prepare_filters_empty_messages_but_keeps_final_assistant() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": ""},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": []}
            ]
        });
        prepare_claude_request(&mut body, "claude", None);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "empty user removed, final assistant kept");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn prepare_strips_cache_control_from_messages() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
                ]
            }]
        });
        prepare_claude_request(&mut body, "claude", None);
        assert!(body["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
    }

    #[test]
    fn prepare_adds_cache_control_to_last_non_thinking() {
        let mut body = json!({
            "thinking": {"type": "enabled", "budget_tokens": 1000},
            "messages": [
                {"role": "user", "content": "think hard"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm"},
                    {"type": "text", "text": "answer"}
                ]}
            ]
        });
        prepare_claude_request(&mut body, "claude", None);
        let content = body["messages"][1]["content"].as_array().unwrap();
        // The non-thinking (text) block should have cache_control
        let text_block = content.iter().find(|b| b["type"] == "text").unwrap();
        assert!(
            text_block.get("cache_control").is_some(),
            "last text gets cache_control"
        );
    }

    #[test]
    fn prepare_filter_builtin_tools_for_non_claude() {
        let mut body = json!({
            "tools": [
                {"name": "web_search_20250305", "type": "built_in"},
                {"name": "my_custom_tool", "description": "x", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "minimax", None);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1, "built-in tool should be filtered");
        assert_eq!(tools[0]["name"], "my_custom_tool");
    }

    #[test]
    fn prepare_adds_tool_cache_control_to_last() {
        let mut body = json!({
            "tools": [
                {"name": "a", "description": "x", "input_schema": {"type": "object"}},
                {"name": "b", "description": "y", "input_schema": {"type": "object"}}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "claude", None);
        let tools = body["tools"].as_array().unwrap();
        assert!(tools[0].get("cache_control").is_none());
        assert!(tools[1].get("cache_control").is_some());
    }

    #[test]
    fn prepare_removes_tools_when_empty() {
        let mut body = json!({
            "tools": [],
            "tool_choice": {"type": "auto"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "minimax", None);
        assert!(body.get("tools").is_none(), "empty tools removed");
        assert!(body.get("tool_choice").is_none(), "tool_choice removed");
    }

    // ─── fixToolUseOrdering ────────────────────────────────────────

    #[test]
    fn remove_text_after_tool_use() {
        let msgs = vec![json!({"role": "assistant", "content": [
            {"type": "text", "text": "before"},
            {"type": "tool_use", "id": "tu1", "name": "x", "input": {}},
            {"type": "text", "text": "after tool use"}
        ]})];
        let result = fix_tool_use_ordering(msgs);
        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "text after tool_use removed");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn merge_consecutive_same_role() {
        let msgs = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "a"}]}),
            json!({"role": "user", "content": [{"type": "text", "text": "b"}]}),
        ];
        let result = fix_tool_use_ordering(msgs);
        assert_eq!(result.len(), 1, "consecutive users merged");
        let content = result[0]["content"].as_array().unwrap();
        // tool_results first, so both texts should be after tool_results
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn merge_tool_result_first() {
        let msgs = vec![
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu1", "content": "result"},
                {"type": "text", "text": "follow-up"}
            ]}),
            json!({"role": "user", "content": [
                {"type": "text", "text": "more"}
            ]}),
        ];
        let result = fix_tool_use_ordering(msgs);
        assert_eq!(result.len(), 1);
        let content = result[0]["content"].as_array().unwrap();
        // tool_result should come before other content
        assert_eq!(content[0]["type"], "tool_result");
    }

    #[test]
    fn do_not_merge_different_roles() {
        let msgs = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            json!({"role": "assistant", "content": [{"type": "text", "text": "hello"}]}),
        ];
        let result = fix_tool_use_ordering(msgs);
        assert_eq!(result.len(), 2);
    }

    // ─── defer_loading cache breakpoint ─────────────────────────────

    #[test]
    fn last_cacheable_tool_index_skips_deferred() {
        let tools = vec![
            json!({"name": "a", "defer_loading": true}),
            json!({"name": "b"}),
            json!({"name": "c", "defer_loading": true}),
        ];
        assert_eq!(last_cacheable_tool_index(&tools), Some(1));
    }

    #[test]
    fn last_cacheable_tool_index_all_deferred() {
        let tools = vec![
            json!({"name": "a", "defer_loading": true}),
            json!({"name": "b", "defer_loading": true}),
        ];
        assert_eq!(last_cacheable_tool_index(&tools), None);
    }

    #[test]
    fn prepare_skips_deferred_tool_for_cache_control() {
        let mut body = json!({
            "tools": [
                {"name": "a", "description": "x", "input_schema": {"type": "object"}},
                {"name": "b", "description": "y", "input_schema": {"type": "object"}, "defer_loading": true}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        });
        prepare_claude_request(&mut body, "claude", None);
        let tools = body["tools"].as_array().unwrap();
        // tool "a" (index 0) is last cacheable → gets cache_control
        assert!(
            tools[0].get("cache_control").is_some(),
            "first tool gets cache_control"
        );
        // tool "b" (index 1) has defer_loading → no cache_control
        assert!(
            tools[1].get("cache_control").is_none(),
            "deferred tool skips cache_control"
        );
    }

    // ─── [1m] context marker ────────────────────────────────────────

    #[test]
    fn strip_model_context_marker_basic() {
        assert_eq!(
            strip_model_context_marker("claude-opus-5[1m]"),
            ("claude-opus-5".to_string(), Some("1m"))
        );
        assert_eq!(
            strip_model_context_marker("claude-opus-5[1M]"),
            ("claude-opus-5".to_string(), Some("1m"))
        );
        assert_eq!(
            strip_model_context_marker("claude-sonnet-4"),
            ("claude-sonnet-4".to_string(), None)
        );
    }

    // ─── server_tool_use foreign id drop ────────────────────────────

    #[test]
    fn is_valid_srvtoolu_id_valid() {
        assert!(is_valid_srvtoolu_id("srvtoolu_abc123"));
        assert!(is_valid_srvtoolu_id("srvtoolu_1"));
    }

    #[test]
    fn is_valid_srvtoolu_id_invalid() {
        assert!(!is_valid_srvtoolu_id("srvtoolu_"));
        assert!(!is_valid_srvtoolu_id("foreign_tool_xyz"));
        assert!(!is_valid_srvtoolu_id(""));
    }

    #[test]
    fn passthrough_drops_foreign_server_tool_use() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [
                    {"type": "server_tool_use", "id": "srvtoolu_valid1"},
                    {"type": "server_tool_use", "id": "foreign_xyz"}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "srvtoolu_valid1", "content": "ok"},
                    {"type": "tool_result", "tool_use_id": "foreign_xyz", "content": "orphaned"}
                ]}
            ]
        });
        normalize_claude_passthrough(&mut body, "claude-sonnet-4");
        let assistant = &body["messages"][1];
        let content = assistant["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "foreign server_tool_use dropped");
        assert_eq!(content[0]["id"], "srvtoolu_valid1");

        let user = &body["messages"][2];
        let user_content = user["content"].as_array().unwrap();
        assert_eq!(user_content.len(), 1, "orphaned tool_result dropped");
        assert_eq!(user_content[0]["tool_use_id"], "srvtoolu_valid1");
    }

    // ─── has_valid_content IMAGE/DOCUMENT ────────────────────────────

    #[test]
    fn has_valid_content_image_block() {
        let msg = json!({"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "data": "abc"}}
        ]});
        assert!(
            has_valid_content(&msg),
            "image block should be valid content"
        );
    }

    #[test]
    fn has_valid_content_document_block() {
        let msg = json!({"role": "user", "content": [
            {"type": "document", "source": {"type": "base64", "data": "abc"}, "media_type": "application/pdf"}
        ]});
        assert!(
            has_valid_content(&msg),
            "document block should be valid content"
        );
    }
}
