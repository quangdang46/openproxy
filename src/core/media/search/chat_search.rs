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

use super::base::{make_result, now_iso, resolve_base_url, SearchRequest, SearchResultSet};

/// Fallback request timeout (JS `REQUEST_TIMEOUT_MS`).
const CHAT_SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Run a chat-based LLM search for `provider.id()`. Returns `None` when the
/// provider has no chat fallback, or the fallback itself fails. The upstream
/// base URL may be overridden via `request.provider_options["baseUrl"]` (used
/// by tests / self-hosted endpoints).
pub async fn handle_chat_search(
    client: &Client,
    provider_id: &str,
    request: &SearchRequest<'_>,
) -> Option<SearchResultSet> {
    let query = &request.query;
    let max_results = request.max_results;
    let token = request.token?;
    match provider_id {
        "gemini" => gemini_chat_search(client, query, max_results, token, request).await,
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
) -> Option<SearchResultSet> {
    const MODEL: &str = "gemini-2.5-flash";
    let base =
        resolve_base_url("https://generativelanguage.googleapis.com/v1beta", request).ok()?;
    let url = format!("{base}/models/{MODEL}:generateContent");
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

    Some(build_result_set(
        provider_for_citation("gemini"),
        &text,
        &citations,
        max_results,
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
) -> Option<SearchResultSet> {
    const MODEL: &str = "openai/gpt-4o-mini";
    // JS strips the provider prefix for the upstream model.
    let upstream_model = MODEL.strip_prefix("openai/").unwrap_or(MODEL);
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

    Some(build_result_set(
        provider_for_citation("openai"),
        &text,
        &citations,
        max_results,
    ))
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
        _ => "chat_search",
    }
}

/// Build a unified `SearchResultSet` from the fallback answer text + citations.
fn build_result_set(
    citation_provider: &str,
    _answer: &str,
    citations: &[(String, String)],
    max_results: u32,
) -> SearchResultSet {
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
    SearchResultSet {
        results,
        total_results: Some(citations.len() as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_result_set_shapes_citations() {
        let set = build_result_set(
            "chat_search",
            "answer text",
            &[("https://example.com".into(), "Example".into())],
            10,
        );
        assert_eq!(set.results.len(), 1);
        assert_eq!(set.results[0].url, "https://example.com");
        assert_eq!(set.results[0].title, "Example");
        assert_eq!(set.results[0].position, 1);
        assert_eq!(set.results[0].citation["provider"], "chat_search");
    }

    #[test]
    fn build_result_set_respects_max_results() {
        let citations = vec![
            ("https://a.com".into(), "A".into()),
            ("https://b.com".into(), "B".into()),
            ("https://c.com".into(), "C".into()),
        ];
        let set = build_result_set("chat_search", "x", &citations, 2);
        assert_eq!(set.results.len(), 2);
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
) -> Option<SearchResultSet> {
    const MODEL: &str = "grok-3";
    let base = resolve_base_url("https://api.x.ai/v1", request).ok()?;
    let body = json!({
        "model": MODEL,
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

    Some(build_result_set("xai", &text, &citations, max_results))
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
) -> Option<SearchResultSet> {
    const MODEL: &str = "kimi-k2.7";
    let base = resolve_base_url("https://api.moonshot.cn/v1", request).ok()?;
    let body = json!({
        "model": MODEL,
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

    Some(build_result_set("kimi", &text, &citations, max_results))
}

/// MiniMax: web_search tool; direct `web_search_results` array first, then
/// tool_calls fallback (chatSearch.js:234-282).
async fn minimax_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<SearchResultSet> {
    const MODEL: &str = "abab6.5s-chat";
    let base = resolve_base_url("https://api.minimax.chat/v1", request).ok()?;
    let body = json!({
        "model": MODEL,
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

    Some(build_result_set("minimax", &text, &citations, max_results))
}

/// Perplexity: sonar models return content + top-level citations array
/// (chatSearch.js:284-312).
async fn perplexity_chat_search(
    client: &Client,
    query: &str,
    max_results: u32,
    token: &str,
    request: &SearchRequest<'_>,
) -> Option<SearchResultSet> {
    const MODEL: &str = "sonar";
    let base = resolve_base_url("https://api.perplexity.ai", request).ok()?;
    let body = json!({
        "model": MODEL,
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

    Some(build_result_set(
        "perplexity",
        &text,
        &citations,
        max_results,
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
}
