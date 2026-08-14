use std::sync::Arc;
use std::time::Duration;

use futures_util::stream;
use futures_util::StreamExt;
use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

const CODEX_RESPONSES_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// SSE peek size (256 KiB) — ported from JS `CODEX_SSE_PEEK_BYTES`.
const CODEX_SSE_PEEK_BYTES: usize = 256 * 1024;

/// Transient-overload patterns that trigger a retry (JS `CODEX_SSE_RETRY_PATTERNS`).
const CODEX_SSE_RETRY_PATTERNS: &[&str] = &["server_is_overloaded", "service_unavailable_error"];

/// Account-fallback patterns → 503 with the capacity message (JS `CODEX_SSE_ACCOUNT_FALLBACK_PATTERNS`).
const CODEX_SSE_ACCOUNT_FALLBACK_PATTERNS: &[&str] =
    &["selected model is at capacity", "model_at_capacity"];

/// Patterns that indicate real user output has started → stop peeking (JS `CODEX_SSE_USER_OUTPUT_PATTERNS`).
const CODEX_SSE_USER_OUTPUT_PATTERNS: &[&str] = &[
    "event: response.output_text.delta",
    "event: response.function_call_arguments.delta",
    "\"type\":\"response.output_text.delta\"",
    "\"type\":\"response.function_call_arguments.delta\"",
];

const CODEX_MODEL_CAPACITY_MESSAGE: &str =
    "Selected model is at capacity. Please try a different model.";

#[derive(Clone)]
#[allow(dead_code)]
pub struct CodexExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

#[derive(Debug)]
pub enum CodexExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    StreamingResponseFailed(String),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for CodexExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for CodexExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for CodexExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for CodexExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for CodexExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for CodexExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for CodexExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

pub struct CodexExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

pub struct CodexExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

impl std::fmt::Debug for CodexExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

impl CodexExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, CodexExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }

    /// Parse Codex model string to extract actual OpenAI model name.
    ///
    /// Examples:
    /// - "codex/o4-mini" → "o4-mini"
    /// - "codex/o4-mini-high" → "o4-mini-high"
    /// - "codex/o3" → "o3"
    /// - "codex/o3-mini" → "o3-mini"
    /// - "o4-mini" → "o4-mini" (no prefix)
    pub fn parse_codex_model(model: &str) -> String {
        if let Some(stripped) = model.strip_prefix("codex/") {
            stripped.to_string()
        } else {
            model.to_string()
        }
    }

    /// Build the URL for Codex Responses API at chatgpt.com.
    ///
    /// When the model name ends with `_compact` or the `provider_node`
    /// carries a custom field `"_compact": true`, the `/compact` suffix
    /// is appended to reduce response size.
    fn build_url(&self, model: &str) -> String {
        let base = CODEX_RESPONSES_URL.trim_end_matches('/').to_string();
        let is_compact_model = model.ends_with("_compact");
        let is_compact_node = self
            .provider_node
            .as_ref()
            .and_then(|n| n.extra.get("_compact"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_compact_model || is_compact_node {
            format!("{}/compact", base)
        } else {
            base
        }
    }

    /// Build request headers for Codex Responses API.
    fn build_headers(
        &self,
        api_key: &str,
        stream: bool,
        connection_id: Option<&str>,
        credentials: &ProviderConnection,
    ) -> Result<HeaderMap, CodexExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(CodexExecutorError::InvalidHeader)?,
        );

        // 9router parity: session_id header for request session continuity.
        let session_id = connection_id
            .filter(|&cid| !cid.is_empty())
            .unwrap_or("default");
        headers.insert(
            "session_id",
            HeaderValue::from_str(session_id).map_err(CodexExecutorError::InvalidHeader)?,
        );

        // 9router parity: identify client type to Codex backend.
        headers.insert("originator", HeaderValue::from_static("codex_cli_rs"));

        // 9router parity: workspace binding for account scope + cache affinity.
        {
            let ws_id = credentials
                .provider_specific_data
                .get("workspaceId")
                .or_else(|| credentials.provider_specific_data.get("chatgptAccountId"))
                .and_then(|v| v.as_str())
                .or(connection_id);
            if let Some(ws) = ws_id {
                headers.insert(
                    "chatgpt-account-id",
                    HeaderValue::from_str(ws).map_err(CodexExecutorError::InvalidHeader)?,
                );
            }
        }

        if stream {
            headers.insert("Accept", HeaderValue::from_static("text/event-stream"));
        }

        Ok(headers)
    }

    /// Default instructions injected when none are present in the request body.
    /// Verbatim port of 9router `open-sse/config/codexInstructions.js`
    /// `CODEX_DEFAULT_INSTRUCTIONS` (JS backtick-escaped `` \` `` → plain backticks).
    const DEFAULT_CODEX_INSTRUCTIONS: &'static str = "You are Codex, based on GPT-5. You are running as a coding agent in the Codex CLI on a user's computer.\n\n## General\n\n- When searching for text or files, prefer using `rg` or `rg --files` respectively because `rg` is much faster than alternatives like `grep`. (If the `rg` command is not found, then use alternatives.)\n\n## Editing constraints\n\n- Default to ASCII when editing or creating files. Only introduce non-ASCII or other Unicode characters when there is a clear justification and the file already uses them.\n- Add succinct code comments that explain what is going on if code is not self-explanatory. You should not add comments like \"Assigns the value to the variable\", but a brief comment might be useful ahead of a complex code block that the user would otherwise have to spend time parsing out. Usage of these comments should be rare.\n- Try to use apply_patch for single file edits, but it is fine to explore other options to make the edit if it does not work well. Do not use apply_patch for changes that are auto-generated (i.e. generating package.json or running a lint or format command like gofmt) or when scripting is more efficient (such as search and replacing a string across a codebase).\n- You may be in a dirty git worktree.\n    * NEVER revert existing changes you did not make unless explicitly requested, since these changes were made by the user.\n    * If asked to make a commit or code edits and there are unrelated changes to your work or changes that you didn't make in those files, don't revert those changes.\n    * If the changes are in files you've touched recently, you should read carefully and understand how you can work with the changes rather than reverting them.\n    * If the changes are in unrelated files, just ignore them and don't revert them.\n- Do not amend a commit unless explicitly requested to do so.\n- While you are working, you might notice unexpected changes that you didn't make. If this happens, STOP IMMEDIATELY and ask the user how they would like to proceed.\n- **NEVER** use destructive commands like `git reset --hard` or `git checkout --` unless specifically requested or approved by the user.\n\n## Plan tool\n\nWhen using the planning tool:\n- Skip using the planning tool for straightforward tasks (roughly the easiest 25%).\n- Do not make single-step plans.\n- When you made a plan, update it after having performed one of the sub-tasks that you shared on the plan.\n\n## Codex CLI harness, sandboxing, and approvals\n\nThe Codex CLI harness supports several different configurations for sandboxing and escalation approvals that the user can choose from.\n\nFilesystem sandboxing defines which files can be read or written. The options for `sandbox_mode` are:\n- **read-only**: The sandbox only permits reading files.\n- **workspace-write**: The sandbox permits reading files, and editing files in `cwd` and `writable_roots`. Editing files in other directories requires approval.\n- **danger-full-access**: No filesystem sandboxing - all commands are permitted.\n\nNetwork sandboxing defines whether network can be accessed without approval. Options for `network_access` are:\n- **restricted**: Requires approval\n- **enabled**: No approval needed\n\nApprovals are your mechanism to get user consent to run shell commands without the sandbox. Possible configuration options for `approval_policy` are\n- **untrusted**: The harness will escalate most commands for user approval, apart from a limited allowlist of safe \"read\" commands.\n- **on-failure**: The harness will allow all commands to run in the sandbox (if enabled), and failures will be escalated to the user for approval to run again without the sandbox.\n- **on-request**: Commands will be run in the sandbox by default, and you can specify in your tool call if you want to escalate a command to run without sandboxing. (Note that this mode is not always available. If it is, you'll see parameters for it in the `shell` command description.)\n- **never**: This is a non-interactive mode where you may NEVER ask the user for approval to run commands. Instead, you must always persist and work around constraints to solve the task for the user. You MUST do your utmost best to finish the task and validate your work before yielding. If this mode is paired with `danger-full-access`, take advantage of it to deliver the best outcome for the user. Further, in this mode, your default testing philosophy is overridden: Even if you don't see local patterns for testing, you may add tests and scripts to validate your work. Just remove them before yielding.\n\nWhen you are running with `approval_policy == on-request`, and sandboxing enabled, here are scenarios where you'll need to request approval:\n- You need to run a command that writes to a directory that requires it (e.g. running tests that write to /var)\n- You need to run a GUI app (e.g., open/xdg-open/osascript) to open browsers or files.\n- You are running sandboxed and need to run a command that requires network access (e.g. installing packages)\n- If you run a command that is important to solving the user's query, but it fails because of sandboxing, rerun the command with approval. ALWAYS proceed to use the `sandbox_permissions` and `justification` parameters - do not message the user before requesting approval for the command.\n- You are about to take a potentially destructive action such as an `rm` or `git reset` that the user did not explicitly ask for\n- (for all of these, you should weigh alternative paths that do not require approval)\n\nWhen `sandbox_mode` is set to read-only, you'll need to request approval for any command that isn't a read.\n\nYou will be told what filesystem sandboxing, network sandboxing, and approval mode are active in a developer or user message. If you are not told about this, assume that you are running with workspace-write, network sandboxing enabled, and approval on-failure.\n\nAlthough they introduce friction to the user because your work is paused until the user responds, you should leverage them when necessary to accomplish important work. If the completing the task requires escalated permissions, Do not let these settings or the sandbox deter you from attempting to accomplish the user's task unless it is set to \"never\", in which case never ask for approvals.\n\nWhen requesting approval to execute a command that will require escalated privileges:\n  - Provide the `sandbox_permissions` parameter with the value `\"require_escalated\"`\n  - Include a short, 1 sentence explanation for why you need escalated permissions in the justification parameter\n\n## Special user requests\n\n- If the user makes a simple request (such as asking for the time) which you can fulfill by running a terminal command (such as `date`), you should do so.\n- If the user asks for a \"review\", default to a code review mindset: prioritise identifying bugs, risks, behavioural regressions, and missing tests. Findings must be the primary focus of the response - keep summaries or overviews brief and only after enumerating the issues. Present findings first (ordered by severity with file/line references), follow with open questions or assumptions, and offer a change-summary only as a secondary detail. If no findings are discovered, state that explicitly and mention any residual risks or testing gaps.\n\n## Frontend tasks\nWhen doing frontend design tasks, avoid collapsing into \"AI slop\" or safe, average-looking layouts.\nAim for interfaces that feel intentional, bold, and a bit surprising.\n- Typography: Use expressive, purposeful fonts and avoid default stacks (Inter, Roboto, Arial, system).\n- Color & Look: Choose a clear visual direction; define CSS variables; avoid purple-on-white defaults. No purple bias or dark mode bias.\n- Motion: Use a few meaningful animations (page-load, staggered reveals) instead of generic micro-motions.\n- Background: Don't rely on flat, single-color backgrounds; use gradients, shapes, or subtle patterns to build atmosphere.\n- Overall: Avoid boilerplate layouts and interchangeable UI patterns. Vary themes, type families, and visual languages across outputs.\n- Ensure the page loads properly on both desktop and mobile\n\nException: If working within an existing website or design system, preserve the established patterns, structure, and visual language.\n\n## Presenting your work and final message\n\nYou are producing plain text that will later be styled by the CLI. Follow these rules exactly. Formatting should make results easy to scan, but not feel mechanical. Use judgment to decide how much structure adds value.\n\n- Default: be very concise; friendly coding teammate tone.\n- Ask only when needed; suggest ideas; mirror the user's style.\n- For substantial work, summarize clearly; follow final-answer formatting.\n- Skip heavy formatting for simple confirmations.\n- Don't dump large files you've written; reference paths only.\n- No \"save/copy this file\" - User is on the same machine.\n- Offer logical next steps (tests, commits, build) briefly; add verify steps if you couldn't do something.\n- For code changes:\n  * Lead with a quick explanation of the change, and then give more details on the context covering where and why a change was made. Do not start this explanation with \"summary\", just jump right in.\n  * If there are natural next steps the user may want to take, suggest them at the end of your response. Do not make suggestions if there are no natural next steps.\n  * When suggesting multiple options, use numeric lists for the suggestions so the user can quickly respond with a single number.\n- The user does not command execution outputs. When asked to show the output of a command (e.g. `git show`), relay the important details in your answer or summarize the key lines so the user understands the result.\n\n### Final answer structure and style guidelines\n\n- Plain text; CLI handles styling. Use structure only when it helps scanability.\n- Headers: optional; short Title Case (1-3 words) wrapped in **…**; no blank line before the first bullet; add only if they truly help.\n- Bullets: use - ; merge related points; keep to one line when possible; 4–6 per list ordered by importance; keep phrasing consistent.\n- Monospace: backticks for commands/paths/env vars/code ids and inline examples; use for literal keyword bullets; never combine with **.\n- Code samples or multi-line snippets should be wrapped in fenced code blocks; include an info string as often as possible.\n- Structure: group related bullets; order sections general → specific → supporting; for subsections, start with a bolded keyword bullet, then items; match complexity to the task.\n- Tone: collaborative, concise, factual; present tense, active voice; self-contained; no \"above/below\"; parallel wording.\n- Don'ts: no nested bullets/hierarchies; no ANSI codes; don't cram unrelated keywords; keep keyword lists short—wrap/reformat if long; avoid naming formatting styles in answers.\n- Adaptation: code explanations → precise, structured with code refs; simple tasks → lead with outcome; big changes → logical walkthrough + rationale + next actions; casual one-offs → plain sentences, no headers/bullets.\n- File References: When referencing files in your response follow the below rules:\n  * Use inline code to make file paths clickable.\n  * Each reference should have a stand alone path. Even if it's the same file.\n  * Accepted: absolute, workspace-relative, a/ or b/ diff prefixes, or bare filename/suffix.\n  * Optionally include line/column (1-based): :line[:column] or #Lline[Ccolumn] (column defaults to 1).\n  * Do not use URIs like file://, vscode://, or https://.\n  * Do not provide range of lines\n  * Examples: src/app.ts, src/app.ts:42, b/server/index.js#L10, C:\\repo\\project\\main.rs:12:5";

    /// Transform the request body from Chat Completions format to Codex Responses API format.
    ///
    /// Handles both pre-translated bodies (input[] array from `chat_to_openai_responses_request`)
    /// and untranslated OpenAI bodies (messages[] array) — this avoids double-translation bugs
    /// when the pipeline already ran request translation before calling the executor.
    ///
    /// The Codex Responses API at chatgpt.com uses `input` as an array of message items.
    /// This function:
    /// - Converts messages[] to input[] with type "message", role, and content as input_text blocks
    /// - Converts "system" role to "developer"
    /// - Strips server-generated IDs (rs_, fc_, resp_, msg_ prefixes) to avoid 404s with store:false
    ///
    /// 9router codex.js parity:
    /// - Forces stream: true (Codex backend; client JSON via forceStream SSE→JSON)
    /// - Forces store: false
    /// - Strips effort suffixes from model (`-high`, `-medium`, …) into reasoning.effort
    /// - Injects instructions default when missing
    fn transform_request_body(
        &self,
        body: &Value,
        actual_model: &str,
        _stream: bool,
    ) -> Result<Value, CodexExecutorError> {
        // Handle both pre-translated (input[]) and untranslated (messages[]) bodies
        let input_items = if let Some(input) = body.get("input").and_then(Value::as_array) {
            if input.is_empty() {
                return Err(CodexExecutorError::UnsupportedFormat(
                    "Empty input array in request body".to_string(),
                ));
            }
            input.clone()
        } else {
            Self::extract_input_items(body)?
        };

        let instructions = body
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(Self::DEFAULT_CODEX_INSTRUCTIONS);

        // Strip effort suffix from model name (9router: none/minimal/low/medium/high/xhigh)
        let effort_levels = ["none", "minimal", "low", "medium", "high", "xhigh"];
        let mut model_id = actual_model.to_string();
        let mut model_effort: Option<&str> = None;
        for level in effort_levels {
            let suffix = format!("-{level}");
            if let Some(stripped) = model_id.strip_suffix(&suffix) {
                model_id = stripped.to_string();
                model_effort = Some(level);
                break;
            }
            // Also support model(high) style
            let paren = format!("({level})");
            if let Some(idx) = model_id.rfind(&paren) {
                model_id = model_id[..idx].trim_end().to_string();
                model_effort = Some(level);
                break;
            }
        }

        // Priority: body.reasoning.effort > reasoning_effort > model suffix > default low
        let effort = body
            .pointer("/reasoning/effort")
            .and_then(Value::as_str)
            .or_else(|| body.get("reasoning_effort").and_then(Value::as_str))
            .or(model_effort)
            .unwrap_or("low");

        let mut request_body = json!({
            "model": model_id,
            "input": input_items,
            "instructions": instructions,
            "stream": true, // 9router always forces stream
            "store": false,
            "reasoning": { "effort": effort, "summary": "auto" },
        });

        if let Some(tools) = body.get("tools") {
            request_body["tools"] = tools.clone();
        }
        if let Some(tool_choice) = body.get("tool_choice") {
            request_body["tool_choice"] = tool_choice.clone();
        }

        // Include reasoning encrypted content — Codex backend requires this for
        // reasoning models. JS: `if effort !== "none" → body.include =
        // ["reasoning.encrypted_content"]` (codex.js:457-459). Overwrite any
        // client-supplied include per JS.
        if effort != "none" {
            request_body["include"] = json!(["reasoning.encrypted_content"]);
        }

        // Inject prompt_cache_key for stable Codex prompt caching when the
        // caller didn't supply one (JS codex.js:426-428).
        let cache_session = body
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                body.get("input")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|item| item.get("session_id"))
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
            });
        if let Some(ck) = cache_session {
            request_body["prompt_cache_key"] = Value::String(ck);
        }

        // service_tier mapping: "fast" → "priority", delete other non-priority
        // (JS codex.js:480-481).
        if let Some(tier) = body.get("service_tier").and_then(Value::as_str) {
            if tier == "fast" {
                request_body["service_tier"] = Value::String("priority".to_string());
            } else if tier == "priority" {
                request_body["service_tier"] = Value::String("priority".to_string());
            }
            // else: dropped (not carried into request_body)
        }

        Ok(request_body)
    }

    /// Extract input items array from a Chat Completions style request body.
    ///
    /// Returns a Vec of Responses API input items, where each message becomes:
    /// ```json
    /// {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hello"}]}
    /// ```
    ///
    /// - "system" role is converted to "developer"
    /// - Server-generated IDs (prefixes rs_, fc_, resp_, msg_) are stripped
    /// - String content is wrapped in an input_text array
    /// - Content arrays have text parts converted to input_text type
    fn extract_input_items(body: &Value) -> Result<Vec<Value>, CodexExecutorError> {
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                CodexExecutorError::UnsupportedFormat("Missing messages array".to_string())
            })?;

        if messages.is_empty() {
            return Err(CodexExecutorError::UnsupportedFormat(
                "No messages found in request body".to_string(),
            ));
        }

        let mut items: Vec<Value> = Vec::new();

        for msg in messages {
            let mut role = msg
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string();

            // Convert "system" role to "developer" (Responses API convention)
            if role == "system" {
                role = "developer".to_string();
            }

            // Extract and transform content into Responses API format
            let content_arr: Value = match msg.get("content") {
                Some(Value::String(s)) => {
                    if s.is_empty() {
                        continue;
                    }
                    json!([{"type": "input_text", "text": s}])
                }
                Some(Value::Array(arr)) => {
                    if arr.is_empty() {
                        continue;
                    }
                    let mut parts: Vec<Value> = Vec::new();
                    for part in arr {
                        let part_type = part.get("type").and_then(Value::as_str).unwrap_or("text");
                        // Convert "text" type to "input_text" for Responses API
                        if part_type == "text" {
                            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                parts.push(json!({"type": "input_text", "text": text}));
                            }
                        } else {
                            // Pass through other content types (image_url, etc.)
                            parts.push(part.clone());
                        }
                    }
                    json!(parts)
                }
                _ => continue,
            };

            // Build the input item
            let mut item = json!({
                "type": "message",
                "role": role,
                "content": content_arr,
            });

            // Strip server-generated IDs (prefixes rs_, fc_, resp_, msg_)
            // Keep user-provided IDs that don't match these patterns
            if let Some(id) = msg.get("id").and_then(Value::as_str) {
                let is_server_id = id.starts_with("rs_")
                    || id.starts_with("fc_")
                    || id.starts_with("resp_")
                    || id.starts_with("msg_");
                if !is_server_id {
                    item["id"] = json!(id);
                }
            }

            // Preserve "name" field if present
            if let Some(name) = msg.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    item["name"] = json!(name);
                }
            }

            items.push(item);
        }

        if items.is_empty() {
            return Err(CodexExecutorError::UnsupportedFormat(
                "No valid content found in messages".to_string(),
            ));
        }

        Ok(items)
    }

    /// Prefetch remote `image_url` content parts into `input_image` parts with
    /// inline base64 data URIs. Mirrors JS `prefetchImages` (codex.js:241-256):
    /// `data:` URLs pass through directly; remote URLs are fetched (15s timeout).
    async fn prefetch_images(&self, body: &mut Value) {
        let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) else {
            return;
        };
        for item in input.iter_mut() {
            let Some(content) = item.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for part in content.iter_mut() {
                let Some(obj) = part.as_object_mut() else {
                    continue;
                };
                if obj.get("type").and_then(Value::as_str) != Some("image_url") {
                    continue;
                }
                let url = obj
                    .get("image_url")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Object(o) => o.get("url").and_then(Value::as_str).map(String::from),
                        _ => None,
                    })
                    .unwrap_or_default();
                if url.is_empty() {
                    continue;
                }
                let detail = obj
                    .get("image_url")
                    .and_then(|v| v.get("detail"))
                    .and_then(Value::as_str)
                    .unwrap_or("auto")
                    .to_string();
                let image_url = if url.starts_with("data:") {
                    url
                } else {
                    // Remote URL: fetch and inline as base64 data URI.
                    let client = reqwest::Client::new();
                    match crate::core::translator::helpers::image_helper::fetch_image_as_base64(
                        &client, &url,
                    )
                    .await
                    {
                        Some(fetched) => fetched.data_url,
                        None => url,
                    }
                };
                let _ = obj.insert("type".into(), Value::String("input_image".to_string()));
                let _ = obj.insert("image_url".into(), Value::String(image_url));
                let _ = obj.insert("detail".into(), Value::String(detail));
            }
        }
    }

    /// Parse a Codex upstream error, mapping `usage_limit_reached` to a
    /// resetsAtMs (JS `parseError`, codex.js:365-387).
    pub fn parse_error(status: u16, body_text: &str) -> crate::core::utils::error::UpstreamError {
        if status == 429 && !body_text.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(body_text) {
                let err = v.get("error");
                if err.and_then(|e| e.get("type")).and_then(Value::as_str)
                    == Some("usage_limit_reached")
                {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let mut resets_at_ms = None;
                    if let Some(secs) = err.and_then(|e| e.get("resets_at")).and_then(Value::as_u64)
                    {
                        let ms = secs * 1000;
                        if ms > now_ms {
                            resets_at_ms = Some(ms);
                        }
                    }
                    if resets_at_ms.is_none() {
                        if let Some(secs) = err
                            .and_then(|e| e.get("resets_in_seconds"))
                            .and_then(Value::as_u64)
                        {
                            resets_at_ms = Some(now_ms + secs * 1000);
                        }
                    }
                    let message = err
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or(body_text)
                        .to_string();
                    return crate::core::utils::error::UpstreamError {
                        status: 429,
                        message,
                        resets_at_ms,
                    };
                }
            }
        }
        crate::core::utils::error::UpstreamError {
            status,
            message: crate::core::utils::error::friendly_error_message(status, body_text),
            resets_at_ms: None,
        }
    }

    pub async fn execute(
        &self,
        mut request: CodexExecutionRequest,
    ) -> Result<CodexExecutorResponse, CodexExecutorError> {
        let actual_model = Self::parse_codex_model(&request.model);
        let url = self.build_url(&actual_model);

        // Get API key from credentials (try api_key first, then access_token for OAuth)
        let api_key = request
            .credentials
            .api_key
            .as_deref()
            .or(request.credentials.access_token.as_deref())
            .ok_or_else(|| {
                CodexExecutorError::MissingCredentials("API key required".to_string())
            })?;

        let connection_id = request
            .credentials
            .email
            .as_deref()
            .or(request.credentials.id.as_str().into())
            .or(request.credentials.display_name.as_deref());
        // Always stream upstream (9router force stream); client JSON via chat sse_to_json
        let headers = self.build_headers(api_key, true, connection_id, &request.credentials)?;

        // Prefetch remote images into inline base64 data URIs (JS prefetchImages).
        if let Some(input) = request.body.get("input") {
            if input
                .as_array()
                .is_some_and(|arr| arr.iter().any(|it| it.get("content").is_some()))
            {
                self.prefetch_images(&mut request.body).await;
            }
        }

        let transformed_body = self.transform_request_body(&request.body, &actual_model, true)?;

        let client = self.pool.get("openai", request.proxy.as_ref())?;

        // SSE-level transient-error retry (JS execute/_peekSseTransientError,
        // codex.js:258-362). Retries on server_is_overloaded /
        // service_unavailable_error; account-fallback → 503 capacity message.
        const MAX_RETRIES: usize = 3;
        for attempt in 0..MAX_RETRIES {
            let resp = client
                .post(&url)
                .headers(headers.clone())
                .json(&transformed_body)
                .send()
                .await?;

            // Capture parts before consuming the body.
            let status = resp.status();
            let resp_headers = resp.headers().clone();

            // Stream-peek the first ≤256 KiB of the SSE body.
            let stream = resp.bytes_stream();
            let mut chunks: Vec<bytes::Bytes> = Vec::new();
            let mut text = String::new();
            let mut matched: Option<&str> = None;
            let mut account_fallback = false;
            let mut pinned = stream;
            let mut total = 0usize;
            while total < CODEX_SSE_PEEK_BYTES {
                match pinned.next().await {
                    Some(Ok(chunk)) => {
                        total += chunk.len();
                        chunks.push(chunk.clone());
                        text.push_str(&String::from_utf8_lossy(&chunk));
                        let lower = text.to_lowercase();
                        let account_hit = CODEX_SSE_ACCOUNT_FALLBACK_PATTERNS
                            .iter()
                            .find(|p| lower.contains(**p));
                        if let Some(hit) = account_hit {
                            matched = Some(hit);
                            account_fallback = true;
                            break;
                        }
                        let retry_hit = CODEX_SSE_RETRY_PATTERNS
                            .iter()
                            .find(|p| lower.contains(**p));
                        if let Some(hit) = retry_hit {
                            matched = Some(hit);
                            break;
                        }
                        if CODEX_SSE_USER_OUTPUT_PATTERNS
                            .iter()
                            .any(|p| lower.contains(*p))
                        {
                            break;
                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }

            if let Some(_hit) = matched {
                if account_fallback {
                    // Return 503 with the exact capacity message for downstream
                    // account fallback matching.
                    let err_body = json!({
                        "error": {
                            "message": CODEX_MODEL_CAPACITY_MESSAGE,
                            "type": "server_error",
                            "code": "service_unavailable",
                        }
                    });
                    let bytes = serde_json::to_vec(&err_body).unwrap_or_default();
                    let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
                    *http_resp.status_mut() = reqwest::StatusCode::SERVICE_UNAVAILABLE;
                    http_resp.headers_mut().insert(
                        reqwest::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                    return Ok(CodexExecutorResponse {
                        response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                        url,
                        headers,
                        transformed_body,
                        transport: TransportKind::Reqwest,
                    });
                }
                if attempt + 1 < MAX_RETRIES {
                    let delay = Duration::from_millis(500 * 2u64.pow(attempt as u32));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                let err_body = json!({
                    "error": {
                        "message": matched.unwrap_or("server_is_overloaded"),
                        "type": "server_error",
                        "code": "service_unavailable",
                    }
                });
                let bytes = serde_json::to_vec(&err_body).unwrap_or_default();
                let mut http_resp = http::Response::new(ReqwestBody::from(bytes));
                *http_resp.status_mut() = reqwest::StatusCode::SERVICE_UNAVAILABLE;
                http_resp.headers_mut().insert(
                    reqwest::header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                return Ok(CodexExecutorResponse {
                    response: UpstreamResponse::Reqwest(reqwest::Response::from(http_resp)),
                    url,
                    headers,
                    transformed_body,
                    transport: TransportKind::Reqwest,
                });
            }

            // No transient error matched → re-assemble a live stream from the
            // peeked prefix chunks + the remaining upstream body so SSE flows.
            // Both sides must yield `Result<Bytes, reqwest::Error>`.
            let prefix = stream::iter(chunks).map(Ok::<_, reqwest::Error>);
            let combined = prefix.chain(pinned);
            let mut http_resp = http::Response::new(ReqwestBody::wrap_stream(combined));
            *http_resp.status_mut() = status;
            *http_resp.headers_mut() = resp_headers;
            let reconstructed = reqwest::Response::from(http_resp);

            return Ok(CodexExecutorResponse {
                response: UpstreamResponse::Reqwest(reconstructed),
                url,
                headers,
                transformed_body,
                transport: TransportKind::Reqwest,
            });
        }

        Err(CodexExecutorError::StreamingResponseFailed(
            "max retries exhausted for overloaded SSE response".into(),
        ))
    }
}

/// Convert OpenAI Responses API SSE format to standard SSE format.
///
/// OpenAI Responses API returns events like:
/// - `event: response.done\ndata: {...}\n\n`
/// - `event: content.delta\ndata: {"type": "content.delta", "delta": {"type": "text_delta", "text": "Hello"}}\n\n`
///
/// We need to convert to standard format:
/// - `data: {"type": "content.delta", ...}\n\n`
pub fn convert_openai_sse_to_standard(input: &[u8]) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }

    let input_str = String::from_utf8_lossy(input);
    let mut output = Vec::new();

    for line in input_str.lines() {
        // Skip the event: line, keep only data: lines
        if line.starts_with("data: ") {
            // Extract data content
            let data_content = line.trim_start_matches("data: ");
            // Output in standard SSE format
            output.extend_from_slice(b"data: ");
            output.extend_from_slice(data_content.as_bytes());
            output.extend_from_slice(b"\n\n");
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_model_with_prefix() {
        assert_eq!(CodexExecutor::parse_codex_model("codex/o4-mini"), "o4-mini");
        assert_eq!(
            CodexExecutor::parse_codex_model("codex/o4-mini-high"),
            "o4-mini-high"
        );
        assert_eq!(CodexExecutor::parse_codex_model("codex/o3"), "o3");
        assert_eq!(CodexExecutor::parse_codex_model("codex/o3-mini"), "o3-mini");
    }

    #[test]
    fn test_parse_codex_model_without_prefix() {
        assert_eq!(CodexExecutor::parse_codex_model("o4-mini"), "o4-mini");
        assert_eq!(CodexExecutor::parse_codex_model("gpt-4"), "gpt-4");
    }

    #[test]
    fn test_codex_request_body_format() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "user", "content": "Hello, world!"}
            ],
            "stream": false,
            "temperature": 0.7
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini-high", false)
            .unwrap();

        assert_eq!(result["model"], "o4-mini"); // suffix stripped
        assert_eq!(result["stream"], true); // forced
        assert_eq!(result["store"], false);
        assert_eq!(result["reasoning"]["effort"], "high");
        assert!(
            result.get("temperature").is_none(),
            "temperature should be stripped by allowlist"
        );

        // input should be an array of Response API items
        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["role"], "user");
        let content = input[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello, world!");

        // instructions should be injected
        assert_eq!(
            result["instructions"],
            CodexExecutor::DEFAULT_CODEX_INSTRUCTIONS
        );
    }

    #[test]
    fn test_codex_request_body_multiple_messages() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ]
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini", true)
            .unwrap();

        let input = result["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "Hello");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["text"], "Hi there!");
        assert_eq!(input[2]["role"], "user");
        assert_eq!(input[2]["content"][0]["text"], "How are you?");
    }

    #[test]
    fn test_codex_request_body_converts_system_to_developer() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        let chat_body = json!({
            "model": "codex/o4-mini",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ]
        });

        let result = executor
            .transform_request_body(&chat_body, "o4-mini", true)
            .unwrap();

        let input = result["input"].as_array().unwrap();
        // "system" should now be "developer"
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(
            input[0]["content"][0]["text"],
            "You are a helpful assistant."
        );
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[1]["content"][0]["text"], "Hello!");
        assert_eq!(input.len(), 2);
    }

    #[test]
    fn test_codex_sse_conversion() {
        let openai_sse = b"event: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\nevent: content.delta\ndata: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}\n\nevent: response.done\ndata: {\"type\":\"response.done\"}\n";

        let result = convert_openai_sse_to_standard(openai_sse);
        let result_str = String::from_utf8(result).unwrap();

        assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}"));
        assert!(result_str.contains("data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\" World\"}}"));
        assert!(result_str.contains("data: {\"type\":\"response.done\"}"));
    }

    #[test]
    fn test_codex_sse_conversion_empty() {
        let result = convert_openai_sse_to_standard(b"");
        assert!(result.is_empty());
    }

    #[test]
    fn test_codex_sse_conversion_standard_format_unchanged() {
        let standard_sse = b"data: {\"type\":\"content.delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n";
        let result = convert_openai_sse_to_standard(standard_sse);
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.contains("data: {\"type\":\"content.delta\""));
    }

    #[test]
    fn test_extract_input_items_missing_messages() {
        let body = json!({
            "model": "codex/o4-mini"
        });

        let result = CodexExecutor::extract_input_items(&body);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_input_items_empty_messages() {
        let body = json!({
            "model": "codex/o4-mini",
            "messages": []
        });

        let result = CodexExecutor::extract_input_items(&body);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_input_items_server_ids_stripped() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Hello", "id": "msg_abc123"},
                {"role": "user", "content": "World", "id": "my-custom-id"}
            ]
        });

        let items = CodexExecutor::extract_input_items(&body).unwrap();
        assert_eq!(items.len(), 2);
        // First item had "msg_" prefix -> stripped, no id field expected
        assert!(
            items[0].get("id").is_none(),
            "server-generated msg_ id should be stripped"
        );
        // Second item had custom ID -> preserved
        assert_eq!(items[1]["id"], "my-custom-id");
    }

    #[test]
    fn test_extract_input_items_content_array_with_text_type() {
        let body = json!({
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello "},
                        {"type": "text", "text": "world"}
                    ]
                }
            ]
        });

        let items = CodexExecutor::extract_input_items(&body).unwrap();
        assert_eq!(items.len(), 1);
        let content = items[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello ");
        assert_eq!(content[1]["type"], "input_text");
        assert_eq!(content[1]["text"], "world");
    }

    #[test]
    fn test_build_url_base() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let url = executor.build_url("o4-mini");
        assert_eq!(url, "https://chatgpt.com/backend-api/codex/responses");
    }

    #[test]
    fn test_build_url_compact_suffix() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();
        let url = executor.build_url("o4-mini_compact");
        assert_eq!(
            url,
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
    }

    #[test]
    fn test_codex_instructions_matches_js() {
        // Guard test: DEFAULT_CODEX_INSTRUCTIONS must be the full JS prompt,
        // not the old one-liner.
        assert!(
            CodexExecutor::DEFAULT_CODEX_INSTRUCTIONS
                .starts_with("You are Codex, based on GPT-5. You are running as a coding agent"),
            "instructions must match the JS default (codexInstructions.js)"
        );
        assert!(
            CodexExecutor::DEFAULT_CODEX_INSTRUCTIONS.contains("## Editing constraints"),
            "instructions should include the full multi-section prompt"
        );
        assert!(
            CodexExecutor::DEFAULT_CODEX_INSTRUCTIONS.contains("## Plan tool"),
            "instructions should include the Plan tool section"
        );
    }

    #[test]
    fn test_codex_include_reasoning_when_effort() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        // effort high → include == ["reasoning.encrypted_content"]
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "effort": "high" },
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false)
            .unwrap();
        assert_eq!(out["include"], json!(["reasoning.encrypted_content"]));

        // effort none → NO include
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "reasoning": { "effort": "none" },
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false)
            .unwrap();
        assert!(
            out.get("include").is_none(),
            "no include when effort is none"
        );
    }

    #[test]
    fn test_codex_service_tier_mapping() {
        let executor = CodexExecutor::new(Arc::new(ClientPool::new()), None).unwrap();

        // fast → priority
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "service_tier": "fast",
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false)
            .unwrap();
        assert_eq!(out["service_tier"], "priority");

        // non-priority, non-fast → dropped
        let body = json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "service_tier": "flexible",
            "model": "codex/o4-mini"
        });
        let out = executor
            .transform_request_body(&body, "o4-mini", false)
            .unwrap();
        assert!(out.get("service_tier").is_none());
    }

    #[test]
    fn test_codex_parse_error_usage_limit_reached() {
        // resets_at is in seconds since epoch; use a far-future value.
        let body = json!({
            "error": { "type": "usage_limit_reached", "message": "Limit hit", "resets_at": 4_100_000_000u64 }
        });
        let err = CodexExecutor::parse_error(429, &body.to_string());
        assert_eq!(err.status, 429);
        assert!(
            err.resets_at_ms.is_some(),
            "resets_at_ms should be populated"
        );
        assert_eq!(err.message, "Limit hit");
    }
}
