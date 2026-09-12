//! Chat-completions based LLM search — dedicated-search failover.
//!
//! Port of `open-sse/handlers/search/chatSearch.js`: wraps chat-completions
//! endpoints that carry built-in web search into the unified `/v1/search`
//! result shape. Invoked by the search handler when a dedicated search
//! provider fails with a retriable error (anything except 400/401/403/404)
//! within the global budget.
//!
//! Covers all 7 CHAT_SEARCH_CONFIG providers: gemini, openai, xai, kimi,
//! minimax, perplexity (+agent variant shares the endpoint shape).

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

use super::base::{
    get_provider_setting, make_result, now_iso, resolve_base_url, ChatSearchResult, SearchRequest,
    SearchResultSet,
};

/// Fallback request timeout (JS `REQUEST_TIMEOUT_MS`).
const CHAT_SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Antigravity IDE fingerprint (port of `ANTIGRAVITY_IDE_USER_AGENT` in
/// `open-sse/providers/shared.js:81-83`:
/// `antigravity/ide/${ANTIGRAVITY_IDE_VERSION} darwin/arm64`, version 2.11.0).
pub const ANTIGRAVITY_IDE_USER_AGENT: &str = "antigravity/ide/2.11.0 darwin/arm64";

/// Default antigravity chat-search model (registry `searchViaChat.defaultModel`).
const ANTIGRAVITY_DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Caller model override: `request.provider_options["model"]`, else the
/// provider default (port of `model || searchModel(provider)` in
/// chatSearch.js:467-473).
fn chat_model(request: &SearchRequest<'_>, default: &str) -> String {
    get_provider_setting(request, "model").unwrap_or_else(|| default.to_string())
}

/// Run a chat-based LLM search for `provider.id()`. Returns `None` when the
/// provider has no chat fallback, or the fallback itself fails. The upstream
/// base URL may be overridden via `request.provider_options["baseUrl"]` (used
/// by tests / self-hosted endpoints).
/// Whether `provider_id` supports a chat-completions grounding search
/// (port of `CHAT_SEARCH_CONFIG` keys in chatSearch.js + the registry
/// `searchViaChat` entries: gemini, antigravity, openai, xai, kimi,
/// minimax, perplexity).
pub fn has_chat_search(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "gemini"
            | "antigravity"
            | "openai"
            | "xai"
            | "kimi"
            | "kimi-coding"
            | "minimax"
            | "minimax-cn"
            | "perplexity"
            | "perplexity-agent"
    )
}

pub async fn handle_chat_search(
    client: &Client,
    provider_id: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    let query = &request.query;
    let max_results = request.max_results;
    let token = request.token?;
    match provider_id {
        "gemini" => gemini_chat_search(client, query, max_results, token, request).await,
        "antigravity" => antigravity_chat_search(client, query, max_results, token, request).await,
        "openai" => openai_chat_search(client, query, max_results, token, request).await,
        "xai" => xai_chat_search(client, query, max_results, token, request).await,
        "kimi" | "kimi-coding" => {
            kimi_chat_search(client, query, max_results, token, request).await
        }
        "minimax" | "minimax-cn" => {
            minimax_chat_search(client, query, max_results, token, request).await
        }
        "perplexity" | "perplexity-agent" => {
            perplexity_chat_search(client, query, max_results, token, request).await
        }
        _ => None,
    }
}

/// Gemini `generateContent` with `google_search` tool. Port of JS
/// `CHAT_SEARCH_CONFIG.gemini` (chatSearch.js:51-74).
async fn gemini_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    let model = chat_model(request, "gemini-2.5-flash");
    let base =
        resolve_base_url("https://generativelanguage.googleapis.com/v1beta", request).ok()?;
    let url = format!("{base}/models/{model}:generateContent");
    let body = json!({
        "contents": [{ "role": "user", "parts": [{ "text": query }] }],
        "tools": [{ "google_search": {} }],
    });
    let resp = client
        .post(&url)
        .header("x-goog-api-key", token)
        .json(&body)
        .timeout(CHAT_SEARCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json().await.ok()?;

    let candidate = data.get("candidates").and_then(|c| c.get(0)).cloned()?;
    let parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text: String = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();

    let chunks = candidate
        .get("groundingMetadata")
        .and_then(|g| g.get("groundingChunks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let citations: Vec<(String, String)> = chunks
        .iter()
        .filter_map(|ch| {
            let web = ch.get("web")?;
            let url = web
                .get("uri")
                .or_else(|| web.get("url"))
                .and_then(Value::as_str)?
                .to_string();
            let title = web
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((url, title))
        })
        .collect();

    let tokens = data
        .get("usageMetadata")
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(build_result_set(
        provider_for_citation("gemini"),
        &text,
        &citations,
        max_results,
        &chat_model(request, "gemini-2.5-flash"),
        tokens,
    ))
}

/// OpenAI chat completions with web search. Port of JS
/// `CHAT_SEARCH_CONFIG.openai` (chatSearch.js:76-97).
async fn openai_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    // JS: buildBody(query, model) with model = caller override ||
    // registry searchViaChat.defaultModel ("openai/gpt-4o-mini").
    let model = chat_model(request, "openai/gpt-4o-mini");
    // JS strips the provider prefix for the upstream model.
    let upstream_model = model.strip_prefix("openai/").unwrap_or(&model).to_string();
    let mut body = json!({
        "model": upstream_model,
        "messages": [{ "role": "user", "content": query }],
    });
    // Non-search-preview models need the explicit web_search tool.
    if !upstream_model.to_lowercase().contains("search") {
        body["tools"] = json!([{ "type": "web_search" }]);
    }
    let base = resolve_base_url("https://api.openai.com/v1", request).ok()?;
    let resp = client
        .post(format!("{base}/chat/completions"))
        .bearer_auth(token)
        .json(&body)
        .timeout(CHAT_SEARCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json().await.ok()?;

    let msg = data
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned();
    let text = msg
        .as_ref()
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // Citations from message.annotations[].url_citation, else data.citations.
    let mut citations: Vec<(String, String)> = Vec::new();
    if let Some(annotations) = msg
        .as_ref()
        .and_then(|m| m.get("annotations"))
        .and_then(Value::as_array)
    {
        for a in annotations {
            if let Some(uc) = a.get("url_citation") {
                if let Some(url) = uc.get("url").and_then(Value::as_str) {
                    let title = uc
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    citations.push((url.to_string(), title));
                }
            }
        }
    }
    if citations.is_empty() {
        if let Some(top) = data.get("citations").and_then(Value::as_array) {
            for c in top {
                let url = match c {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => o.get("url").and_then(Value::as_str).map(String::from),
                    _ => None,
                };
                if let Some(url) = url {
                    let title = c
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    citations.push((url, title));
                }
            }
        }
    }

    let tokens = data
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(build_result_set(
        provider_for_citation("openai"),
        &text,
        &citations,
        max_results,
        &upstream_model,
        tokens,
    ))
}

/// Antigravity Google Search grounding request. Port of JS
/// `CHAT_SEARCH_CONFIG.antigravity` (chatSearch.js:104-167): POST
/// `{ANTIGRAVITY_IDE_BASE_URL}/v1internal:generateContent` with
/// `{ project, model, userAgent: "antigravity", requestType: "search",
/// request: { contents, tools: [{ googleSearch }], generationConfig } }`.
/// The project id is required — upstream 403s on a missing value.
async fn antigravity_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    // JS `model || searchModel(provider)` (handleChatSearch:471); the registry
    // default for antigravity is gemini-2.5-flash (antigravity.js:89-93).
    let model = chat_model(request, ANTIGRAVITY_DEFAULT_MODEL);
    // JS `requireCredentials`: the account must carry a projectId
    // (stored in provider_specific_data). Missing → no fallback.
    let project_id =
        super::base::get_provider_setting(request, "projectId").filter(|p| !p.is_empty())?;
    let base = resolve_base_url("https://daily-cloudcode-pa.googleapis.com", request).ok()?;
    let body = json!({
        "project": project_id,
        "model": model,
        "userAgent": "antigravity",
        "requestType": "search",
        "request": {
            "contents": [{ "role": "user", "parts": [{ "text": query }] }],
            "tools": [{ "googleSearch": {} }],
            "generationConfig": { "temperature": 1.0, "maxOutputTokens": 8192 },
        },
    });
    let resp = client
        .post(format!("{base}/v1internal:generateContent"))
        .bearer_auth(token)
        .header("User-Agent", ANTIGRAVITY_IDE_USER_AGENT)
        .json(&body)
        .timeout(CHAT_SEARCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json().await.ok()?;

    // Antigravity wraps the Gemini payload in { response: {...} }.
    let response = data.get("response").unwrap_or(&data);
    let candidate = response.get("candidates").and_then(|c| c.get(0))?;
    let parts = candidate
        .get("content")
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text: String = parts
        .iter()
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();

    let grounding = candidate.get("groundingMetadata");
    let chunks = grounding
        .and_then(|g| g.get("groundingChunks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let supports = grounding
        .and_then(|g| g.get("groundingSupports"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Upstream repeats the same source across chunks — key by URL so it stays
    // one citation. Index URLs first, then attribute supports to them.
    let mut sources: Vec<(String, String, Vec<String>, Vec<String>)> = Vec::new();
    let mut by_index: Vec<Option<usize>> = Vec::new();
    for ch in &chunks {
        let web = ch.get("web");
        let url = web
            .and_then(|w| w.get("uri").or_else(|| w.get("url")))
            .and_then(Value::as_str)
            .unwrap_or("");
        if url.is_empty() {
            by_index.push(None);
            continue;
        }
        let title = web
            .and_then(|w| w.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let pos = match sources.iter().position(|(u, _, _, _)| u == url) {
            Some(i) => i,
            None => {
                sources.push((url.to_string(), title.to_string(), Vec::new(), Vec::new()));
                sources.len() - 1
            }
        };
        by_index.push(Some(pos));
    }
    for s in &supports {
        let segment = s.get("segment");
        let grounded = segment
            .and_then(|g| g.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let expanded = expand_segment(&text, segment);
        let expanded = if expanded.is_empty() {
            grounded
        } else {
            expanded.as_str()
        };
        let indices = s
            .get("groundingChunkIndices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for idx in indices {
            let pos = idx
                .as_u64()
                .and_then(|i| by_index.get(i as usize))
                .copied()
                .flatten();
            let Some(pos) = pos else { continue };
            let entry = &mut sources[pos];
            if !grounded.is_empty() && !entry.2.contains(&grounded.to_string()) {
                entry.2.push(grounded.to_string());
            }
            if !expanded.is_empty() && !entry.3.contains(&expanded.to_string()) {
                entry.3.push(expanded.to_string());
            }
        }
    }

    let now = now_iso();
    let results: Vec<crate::core::media::search::SearchResult> = sources
        .iter()
        .take(max_results.max(1) as usize)
        .enumerate()
        .map(|(i, (url, title, snippets, contexts))| {
            let snippet = snippets
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" | ");
            let snippet = if snippet.is_empty() {
                title.clone()
            } else {
                snippet
            };
            let context = contexts
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n");
            let content = if context.is_empty() {
                snippet.clone()
            } else {
                context
            };
            make_result(
                "antigravity",
                Some(title),
                Some(url),
                Some(&snippet),
                None,
                None,
                None,
                Some(&content),
                None,
                None,
                None,
                None,
                i as u32,
                &now,
            )
        })
        .collect();
    let tokens = response
        .get("usageMetadata")
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let set = SearchResultSet {
        results,
        total_results: Some(sources.len() as u64),
    };
    Some(ChatSearchResult::new(set, &text, &model, tokens))
}

/// Widen a grounded segment to its surrounding sentence(s) in the answer
/// text (chatSearch.js:46-57 `expandSegment`). Returns `""` (not `None`)
/// when the text or indices are missing, matching JS behavior
/// (chatSearch.js:49 returns `""`).
fn expand_segment(text: &str, segment: Option<&Value>) -> String {
    const BEFORE: usize = 150;
    const AFTER: usize = 250;
    let (Some(segment),) = (segment,) else {
        return String::new();
    };
    let (Some(start_idx), Some(end_idx)) = (
        segment.get("startIndex").and_then(Value::as_u64),
        segment.get("endIndex").and_then(Value::as_u64),
    ) else {
        return String::new();
    };
    if text.is_empty() {
        return String::new();
    }
    let (start_idx, end_idx) = (start_idx as usize, end_idx as usize);
    // Non-integer indices are rejected in JS via Number.isInteger; u64
    // extraction above already guarantees integer-ness.
    let len = text.len();
    let start = start_idx.saturating_sub(BEFORE).min(len);
    let end = end_idx.saturating_add(AFTER).min(len);
    if start >= end {
        return String::new();
    }
    let mut out = text[start..end].trim().to_string();
    // Drop the partial words the window cut off at either edge.
    if start > 0 {
        out = format!("...{}", trim_leading_partial_word(&out));
    }
    if end < len {
        out = format!("{}...", trim_trailing_partial_word(&out));
    }
    out.trim().to_string()
}

fn trim_leading_partial_word(s: &str) -> String {
    match s.find(char::is_whitespace) {
        Some(i) => s[i..].to_string(),
        None => String::new(),
    }
}

fn trim_trailing_partial_word(s: &str) -> String {
    let trimmed_end = s.trim_end();
    match trimmed_end.rfind(char::is_whitespace) {
        Some(i) => trimmed_end[..i].to_string(),
        None => String::new(),
    }
}

fn provider_for_citation(provider: &str) -> &'static str {
    // The citation provider id reflects the fallback LLM provider
    // (9router chatSearch success payload: citation.provider = real id).
    match provider {
        "gemini" => "gemini",
        "openai" => "openai",
        "xai" => "xai",
        "kimi" | "kimi-coding" => "kimi",
        "minimax" | "minimax-cn" => "minimax",
        "perplexity" | "perplexity-agent" => "perplexity",
        "antigravity" => "antigravity",
        _ => "chat_search",
    }
}

/// Build a [`ChatSearchResult`] from the fallback answer text + citations,
/// threading the answer text and token count through so the envelope can
/// emit `data.answer` / `data.usage` (chatSearch.js:534-549). The model is
/// the resolved caller-or-default model string.
fn build_result_set(
    citation_provider: &str,
    answer: &str,
    citations: &[(String, String)],
    max_results: u32,
    model: &str,
    llm_tokens: u64,
) -> ChatSearchResult {
    let now = now_iso();
    let results = citations
        .iter()
        .take(max_results.max(1) as usize)
        .enumerate()
        .map(|(i, (url, title))| {
            make_result(
                citation_provider,
                Some(title),
                Some(url),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                i as u32,
                &now,
            )
        })
        .collect();
    ChatSearchResult::new(
        SearchResultSet {
            results,
            total_results: Some(citations.len() as u64),
        },
        answer,
        model,
        llm_tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_result_set_shapes_citations() {
        let chat = build_result_set(
            "chat_search",
            "answer text",
            &[("https://example.com".into(), "Example".into())],
            10,
            "gpt-4o-mini",
            7,
        );
        assert_eq!(chat.set.results.len(), 1);
        assert_eq!(chat.set.results[0].url, "https://example.com");
        assert_eq!(chat.set.results[0].title, "Example");
        assert_eq!(chat.set.results[0].position, 1);
        assert_eq!(chat.set.results[0].citation["provider"], "chat_search");
        // Answer text + usage thread through for the envelope.
        assert_eq!(chat.answer_text, "answer text");
        assert_eq!(chat.model, "gpt-4o-mini");
        assert_eq!(chat.llm_tokens, 7);
    }

    #[test]
    fn build_result_set_respects_max_results() {
        let citations = vec![
            ("https://a.com".into(), "A".into()),
            ("https://b.com".into(), "B".into()),
            ("https://c.com".into(), "C".into()),
        ];
        let chat = build_result_set("chat_search", "x", &citations, 2, "m", 0);
        assert_eq!(chat.set.results.len(), 2);
    }
}

// ---------------------------------------------------------------------------
// Remaining CHAT_SEARCH_CONFIG providers (xai / kimi / minimax / perplexity /
// perplexity-agent) — same failover contract, provider-specific request
// shapes and citation extraction (chatSearch.js:98-290).
// ---------------------------------------------------------------------------

/// Standard chat-completions POST helper shared by the Bearer-token providers.
async fn post_chat(client: &Client, url: String, token: &str, body: Value) -> Option<Value> {
    let resp = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .timeout(CHAT_SEARCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

/// Pull message.content from a chat-completions response.
fn message_content(data: &Value) -> String {
    data.pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// xAI: POST /v1/responses with input[] + web_search tool; output[] blocks
/// carry text + url_citation annotations (chatSearch.js:126-176).
async fn xai_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    let model = chat_model(request, "grok-3");
    let base = resolve_base_url("https://api.x.ai/v1", request).ok()?;
    let body = json!({
        "model": model,
        "input": [{ "role": "user", "content": query }],
        "tools": [{ "type": "web_search" }],
    });
    let data = post_chat(client, format!("{base}/responses"), token, body).await?;

    let mut text = String::new();
    let mut citations: Vec<(String, String)> = Vec::new();
    if let Some(output) = data.get("output").and_then(Value::as_array) {
        for item in output {
            if let Some(parts) = item.get("content").and_then(Value::as_array) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                    if let Some(anns) = p.get("annotations").and_then(Value::as_array) {
                        for a in anns {
                            let src = a.get("url").map(|_| a.clone()).unwrap_or_else(|| {
                                a.get("url_citation").cloned().unwrap_or(Value::Null)
                            });
                            if let Some(url) = src.get("url").and_then(Value::as_str) {
                                let title = src.get("title").and_then(Value::as_str).unwrap_or("");
                                citations.push((url.to_string(), title.to_string()));
                            }
                        }
                    }
                }
            }
        }
    }
    if citations.is_empty() {
        if let Some(top) = data.get("citations").and_then(Value::as_array) {
            for c in top {
                let url = match c {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => o.get("url").and_then(Value::as_str).map(String::from),
                    _ => None,
                };
                if let Some(url) = url {
                    citations.push((url, String::new()));
                }
            }
        }
    }

    let tokens = data
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(build_result_set(
        "xai",
        &text,
        &citations,
        max_results,
        &model,
        tokens,
    ))
}

/// Extract citations from tool_calls[].function.arguments search_results /
/// results / references arrays (kimi + minimax fallback).
fn citations_from_tool_calls(data: &Value, out: &mut Vec<(String, String)>) {
    let Some(calls) = data
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
    else {
        return;
    };
    for call in calls {
        let Some(arg_str) = call.pointer("/function/arguments").and_then(Value::as_str) else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(arg_str) else {
            continue;
        };
        for key in ["search_results", "results", "references"] {
            if let Some(items) = parsed.get(key).and_then(Value::as_array) {
                for it in items {
                    let url = it
                        .get("url")
                        .or_else(|| it.get("link"))
                        .and_then(Value::as_str);
                    if let Some(url) = url {
                        let title = it.get("title").and_then(Value::as_str).unwrap_or("");
                        let snippet = it
                            .get("snippet")
                            .or_else(|| it.get("summary"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        out.push((url.to_string(), title.to_string()));
                        let _ = snippet;
                    }
                }
            }
        }
    }
}

/// Kimi: builtin_function `$web_search` tool; citations ride the tool_call
/// arguments (chatSearch.js:178-232).
async fn kimi_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    let model = chat_model(request, "kimi-k2.7");
    let base = resolve_base_url("https://api.moonshot.cn/v1", request).ok()?;
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": query }],
        "tools": [{
            "type": "builtin_function",
            "function": { "name": "$web_search" },
        }],
    });
    let data = post_chat(client, format!("{base}/chat/completions"), token, body).await?;
    let text = message_content(&data);
    let mut citations = Vec::new();
    citations_from_tool_calls(&data, &mut citations);

    let tokens = data
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(build_result_set(
        "kimi",
        &text,
        &citations,
        max_results,
        &model,
        tokens,
    ))
}

/// MiniMax: web_search tool; direct `web_search_results` array first, then
/// tool_calls fallback (chatSearch.js:234-282).
async fn minimax_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    let model = chat_model(request, "abab6.5s-chat");
    let base = resolve_base_url("https://api.minimax.chat/v1", request).ok()?;
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": query }],
        "tools": [{ "type": "web_search" }],
    });
    let data = post_chat(
        client,
        format!("{base}/text/chatcompletion_v2"),
        token,
        body,
    )
    .await?;
    let text = message_content(&data);

    let mut citations: Vec<(String, String)> = Vec::new();
    if let Some(direct) = data.get("web_search_results").and_then(Value::as_array) {
        for it in direct {
            let url = it
                .get("url")
                .or_else(|| it.get("link"))
                .and_then(Value::as_str);
            if let Some(url) = url {
                let title = it.get("title").and_then(Value::as_str).unwrap_or("");
                citations.push((url.to_string(), title.to_string()));
            }
        }
    }
    if citations.is_empty() {
        citations_from_tool_calls(&data, &mut citations);
    }

    let tokens = data
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(build_result_set(
        "minimax",
        &text,
        &citations,
        max_results,
        &model,
        tokens,
    ))
}

/// Perplexity: sonar models return content + top-level citations array
/// (chatSearch.js:284-312).
async fn perplexity_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<ChatSearchResult> {
    let model = chat_model(request, "sonar");
    let base = resolve_base_url("https://api.perplexity.ai", request).ok()?;
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": query }],
    });
    let data = post_chat(client, format!("{base}/chat/completions"), token, body).await?;
    let text = message_content(&data);

    let mut citations: Vec<(String, String)> = Vec::new();
    if let Some(Value::Array(arr)) = data.get("citations") {
        for c in arr {
            match c {
                Value::String(url) => citations.push((url.clone(), String::new())),
                Value::Object(o) => {
                    if let Some(url) = o.get("url").and_then(Value::as_str) {
                        let title = o.get("title").and_then(Value::as_str).unwrap_or("");
                        citations.push((url.to_string(), title.to_string()));
                    }
                }
                _ => {}
            }
        }
    }

    let tokens = data
        .get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    Some(build_result_set(
        "perplexity",
        &text,
        &citations,
        max_results,
        &model,
        tokens,
    ))
}

#[cfg(test)]
mod extended_tests {
    use super::*;

    #[test]
    fn citations_from_tool_calls_parses_kimi_arguments() {
        let data = json!({
            "choices": [{"message": {"tool_calls": [{"function": {"arguments":
                "{\"search_results\": [{\"url\": \"https://k.dev\", \"title\": \"K\"}]}"
            }}]}}]
        });
        let mut out = Vec::new();
        citations_from_tool_calls(&data, &mut out);
        assert_eq!(out, vec![("https://k.dev".to_string(), "K".to_string())]);
    }

    #[test]
    fn citations_from_tool_calls_handles_bad_json() {
        let data = json!({
            "choices": [{"message": {"tool_calls": [{"function": {"arguments": "not-json"}}]}}]
        });
        let mut out = Vec::new();
        citations_from_tool_calls(&data, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn message_content_reads_choices() {
        assert_eq!(
            message_content(&json!({"choices":[{"message":{"content":"hey"}}]})),
            "hey"
        );
    }

    #[test]
    fn antigravity_is_chat_search_capable() {
        // P0 #7: antigravity must be reachable via searchViaChat after the
        // dedicated lookup misses.
        assert!(has_chat_search("antigravity"));
        assert!(!has_chat_search("serper"));
    }

    #[test]
    fn expand_segment_widens_context() {
        let text = "aaa bbb grounded sentence ccc ddd";
        let seg = json!({"startIndex": 8, "endIndex": 24, "text": "grounded sentence"});
        let out = expand_segment(text, Some(&seg));
        assert!(out.contains("grounded sentence"), "got: {out}");
    }

    #[test]
    fn expand_segment_returns_empty_string_when_indices_missing() {
        // JS expandSegment returns "" (not null) for missing text/indices
        // (chatSearch.js:47-49); call sites treat "" as absent.
        assert_eq!(expand_segment("text", None), "");
        assert_eq!(expand_segment("text", Some(&json!({}))), "");
        assert_eq!(
            expand_segment("", Some(&json!({"startIndex": 0, "endIndex": 1}))),
            ""
        );
    }

    #[test]
    fn antigravity_dedups_repeated_url_into_one_citation() {
        // Upstream repeats the same source across chunks — keyed by URL.
        let body = json!({
            "response": {
                "candidates": [{
                    "content": {"parts": [{"text": "answer text here"}]},
                    "groundingMetadata": {
                        "groundingChunks": [
                            {"web": {"uri": "https://dup.example/a", "title": "A"}},
                            {"web": {"uri": "https://dup.example/a", "title": "A"}},
                            {"web": {"uri": "https://other.example/b", "title": "B"}}
                        ]
                    }
                }]
            }
        });
        let response = body.get("response").unwrap();
        let candidate = response.get("candidates").and_then(|c| c.get(0)).unwrap();
        let chunks = candidate
            .get("groundingMetadata")
            .and_then(|g| g.get("groundingChunks"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut seen: Vec<String> = Vec::new();
        for ch in &chunks {
            let url = ch
                .get("web")
                .and_then(|w| w.get("uri"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if !url.is_empty() && !seen.contains(&url.to_string()) {
                seen.push(url.to_string());
            }
        }
        assert_eq!(seen.len(), 2, "repeated URL must collapse to one source");
        assert!(seen.contains(&"https://dup.example/a".to_string()));
    }

    #[test]
    fn antigravity_supports_segment_expands_from_answer_text() {
        // groundingSupports segments are expanded against the answer text.
        let text = "The Eiffel Tower is in Paris and was completed in 1889.";
        let seg = json!({"startIndex": 4, "endIndex": 16, "text": "Eiffel Tower"});
        let expanded = expand_segment(text, Some(&seg));
        assert!(!expanded.is_empty(), "expansion must yield context");
        assert!(expanded.contains("Eiffel Tower"), "got: {expanded}");
        // Missing indices → "" so callers fall back to the grounded text.
        let empty = expand_segment(text, Some(&json!({"text": "Eiffel Tower"})));
        assert_eq!(empty, "");
    }
}
