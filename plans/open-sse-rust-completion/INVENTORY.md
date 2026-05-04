# Provider and Format Inventory — br-30o

> Audit date: 2026-05-04
> Bead: br-30o (Spike: Provider and format inventory audit)
> Based on: open-sse/executors/index.js, open-sse/translator/index.js, src/core/executor/default.rs, src/core/executor/provider.rs

---

## Part 1: Executor Status

### JS Specialized Executors (from `open-sse/executors/index.js`)

| Provider | JS Class | Rust Status | Rust File | Notes |
|----------|----------|-------------|-----------|-------|
| antigravity | AntigravityExecutor | **MISSING** | — | Custom auth (client_credentials), project-id, session-based requests. Needs bespoke executor. Spike `br-a7r` before implementation. |
| azure | AzureExecutor | **Partial** | default.rs only | Rust has azure entry in PROVIDER_CONFIGS (openai format) but no bespoke executor. JS version overrides `buildUrl` to use azureEndpoint+deployment, overrides `buildHeaders` to use `api-key`. Simple enough to add to default.rs with credential mapping. |
| gemini-cli | GeminiCLIExecutor | **MISSING** | — | Custom auth (OAuth client credentials), `cloudcode-pa.googleapis.com` base URL. Needs bespoke executor. Spike `br-a7r` before implementation. |
| github | GithubExecutor | **MISSING** | — | Uses copilotToken, special headers (copilot-integration-id, editor-version, etc.), `/chat/completions` format with OpenAI-like body. Auth is OAuth token exchange. |
| iflow | IFlowExecutor | **MISSING** | — | Cookie-based auth, custom URL (`apis.iflow.cn/v1/chat/completions`). Simple enough to add to default.rs. |
| qoder | QoderExecutor | **MISSING** | — | OAuth flow (similar to iflow), custom URL (`api.qoder.com`). Simple enough to add to default.rs. |
| kiro | KiroExecutor | **HAS** | kiro.rs | Rust implementation already exists with AWS event-stream decoding. |
| codex | CodexExecutor | **HAS** | codex.rs | Rust implementation already exists. Uses /responses endpoint, transforms messages→input, handles streaming. |
| cursor | CursorExecutor | **HAS** | cursor.rs | Rust implementation already exists. Binary protobuf frames, zlib decompression. Complex. |
| vertex | VertexExecutor | **HAS** | vertex.rs | Rust implementation already exists. |
| qwen | QwenExecutor | **MISSING** | — | OAuth flow with 20-min refresh lead time, custom URL (`portal.qwen.ai`). Should be added to default.rs with auth mapping. Spike `br-a7r` not needed for qwen (simpler than antigravity/gemini-cli). |
| opencode | OpenCodeExecutor | **MISSING** | — | Very simple: base `https://opencode.ai`, Bearer "public", path varies by model (chat/completions vs /messages for big-pickle). Can add to default.rs. |
| opencode-go | OpenCodeGoExecutor | **HAS** | default.rs | Rust has opencode-go entry in PROVIDER_CONFIGS, custom model routing (claude vs openai format per model). |
| grok-web | GrokWebExecutor | **HAS** | grok_web.rs | Rust implementation already exists. Perplexity-style web scraping with session management. |
| perplexity-web | PerplexityWebExecutor | **HAS** | grok_web.rs | Rust implementation already exists. |
| default | DefaultExecutor | **HAS** | default.rs, provider.rs | Catch-all. All other providers use this. |

### Summary: Executor Actions

| Category | Count | Providers |
|----------|-------|-----------|
| Already in Rust (specialized) | 6 | kiro, codex, cursor, vertex, grok-web, perplexity-web |
| In Rust default.rs PROVIDER_CONFIGS | 15+ | openai, anthropic, gemini, glm, kimi, minimax, deepseek, groq, xai, mistral, together, fireworks, cerebras, cohere, nvidia, nebius, siliconflow, hyperbolic, perplexity, nanobanana, chutes, gitlab, codebuddy, kilocode, cline, opencode-go, glm-cn, alicode, alicode-intl, volcengine-ark, byteplus, cloudflare-ai, azure, blackbox, ollama-cloud, vertex-partner, ollama-local |
| Need bespoke executor (complex) | 3 | antigravity, gemini-cli, github |
| Need default.rs entry (simple) | 4 | azure (upgrade), iflow, qoder, qwen, opencode |

### Decision: Bespoke vs Default

The JS codebase only creates bespoke executors when the provider has **custom auth flow, session management, or non-standard request body formatting**. Everything else falls through to `DefaultExecutor(getExecutor(provider))`.

Rust should follow the same rule. The providers flagged as "MISSING but simple" should be added as default.rs entries, not as new executor files.

---

## Part 2: Translator Pair Status

### JS Request Translators (from `open-sse/translator/index.js` lazy-load list)

| Direction | JS File | Rust Status | Notes |
|-----------|---------|-------------|-------|
| openai → claude | openai-to-claude.js | **MISSING** | response_format→json_schema injection, system→messages[0], tool_calls mapping, thinking config |
| claude → openai | claude-to-openai.js | **MISSING** | system→messages, content array flattening (text-only→string), multimodal preservation, output_config stripping for compatible endpoints |
| openai → gemini | openai-to-gemini.js | **MISSING** | messages→contents, system→systemInstruction, tool_calls→functionDeclaration parts |
| gemini → openai | gemini-to-openai.js | **MISSING** | contents→messages, functionCall→tool_calls |
| openai → vertex | openai-to-vertex.js | **MISSING** | Similar to gemini, vertex-specific auth headers |
| openai → kiro | openai-to-kiro.js | **MISSING** | AWS event-stream body preparation |
| openai → ollama | openai-to-ollama.js | **MISSING** | Ollama NDJSON format, model field handling |
| openai → cursor | openai-to-cursor.js | **MISSING** | Cursor-specific format mapping |
| antigravity → openai | antigravity-to-openai.js | **MISSING** | Antigravity body normalization |
| openai → responses | openai-responses.js | **PARTIAL** | Some transforms in codex.rs and chat.rs handling |
| claude → openai | claude-to-openai.js | see above | same row |

### JS Response Translators (from `open-sse/translator/index.js` lazy-load list)

| Direction | JS File | Rust Status | Notes |
|-----------|---------|-------------|-------|
| claude → openai | claude-to-openai.js | **PARTIAL** | AnthropicToOpenAiTransformer in response_transform.rs with message_start, content_block_start, content_block_delta, message_delta, thinking_delta, cache_control_delta |
| openai → claude | openai-to-claude.js | **MISSING** | Reverse mapping (rarely used) |
| gemini → openai | gemini-to-openai.js | **MISSING** | Gemini SSE→OpenAI chunk mapping |
| kiro → openai | kiro-to-openai.js | **MISSING** | Kiro event-stream→OpenAI SSE |
| cursor → openai | cursor-to-openai.js | **MISSING** | Cursor protobuf→OpenAI SSE |
| ollama → openai | ollama-to-openai.js | **MISSING** | Ollama NDJSON→OpenAI SSE |
| openai-responses | openai-responses.js | **PARTIAL** | codex.rs handles Responses API, streamToJsonConverter exists |
| openai → antigravity | openai-to-antigravity.js | **MISSING** | Reverse mapping (rarely used) |

### Note on Format Detection

In the JS codebase, format detection is centralized via `detectFormat()` (open-sse/services/provider.js → called by chatCore.js). The Rust code currently does format detection inline in route handlers. The inventory confirms that the registry-based approach (Phase 2 bead `br-1mj`) must handle format detection as a first-class concern since every request path needs it.

---

## Part 3: Key Observations

1. **No request translator registry exists in Rust.** `src/core/translator/mod.rs` only exports `TranslationFormat` + `response_transform`. This is the primary gap and the reason Phase 2 (`br-1mj`) must create the registry first.

2. **Azure/iflow/qoder/qwen/opencode** are all simple enough to add as default.rs entries. They don't need new executor files — just provider config entries plus credential mapping overrides.

3. **Antigravity and Gemini-CLI** are the highest-complexity missing executors. Both involve multi-step OAuth (client_credentials → project-id → session creation). The lifecycle spike `br-a7r` must complete before these beads start.

4. **github** is a specialized executor because of the `copilot-integration-id`, `editor-version`, `editor-plugin-version`, and `x-github-api-version` headers that must be set correctly. The auth flow uses `copilotToken` from the connection. It needs a bespoke executor file.

5. **Response transformers** are more complete in Rust than request translators. `response_transform.rs` already has `AnthropicToOpenAiTransformer` with proper SSE event mapping. The missing pieces are Gemini, Kiro, Cursor, Ollama, and Responses API response transformers.

6. **Translator direction count**: 10 request pairs + 9 response pairs = 19 translator slots total. Only 1 (claude→openai response) is partially covered.

---

## Inventory Output

This document serves as the authoritative reference for downstream implementation beads. Every provider and translator pair appears here with a status.

**Commit: DONE** — `plans/open-sse-rust-completion/INVENTORY.md`
