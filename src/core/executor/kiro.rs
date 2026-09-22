use std::sync::Arc;

use hyper::http;
use hyper::http::uri::InvalidUri;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Maximum total AWS EventStream message length (1 MiB).
const MAX_EVENTSTREAM_MESSAGE_LENGTH: usize = 1024 * 1024;

/// Maximum bytes buffered for a repair attempt (JS `KIRO_REPAIR_BUFFER_MAX_BYTES`).
const KIRO_REPAIR_BUFFER_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Heartbeat cadence for the integrity gate (JS `KIRO_REPAIR_HEARTBEAT_MS`).
pub const KIRO_REPAIR_HEARTBEAT_MS: u64 = 10_000;

use crate::core::proxy::ProxyTarget;
use crate::types::{ProviderConnection, ProviderNode};

use super::{ClientPool, TransportKind, UpstreamResponse};

pub struct KiroExecutionRequest {
    pub model: String,
    pub body: Value,
    pub stream: bool,
    pub credentials: ProviderConnection,
    pub proxy: Option<ProxyTarget>,
}

/// 9router registry baseUrls (generateAssistantResponse surfaces).
const KIRO_BASE_URLS: &[&str] = &[
    "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
    "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
    "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
];

/// 9router `KIRO_CODEWHISPERER_TARGET` (config/kiroConstants.js).
const KIRO_CODEWHISPERER_TARGET: &str =
    "AmazonCodeWhispererStreamingService.GenerateAssistantResponse";
const KIRO_REGION: &str = "us-east-1";
const KIRO_SERVICE: &str = "codewhisperer";

/// Rewrite the AWS region segment of an amazonaws.com host, e.g.
/// `q.us-east-1.amazonaws.com` → `q.{region}.amazonaws.com`.
/// 9router getOrderedBaseUrls parity: `([a-z]+)\.[a-z0-9-]+\.amazonaws\.com`
/// → `$1.{region}.amazonaws.com`.
fn regionalize_host(host_url: &str, region: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"([a-z]+)\.[a-z0-9-]+\.amazonaws\.com").expect("static regex")
    });
    re.replace(host_url, format!("$1.{region}.amazonaws.com"))
        .into_owned()
}

fn normalize_kiro_model(model: &str) -> String {
    if let Some(stripped) = model.strip_suffix("-thinking-agentic") {
        return stripped.to_string();
    }
    if let Some(stripped) = model.strip_suffix("-thinking") {
        return stripped.to_string();
    }
    if let Some(stripped) = model.strip_suffix("-agentic") {
        return stripped.to_string();
    }
    model.to_string()
}

// ==================== INTEGRITY REPAIR LOOP (9router runIntegrityRecovery) ====================
//
// When the first attempt ends with a retryable disposition — an ellipsis-only
// answer, a "short future action" final, or an invalid tool_call wrapper — the
// JS executor retries ONCE with a repair instruction appended to the current
// user turn (kiro.js runIntegrityRecovery, 411-479). The repair is gated by the
// per-account `kiroToolCallRepair` flag (default on). A second non-complete
// attempt surfaces as an SSE error with the `kiro_*` code.

/// Max chars for the "short future action" heuristic (9router
/// KIRO_SHORT_FINAL_MAX_CHARS).
const KIRO_SHORT_FINAL_MAX_CHARS: usize = 800;

/// The classification of a completed (non-repaired) first attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KiroRepairKind {
    /// Answer is exactly "..." or "…".
    Ellipsis,
    /// Final only announced a future action.
    ShortFinal,
    /// A tool_call wrapper was malformed (missing name / arguments).
    InvalidTool,
    /// No repair needed.
    None,
}

/// True when the content is only an ellipsis (9router isEllipsisOnly).
pub fn is_ellipsis_only(content: &str) -> bool {
    matches!(content.trim(), "..." | "…")
}

/// True when the content reads like a future-action announcement rather than
/// a completed answer (9router isShortFutureAction). The English/Chinese
/// regexes mirror kiro.js lines 46-56.
pub fn is_short_future_action(content: &str) -> bool {
    let text = content.trim().replace('’', "'");
    if text.is_empty() {
        return false;
    }
    // Observed whole-response signature (kiro.js OBSERVED_TRAILING_FUTURE_ACTION).
    if text.len() > 20
        && text.starts_with("目前證據顯示")
        && text.contains("最後補查 504 access log")
    {
        return true;
    }
    // English future action with a result clause → already completed.
    if english_future_action().is_match(&text) && english_result_clause().is_match(&text) {
        return false;
    }
    // Chinese future action with a result clause → already completed.
    if chinese_future_action().is_match(&text) && chinese_result_clause().is_match(&text) {
        return false;
    }
    text.len() <= KIRO_SHORT_FINAL_MAX_CHARS
        && short_future_action().is_match(&text)
        && !user_wait().is_match(&text)
        && !completed_final().is_match(&text)
        && !result_evidence().is_match(&text)
}

// English / Chinese future-action detection (kiro.js SHORT_FUTURE_ACTION +
// companions). Each regex is compiled once and cached process-wide.
macro_rules! kiro_re {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static regex::Regex {
            static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            RE.get_or_init(|| regex::Regex::new($pattern).expect("static kiro regex"))
        }
    };
}

kiro_re!(
    short_future_action,
    r"(?i)^(?:(?:(?:現在|接著|接下來|下一步)[，,:：\s]*(?:我(?:只)?(?:會|要|將|再)?\s*)?|我只再)(?:補|查|確認|驗證|追(?:查|蹤)?|繼續|檢查|測試)|我(?:會|要|將)(?:再|重新)?(?:補(?:齊|查)?|抓取|查(?:詢)?|確認|驗證|追(?:查|蹤)?|繼續|檢查|測試)|(?:(?:next|now|then)\b[\s,:-]*)?(?:i(?:'ll| will| am going to| need to)|let me)\s+(?:verify|check|confirm|validate|investigate|trace|continue|follow up|test)\b)"
);
kiro_re!(
    english_future_action,
    r"(?i)^(?:(?:next|now|then)\b[\s,:-]*)?(?:i(?:'ll| will| am going to| need to)|let me)\s+(?:verify|check|confirm|validate|investigate|trace|continue|follow up|test)\b"
);
kiro_re!(
    english_result_clause,
    r"(?i)(?:[:;\n]|[.!?]\s+\S|\b(?:status|checksum|response|deployment)\s+(?:is|are|was|were|matches?|equals?|returned)\b)"
);
kiro_re!(
    chinese_future_action,
    r"^(?:(?:現在|接著|接下來|下一步)[，,:：\s]*(?:我(?:只)?(?:會|要|將|再)?\s*)?|我只再|我(?:會|要|將)(?:再|重新)?)(?:補|抓取|查|確認|驗證|追|繼續|檢查|測試)"
);
kiro_re!(
    chinese_result_clause,
    r"(?:[。！？]\s*\S|(?:版本|狀態|回應|結果|部署|校驗碼)(?:是|為|等於|顯示))"
);
kiro_re!(
    user_wait,
    r"(?i)(?:請(?:你|先)|你(?:先|需要|可以|提供|確認|批准|允許)|等待(?:你|使用者)|等你|核准|同意|授權|\b(?:after|when|once)\s+you\b|\byour\s+(?:approval|confirmation|permission|input)\b|\bwait(?:ing)?\s+for\s+you\b|\bplease\s+(?:approve|confirm|provide|send)\b)"
);
kiro_re!(
    completed_final,
    r"(?i)(?:已(?:經)?完成|完成(?:了|驗證|確認)|修復完成|確認無誤|驗證(?:完成|通過)|測試(?:均)?通過|結論|總結|\b(?:done|completed|fixed|verified|confirmed|passed|in conclusion|summary)\b|\b(?:is|are) complete\b)"
);
kiro_re!(
    result_evidence,
    r"(?i)(?:顯示|發現|因此|成功|失敗|正常|無錯誤|沒有錯誤|\b(?:found|shows?|showed|because|therefore|succeeded|failed|healthy|green|no errors?)\b)"
);

/// The repair instruction appended to the current user turn for a given kind
/// (9router REPAIR_INSTRUCTIONS, kiro.js 41-45).
pub fn repair_instruction(kind: KiroRepairKind) -> &'static str {
    match kind {
        KiroRepairKind::Ellipsis => "Retry the previous response because it ended with only an ellipsis. Return the complete final answer, not only ... or ….",
        KiroRepairKind::ShortFinal => "Retry the previous response because its final only announced a future action. Complete the check now and return the result or a concrete blocker.",
        KiroRepairKind::InvalidTool => "Retry the previous response because its Kiro tool_call wrapper was malformed. If you use the wrapper tool named tool_call, its input must contain a non-empty name and an arguments field.",
        KiroRepairKind::None => "Retry the previous incomplete Kiro response.",
    }
}

/// Append the repair instruction to the current user turn, never to a
/// top-level `systemPrompt` (9router appendRepairInstruction, kiro.js
/// 130-143: kiro.dev answers any body carrying that field with 400
/// REQUEST_BODY_INVALID). Returns a cloned body.
pub fn append_repair_instruction(body: &Value, kind: KiroRepairKind) -> Value {
    let mut repaired = body.clone();
    let instruction = repair_instruction(kind);
    if let Some(msg) = repaired
        .get_mut("conversationState")
        .and_then(|s| s.get_mut("currentMessage"))
        .and_then(|m| m.get_mut("userInputMessage"))
    {
        let existing = msg.get("content").and_then(Value::as_str).unwrap_or("");
        let joined = if existing.is_empty() {
            instruction.to_string()
        } else {
            format!("{existing}\n\n{instruction}")
        };
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("content".to_string(), Value::String(joined));
        }
    }
    repaired
}

/// Accumulated output of a full first attempt, used to classify whether a
/// repair retry is warranted (9router `readIntegrityAttempt` output).
#[derive(Debug, Clone, Default)]
pub struct KiroAttemptOutput {
    pub content: String,
    pub reasoning: String,
    pub has_tool_calls: bool,
    pub saw_error: bool,
}

/// Inspect a raw OpenAI-chunk SSE body and accumulate content/reasoning/tool
/// calls (9router `inspectSSEChunk`). Malformed lines are skipped silently —
/// the transform path diagnoses them.
pub fn inspect_sse_body(body: &[u8], output: &mut KiroAttemptOutput) {
    let text = String::from_utf8_lossy(body);
    for line in text.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data: ") {
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if event.get("error").is_some() {
                output.saw_error = true;
            }
            if let Some(choices) = event.get("choices").and_then(Value::as_array) {
                for choice in choices {
                    let Some(delta) = choice.get("delta") else {
                        continue;
                    };
                    if let Some(c) = delta.get("content").and_then(Value::as_str) {
                        output.content.push_str(c);
                    }
                    if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
                        output.reasoning.push_str(r);
                    }
                    if delta
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .is_some_and(|t| !t.is_empty())
                    {
                        output.has_tool_calls = true;
                    }
                }
            }
        }
    }
}

/// Classify a completed first attempt (9router `readIntegrityAttempt` tail):
/// ellipsis / short_final when no tool calls and the content looks truncated;
/// otherwise no repair.
pub fn classify_attempt(output: &KiroAttemptOutput) -> KiroRepairKind {
    if output.has_tool_calls || output.saw_error {
        return KiroRepairKind::None;
    }
    if is_ellipsis_only(&output.content)
        || (output.content.trim().is_empty() && is_ellipsis_only(&output.reasoning))
    {
        return KiroRepairKind::Ellipsis;
    }
    if is_short_future_action(&output.content) {
        return KiroRepairKind::ShortFinal;
    }
    KiroRepairKind::None
}

/// Emit an SSE error frame with a `kiro_*` code, mirroring JS `encodeSSEError`
/// (kiro.js:187-194): `data: {"error":{...}}` then `data: [DONE]`.
pub fn encode_sse_error(code: &str, message: &str, details: Option<Value>) -> Vec<u8> {
    let mut err = serde_json::Map::new();
    err.insert("message".into(), Value::String(message.to_string()));
    err.insert("type".into(), Value::String("upstream_error".to_string()));
    err.insert("code".into(), Value::String(code.to_string()));
    if let Some(d) = details {
        err.insert("details".into(), d);
    }
    let frame = json!({ "error": Value::Object(err) });
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "data: {}\n\n",
            serde_json::to_string(&frame).unwrap_or_default()
        )
        .as_bytes(),
    );
    out.extend_from_slice(b"data: [DONE]\n\n");
    out
}

/// Classify a stop disposition (9router `stopDisposition`, kiro.js:147-156).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDisposition {
    Complete,
    ToolUse,
    Length,
    RetryableProtocolFailure,
    TerminalIncomplete,
    TerminalRefusal,
    UnknownFailure,
}

impl StopDisposition {
    /// JS `stopDisposition` string for diagnostics / failure-code mapping.
    pub fn as_str(&self) -> &'static str {
        match self {
            StopDisposition::Complete => "complete",
            StopDisposition::ToolUse => "tool_use",
            StopDisposition::Length => "length",
            StopDisposition::RetryableProtocolFailure => "retryable_protocol_failure",
            StopDisposition::TerminalIncomplete => "terminal_incomplete",
            StopDisposition::TerminalRefusal => "terminal_refusal",
            StopDisposition::UnknownFailure => "unknown_failure",
        }
    }
}

kiro_re!(
    refusal_like,
    r"(?i)(?:content.*filter|guardrail|safety|policy|blocked)"
);

/// Normalize a raw stop reason (9router `normalizeStopReason`, kiro.js:145-151):
/// trim, camelCase → snake_case, whitespace/hyphens → underscores, then fold
/// known aliases (`stop`/`stop_sequence` → `end_turn`, `tool_calls` →
/// `tool_use`, `length`/`max_output_tokens` → `max_tokens`).
pub fn normalize_stop_reason(value: Option<&str>) -> Option<String> {
    let raw = value.unwrap_or("").trim();
    if raw.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut prev_is_lower = false;
    for c in raw.chars() {
        if c.is_ascii_uppercase() {
            if prev_is_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_is_lower = false;
        } else if c.is_whitespace() || c == '-' {
            out.push('_');
            prev_is_lower = false;
        } else {
            out.push(c.to_ascii_lowercase());
            prev_is_lower = c.is_ascii_lowercase();
        }
    }
    let normalized = match out.as_str() {
        "endturn" | "end_turn" | "stop" | "stop_sequence" => "end_turn",
        "tooluse" | "tool_use" | "tool_calls" => "tool_use",
        "maxtokens" | "max_tokens" | "max_output_tokens" | "length" => "max_tokens",
        other => other,
    };
    Some(normalized.to_string())
}

pub fn stop_disposition(stop_reason: Option<&str>, has_tool_calls: bool) -> StopDisposition {
    let reason = normalize_stop_reason(stop_reason);
    let reason = reason.as_deref();
    if matches!(
        reason,
        Some("malformed_model_output" | "invalid_model_output")
    ) {
        return StopDisposition::RetryableProtocolFailure;
    }
    if matches!(
        reason,
        Some("cancelled" | "pause_turn" | "model_context_window_exceeded")
    ) {
        return StopDisposition::TerminalIncomplete;
    }
    if reason == Some("refusal") || reason.is_some_and(|r| refusal_like().is_match(r)) {
        return StopDisposition::TerminalRefusal;
    }
    if reason == Some("max_tokens") {
        return if has_tool_calls {
            StopDisposition::TerminalIncomplete
        } else {
            StopDisposition::Length
        };
    }
    if reason.is_some() && !matches!(reason, Some("end_turn" | "tool_use")) {
        return StopDisposition::UnknownFailure;
    }
    if has_tool_calls || reason == Some("tool_use") {
        return StopDisposition::ToolUse;
    }
    if reason.is_none() || reason == Some("end_turn") {
        return StopDisposition::Complete;
    }
    StopDisposition::UnknownFailure
}

/// Severity used to merge competing stop reasons (9router `mergeStopReason`,
/// kiro.js:170-183): derived from the disposition, terminal refusal wins.
fn stop_reason_severity(reason: &str) -> u8 {
    match stop_disposition(Some(reason), false) {
        StopDisposition::TerminalRefusal => 6,
        StopDisposition::TerminalIncomplete => 5,
        StopDisposition::UnknownFailure => 4,
        StopDisposition::RetryableProtocolFailure => 3,
        StopDisposition::Length => 2,
        StopDisposition::Complete | StopDisposition::ToolUse => 1,
    }
}

/// Merge two stop reasons keeping the higher severity (9router
/// `mergeStopReason`). `None` means "no reason seen yet".
pub fn merge_stop_reason(current: Option<&str>, incoming: Option<&str>) -> Option<String> {
    match (current, incoming) {
        (None, incoming) => incoming.map(str::to_string),
        (Some(c), None) => Some(c.to_string()),
        (Some(c), Some(i)) => {
            if stop_reason_severity(i) > stop_reason_severity(c) {
                Some(i.to_string())
            } else {
                Some(c.to_string())
            }
        }
    }
}

/// Stop reasons that mean "usable as far as it got, then the budget ran out"
/// (9router `KIRO_TRUNCATION_STOP_REASONS`, kiro.js:157). `cancelled` /
/// `pause_turn` are abandoned turns whose partial content must stay private,
/// so they are deliberately absent.
pub const KIRO_TRUNCATION_STOP_REASONS: &[&str] = &["model_context_window_exceeded", "max_tokens"];

/// True when a terminal-incomplete turn still keeps its streamed output: the
/// stop reason is a truncation reason and at least one chunk already reached
/// the client (finish_reason remaps to `"length"`). Mirrors the JS
/// `declaredTruncatedAfterOutput` / `truncatedAfterOutput` checks
/// (kiro.js:1014-1015,1079-1080), which derive the disposition with the
/// turn's tool-use state — so `max_tokens` only truncates on a tool turn.
pub fn is_truncated_after_output(
    stop_reason: Option<&str>,
    has_tool_calls: bool,
    emitted_chunks: bool,
) -> bool {
    if !emitted_chunks {
        return false;
    }
    let normalized = normalize_stop_reason(stop_reason);
    normalized
        .as_deref()
        .is_some_and(|r| KIRO_TRUNCATION_STOP_REASONS.contains(&r))
        && stop_disposition(stop_reason, has_tool_calls) == StopDisposition::TerminalIncomplete
}

/// Retry only endpoint/auth-surface failures (9router `shouldRetry`,
/// kiro.js:338-342 + `KIRO_ENDPOINT_FALLBACK_STATUSES`). Payload-invalid 400
/// is terminal: sending the same body to every surface cannot repair it.
pub fn should_retry_status(status: u16, has_fallback: bool) -> bool {
    const KIRO_ENDPOINT_FALLBACK_STATUSES: &[u16] = &[401, 403, 404];
    has_fallback && KIRO_ENDPOINT_FALLBACK_STATUSES.contains(&status)
}

/// Outcome of one integrity-gated attempt (9router `readIntegrityAttempt`
/// `kind`, kiro.js:521-627).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityAttemptKind {
    Complete,
    Ellipsis,
    ShortFinal,
    InvalidTool,
    RetryableStop,
    TerminalStop,
    UpstreamError,
    MissingTerminal,
}

/// What `runIntegrityRecovery` does with a first-attempt outcome (9router
/// kiro.js:437-505), as a pure decision: return the bytes, fail terminally,
/// repair with an appended instruction, or retry the body unmodified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    Complete,
    FailTerminal,
    FailInvalidToolDisabled,
    Repair(KiroRepairKind),
    RetryUnmodified,
}

pub fn recovery_action(kind: IntegrityAttemptKind, repair_enabled: bool) -> RecoveryAction {
    match kind {
        IntegrityAttemptKind::Complete => RecoveryAction::Complete,
        IntegrityAttemptKind::TerminalStop | IntegrityAttemptKind::UpstreamError => {
            RecoveryAction::FailTerminal
        }
        IntegrityAttemptKind::InvalidTool if !repair_enabled => {
            RecoveryAction::FailInvalidToolDisabled
        }
        IntegrityAttemptKind::Ellipsis => RecoveryAction::Repair(KiroRepairKind::Ellipsis),
        IntegrityAttemptKind::ShortFinal => RecoveryAction::Repair(KiroRepairKind::ShortFinal),
        IntegrityAttemptKind::InvalidTool => RecoveryAction::Repair(KiroRepairKind::InvalidTool),
        IntegrityAttemptKind::RetryableStop | IntegrityAttemptKind::MissingTerminal => {
            RecoveryAction::RetryUnmodified
        }
    }
}

/// SSE error code for a terminal first/retry attempt (9router
/// `integrityFailureSSE`, kiro.js:507-519).
pub fn integrity_failure_code(
    kind: IntegrityAttemptKind,
    terminal_provenance: Option<&str>,
    disposition: StopDisposition,
) -> &'static str {
    if terminal_provenance == Some("integrity_buffer_exceeded") {
        return "kiro_integrity_buffer_exceeded";
    }
    if kind == IntegrityAttemptKind::UpstreamError {
        return "kiro_upstream_eventstream_error";
    }
    match disposition {
        StopDisposition::TerminalRefusal => "kiro_terminal_refusal",
        StopDisposition::TerminalIncomplete => "kiro_terminal_incomplete",
        _ => "kiro_unknown_stop_reason",
    }
}

/// SSE error code when the bounded retry still fails (9router
/// `runIntegrityRecovery` tail, kiro.js:493-499).
pub fn retry_failure_code(kind: IntegrityAttemptKind) -> &'static str {
    match kind {
        IntegrityAttemptKind::Ellipsis => "kiro_ellipsis_retry_failed",
        IntegrityAttemptKind::ShortFinal => "kiro_short_final_retry_failed",
        IntegrityAttemptKind::InvalidTool => "kiro_tool_call_repair_retry_failed",
        _ => "kiro_missing_terminal_retry_failed",
    }
}

/// Decode a raw kiro response body (binary AWS EventStream) into OpenAI-shaped
/// SSE text by feeding it through the shared `kiro_to_openai_streaming`
/// transform. This is the decode-first step the JS `readIntegrityAttempt`
/// performs before classification (kiro.js:517-524).
pub fn decode_body_to_sse(body: &[u8]) -> String {
    let mut state = crate::core::translator::registry::ResponseTransformState::default();
    let mut sse = String::new();
    // Feed the body in one chunk (the transform buffers partial frames).
    let lines = crate::core::translator::response::kiro_to_openai::kiro_to_openai_streaming(
        body, &mut state,
    );
    for line in lines {
        sse.push_str(&line);
        sse.push('\n');
    }
    sse
}

/// Classify a fully-buffered raw kiro body by first decoding to SSE, then
/// inspecting the transformed chunks. Returns the repair kind.
pub fn classify_buffered_body(body: &[u8]) -> KiroRepairKind {
    let sse = decode_body_to_sse(body);
    let mut output = KiroAttemptOutput::default();
    inspect_sse_body(sse.as_bytes(), &mut output);
    classify_attempt(&output)
}

pub struct KiroExecutorResponse {
    pub response: super::UpstreamResponse,
    pub url: String,
    pub headers: HeaderMap,
    pub transformed_body: Value,
    pub transport: super::TransportKind,
}

impl std::fmt::Debug for KiroExecutorResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiroExecutorResponse")
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("transformed_body", &self.transformed_body)
            .field("transport", &self.transport)
            .finish()
    }
}

#[derive(Debug)]
pub enum KiroExecutorError {
    MissingCredentials(String),
    InvalidCredentials(String),
    SigningError(String),
    InvalidHeader(reqwest::header::InvalidHeaderValue),
    InvalidUri(InvalidUri),
    InvalidRequest(http::Error),
    Serialize(serde_json::Error),
    HyperClientInit(std::io::Error),
    Hyper(hyper_util::client::legacy::Error),
    Request(reqwest::Error),
    /// An endpoint/auth-surface failure (401/403/404) on a URL that still had
    /// fallback surfaces left — mirrors 9router shouldRetry.
    EndpointStatus {
        status: u16,
        url: String,
        message: String,
    },
    EventStreamDecode(String),
    UnsupportedFormat(String),
}

impl From<reqwest::Error> for KiroExecutorError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<reqwest::header::InvalidHeaderValue> for KiroExecutorError {
    fn from(error: reqwest::header::InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }
}

impl From<InvalidUri> for KiroExecutorError {
    fn from(error: InvalidUri) -> Self {
        Self::InvalidUri(error)
    }
}

impl From<http::Error> for KiroExecutorError {
    fn from(error: http::Error) -> Self {
        Self::InvalidRequest(error)
    }
}

impl From<serde_json::Error> for KiroExecutorError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

impl From<std::io::Error> for KiroExecutorError {
    fn from(error: std::io::Error) -> Self {
        Self::HyperClientInit(error)
    }
}

impl From<hyper_util::client::legacy::Error> for KiroExecutorError {
    fn from(error: hyper_util::client::legacy::Error) -> Self {
        Self::Hyper(error)
    }
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct KiroExecutor {
    pool: Arc<ClientPool>,
    provider_node: Option<ProviderNode>,
}

impl KiroExecutor {
    pub fn new(
        pool: Arc<ClientPool>,
        provider_node: Option<ProviderNode>,
    ) -> Result<Self, KiroExecutorError> {
        Ok(Self {
            pool,
            provider_node,
        })
    }

    pub fn parse_aws_credentials(access_token: &str) -> Result<AwsCredentials, KiroExecutorError> {
        let credentials: AwsCredentials = serde_json::from_str(access_token).map_err(|e| {
            KiroExecutorError::InvalidCredentials(format!("JSON parse error: {}", e))
        })?;

        if credentials.access_key.is_empty() || credentials.secret_key.is_empty() {
            return Err(KiroExecutorError::InvalidCredentials(
                "AWS credentials missing access_key or secret_key".to_string(),
            ));
        }

        Ok(credentials)
    }

    /// Auth-aware URL order (9router getOrderedBaseUrls, kiro.js 299-328).
    /// kiro.dev must never be first for any auth method (the legacy path
    /// gateway answers valid modern payloads with terminal 400
    /// REQUEST_BODY_INVALID). Amazon surfaces reject foreign tokens with
    /// 401/403, which DO fall through, so q → codewhisperer → others is safe
    /// for every auth method (CLIRO parity). amazonaws.com hosts are
    /// regionalized to the token's region when the account specifies one.
    pub fn build_url(
        &self,
        _model: &str,
        _stream: bool,
        credentials: &ProviderConnection,
    ) -> Vec<String> {
        let base_urls: Vec<String> = KIRO_BASE_URLS.iter().map(|s| (*s).to_string()).collect();

        // 9router getOrderedBaseUrls regionalization: rewrite the AWS region
        // segment of every amazonaws.com host to the token's region.
        // `([a-z]+)\.[a-z0-9-]+\.amazonaws\.com` → `$1.{region}.amazonaws.com`.
        let region = credentials
            .provider_specific_data
            .get("region")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|r| !r.is_empty() && *r != "us-east-1")
            .unwrap_or("");

        let regionalize = |u: &str| -> String {
            if region.is_empty() || !u.contains("amazonaws.com") {
                return u.to_string();
            }
            regionalize_host(u, region)
        };

        let amazon: Vec<String> = base_urls
            .iter()
            .filter(|u| u.contains("amazonaws.com"))
            .map(|u| regionalize(u))
            .collect();
        let others: Vec<String> = base_urls
            .iter()
            .filter(|u| !u.contains("amazonaws.com"))
            .cloned()
            .collect();

        let q: Vec<String> = amazon
            .iter()
            .filter(|u| u.contains("://q."))
            .cloned()
            .collect();
        let remaining: Vec<String> = amazon
            .iter()
            .filter(|u| !u.contains("://q."))
            .cloned()
            .collect();
        if !q.is_empty() {
            return q.into_iter().chain(remaining).chain(others).collect();
        }
        amazon.into_iter().chain(others).collect()
    }

    /// Port of 9router buildHeaders verbatim (kiro.js 235-282): registry
    /// headers + Amz-Sdk-Request/Invocation-Id, conditional X-Amz-Target on
    /// the codewhisperer surface, api_key/external_idp TokenType handling,
    /// then the runtime-surface headers (x-amz-sso-bearer when accessToken,
    /// x-amzn-kiro-agent-mode=spec, machine-id, optional profile-arn).
    fn build_bearer_headers(
        &self,
        credentials: &ProviderConnection,
        url: &str,
    ) -> Result<HeaderMap, KiroExecutorError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.amazon.eventstream"),
        );
        headers.insert(
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static("AWS-SDK-JS/3.0.0 kiro-ide/1.0.0"),
        );
        headers.insert(
            HeaderName::from_static("x-amz-user-agent"),
            HeaderValue::from_static("aws-sdk-js/3.0.0 kiro-ide/1.0.0"),
        );
        headers.insert(
            HeaderName::from_static("amz-sdk-request"),
            HeaderValue::from_static("attempt=1; max=3"),
        );
        let inv_id = uuid::Uuid::new_v4().to_string();
        headers.insert(
            HeaderName::from_static("amz-sdk-invocation-id"),
            HeaderValue::from_str(&inv_id).map_err(KiroExecutorError::InvalidHeader)?,
        );
        if url.contains("://codewhisperer.") {
            headers.insert(
                HeaderName::from_static("x-amz-target"),
                HeaderValue::from_static(KIRO_CODEWHISPERER_TARGET),
            );
        }

        let auth_method = credentials
            .provider_specific_data
            .get("authMethod")
            .or_else(|| credentials.provider_specific_data.get("auth_method"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_api_key = auth_method == "api_key";
        let is_external_idp = auth_method == "external_idp";

        let api_key = credentials.api_key.as_deref().or(if is_api_key {
            credentials.access_token.as_deref()
        } else {
            None
        });

        if is_api_key {
            let key = api_key
                .or(credentials.access_token.as_deref())
                .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".into()))?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(KiroExecutorError::InvalidHeader)?,
            );
            headers.insert(
                HeaderName::from_static("TokenType"),
                HeaderValue::from_static("API_KEY"),
            );
        } else {
            let token = credentials
                .access_token
                .as_deref()
                .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".into()))?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(KiroExecutorError::InvalidHeader)?,
            );
            if is_external_idp {
                headers.insert(
                    HeaderName::from_static("TokenType"),
                    HeaderValue::from_static("EXTERNAL_IDP"),
                );
            }
        }

        // CLIRO parity for the Amazon surfaces: the Kiro runtime accepts the
        // SSO bearer header + agent-mode marker (kiro.js 268-279). Without
        // these the deprecated path gateway answers REQUEST_BODY_INVALID.
        if let Some(token) = credentials.access_token.as_deref() {
            headers.insert(
                HeaderName::from_static("x-amz-sso-bearer"),
                HeaderValue::from_str(token).map_err(KiroExecutorError::InvalidHeader)?,
            );
        }
        headers.insert(
            HeaderName::from_static("x-amzn-kiro-agent-mode"),
            HeaderValue::from_static("spec"),
        );
        headers.insert(
            HeaderName::from_static("x-amzn-codewhisperer-machine-id"),
            HeaderValue::from_static("kiro-desktop"),
        );
        if let Some(profile_arn) = credentials
            .provider_specific_data
            .get("profileArn")
            .and_then(|v| v.as_str())
        {
            headers.insert(
                HeaderName::from_static("x-amzn-codewhisperer-profile-arn"),
                HeaderValue::from_str(profile_arn).map_err(KiroExecutorError::InvalidHeader)?,
            );
        }
        Ok(headers)
    }

    /// Send the request to one URL and return the raw response (headers +
    /// post). Shared by the URL failover loop and the integrity repair retry.
    async fn send_one(
        &self,
        url: &str,
        body: &Value,
        credentials: &ProviderConnection,
        stream: bool,
    ) -> Result<(reqwest::Response, HeaderMap), KiroExecutorError> {
        let body_bytes = serde_json::to_vec(body)?;
        let content_hash = sha256_hex(&body_bytes);

        // AWS JSON credentials → SigV4 (IDC / some enterprise paths)
        let is_aws_auth = credentials
            .access_token
            .as_deref()
            .map(|t| t.trim_start().starts_with('{'))
            .unwrap_or(false);

        let headers = if is_aws_auth {
            let creds = match Self::parse_aws_credentials(
                credentials
                    .access_token
                    .as_deref()
                    .ok_or_else(|| KiroExecutorError::MissingCredentials("kiro".to_string()))?,
            ) {
                Ok(c) => c,
                Err(e) => return Err(e),
            };
            self.sign_request(url, &creds, &content_hash, stream)
                .await?
        } else {
            self.build_bearer_headers(credentials, url)?
        };

        let client = self.pool.get("kiro", None)?;
        let response = client
            .post(url)
            .headers(headers.clone())
            .body(body_bytes)
            .send()
            .await?;
        Ok((response, headers))
    }

    pub async fn execute_request(
        &self,
        request: KiroExecutionRequest,
    ) -> Result<KiroExecutorResponse, KiroExecutorError> {
        let urls = self.build_url(&request.model, request.stream, &request.credentials);

        // Try each URL with failover
        let mut last_error = None;
        for (url_index, url) in urls.iter().enumerate() {
            let (response, headers) = match self
                .send_one(url, &request.body, &request.credentials, request.stream)
                .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            {
                // 9router shouldRetry: endpoint/auth-surface failures
                // (401/403/404) fall through to the next URL — the same
                // payload can succeed on a different surface. Payload-invalid
                // 400 is terminal (sending the same body everywhere cannot
                // repair it). EventStream→SSE conversion runs in
                // kiro_to_openai_streaming (ResponseTransform path).
                let status = response.status().as_u16();
                let has_fallback = url_index + 1 < urls.len();
                if should_retry_status(status, has_fallback) {
                    last_error = Some(KiroExecutorError::EndpointStatus {
                        status,
                        url: url.clone(),
                        message: format!(
                            "Kiro endpoint {} returned {}; trying next surface",
                            url,
                            response.status()
                        ),
                    });
                    continue;
                }
                // 9router integrity repair (kiro.js attachIntegrityGate +
                // runIntegrityRecovery): when enabled (per-account
                // kiroToolCallRepair, default on) and the response is a
                // complete SSE body, classify it and retry ONCE with a
                // repair instruction appended to the current user turn when
                // the first attempt ended retryably (ellipsis / short
                // future action). The response is otherwise returned
                // untouched so the streaming path stays first-class.
                let repair_enabled = request
                    .credentials
                    .provider_specific_data
                    .get("kiroToolCallRepair")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);

                if repair_enabled && status == 200 {
                    // Buffer the body with a bounded cap (JS KIRO_REPAIR_BUFFER_MAX_BYTES).
                    let mut full = Vec::new();
                    {
                        use futures_util::StreamExt;
                        let mut stream = response.bytes_stream();
                        let mut over_budget = false;
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(bytes) => {
                                    if full.len() + bytes.len() > KIRO_REPAIR_BUFFER_MAX_BYTES {
                                        over_budget = true;
                                        break;
                                    }
                                    full.extend_from_slice(&bytes);
                                }
                                Err(e) => {
                                    return Ok(KiroExecutorResponse {
                                        response: UpstreamResponse::Reqwest(
                                            http::Response::builder()
                                                .status(200)
                                                .header("content-type", "text/event-stream")
                                                .body(reqwest::Body::from(encode_sse_error(
                                                    "kiro_integrity_buffer_exceeded",
                                                    "Kiro integrity repair buffer exceeded the 8 MiB cap",
                                                    None,
                                                )))
                                                .map_err(KiroExecutorError::InvalidRequest)?
                                                .into(),
                                        ),
                                        url: url.clone(),
                                        headers,
                                        transformed_body: request.body.clone(),
                                        transport: TransportKind::Reqwest,
                                    });
                                }
                            }
                        }
                        if over_budget {
                            return Ok(KiroExecutorResponse {
                                response: UpstreamResponse::Reqwest(
                                    http::Response::builder()
                                        .status(200)
                                        .header("content-type", "text/event-stream")
                                        .body(reqwest::Body::from(encode_sse_error(
                                            "kiro_integrity_buffer_exceeded",
                                            "Kiro integrity repair buffer exceeded the 8 MiB cap",
                                            None,
                                        )))
                                        .map_err(KiroExecutorError::InvalidRequest)?
                                        .into(),
                                ),
                                url: url.clone(),
                                headers,
                                transformed_body: request.body.clone(),
                                transport: TransportKind::Reqwest,
                            });
                        }
                    }

                    // Decode-first classification: the body is binary AWS
                    // EventStream, so decode to SSE before inspecting (JS
                    // readIntegrityAttempt transforms first).
                    let kind = classify_buffered_body(&full);
                    if kind != KiroRepairKind::None {
                        // One bounded retry with the repair instruction
                        // appended to the current user turn (9router
                        // runIntegrityRecovery). The retry goes through the
                        // same per-URL send so SigV4/bearer auth is rebuilt.
                        let repaired_body = append_repair_instruction(&request.body, kind);
                        let retry = self
                            .send_one(url, &repaired_body, &request.credentials, request.stream)
                            .await;
                        match retry {
                            Ok((retry_response, retry_headers)) => {
                                // Diagnose the retry: if it is still not
                                // complete, emit the matching kiro_*_retry_failed
                                // code (JS runIntegrityRecovery, kiro.js:457-478).
                                let retry_status = retry_response.status().as_u16();
                                if retry_status == 200 {
                                    let retry_bytes = retry_response
                                        .bytes()
                                        .await
                                        .map_err(KiroExecutorError::Request)?;
                                    let retry_kind = classify_buffered_body(&retry_bytes);
                                    if retry_kind == KiroRepairKind::None {
                                        // Complete: return the retry response.
                                        return Ok(KiroExecutorResponse {
                                            response: UpstreamResponse::Reqwest(
                                                http::Response::builder()
                                                    .status(200)
                                                    .header("content-type", "text/event-stream")
                                                    .header("cache-control", "no-cache")
                                                    .body(reqwest::Body::from(retry_bytes))
                                                    .map_err(KiroExecutorError::InvalidRequest)?
                                                    .into(),
                                            ),
                                            url: url.clone(),
                                            headers: retry_headers,
                                            transformed_body: request.body.clone(),
                                            transport: TransportKind::Reqwest,
                                        });
                                    }
                                    // Retry still failed — emit the specific code
                                    // (9router runIntegrityRecovery tail).
                                    let retry_attempt_kind = match retry_kind {
                                        KiroRepairKind::Ellipsis => IntegrityAttemptKind::Ellipsis,
                                        KiroRepairKind::ShortFinal => {
                                            IntegrityAttemptKind::ShortFinal
                                        }
                                        KiroRepairKind::InvalidTool => {
                                            IntegrityAttemptKind::InvalidTool
                                        }
                                        KiroRepairKind::None => {
                                            IntegrityAttemptKind::MissingTerminal
                                        }
                                    };
                                    let code = retry_failure_code(retry_attempt_kind);
                                    return Ok(KiroExecutorResponse {
                                        response: UpstreamResponse::Reqwest(
                                            http::Response::builder()
                                                .status(200)
                                                .header("content-type", "text/event-stream")
                                                .body(reqwest::Body::from(encode_sse_error(
                                                    code,
                                                    "Kiro integrity validation failed after one bounded retry",
                                                    Some(json!({ "kind": format!("{retry_kind:?}") })),
                                                )))
                                                .map_err(KiroExecutorError::InvalidRequest)?
                                                .into(),
                                        ),
                                        url: url.clone(),
                                        headers: retry_headers,
                                        transformed_body: request.body.clone(),
                                        transport: TransportKind::Reqwest,
                                    });
                                }
                                // Retry returned non-200 → upstream error.
                                let body = String::from_utf8_lossy(
                                    &retry_response.bytes().await.unwrap_or_default(),
                                )
                                .to_string();
                                return Ok(KiroExecutorResponse {
                                    response: UpstreamResponse::Reqwest(
                                        http::Response::builder()
                                            .status(200)
                                            .header("content-type", "text/event-stream")
                                            .body(reqwest::Body::from(encode_sse_error(
                                                "kiro_integrity_retry_upstream_error",
                                                &format!("Kiro integrity retry failed with HTTP {retry_status}: {body}"),
                                                Some(json!({ "status": retry_status })),
                                            )))
                                            .map_err(KiroExecutorError::InvalidRequest)?
                                            .into(),
                                    ),
                                    url: url.clone(),
                                    headers: retry_headers,
                                    transformed_body: request.body.clone(),
                                    transport: TransportKind::Reqwest,
                                });
                            }
                            Err(e) => {
                                last_error = Some(e);
                                continue;
                            }
                        }
                    }
                    // Not retryable: return the buffered first attempt
                    // (already OpenAI-chunk SSE).
                    let http_response = http::Response::builder()
                        .status(200)
                        .header("content-type", "text/event-stream")
                        .header("cache-control", "no-cache")
                        .body(reqwest::Body::from(full))
                        .map_err(KiroExecutorError::InvalidRequest)?;
                    return Ok(KiroExecutorResponse {
                        response: UpstreamResponse::Reqwest(http_response.into()),
                        url: url.clone(),
                        headers,
                        transformed_body: request.body.clone(),
                        transport: TransportKind::Reqwest,
                    });
                }

                return Ok(KiroExecutorResponse {
                    response: UpstreamResponse::Reqwest(response),
                    url: url.clone(),
                    headers,
                    transformed_body: request.body.clone(),
                    transport: TransportKind::Reqwest,
                });
            }
        }

        Err(last_error.unwrap_or_else(|| {
            KiroExecutorError::SigningError("All Kiro endpoints failed".to_string())
        }))
    }

    async fn sign_request(
        &self,
        url: &str,
        credentials: &AwsCredentials,
        content_hash: &str,
        _stream: bool,
    ) -> Result<HeaderMap, KiroExecutorError> {
        use hmac::{Hmac, KeyInit, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.amazon.eventstream"),
        );

        let timestamp = chrono::Utc::now();
        let date_time = timestamp.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = timestamp.format("%Y%m%d").to_string();

        // Extract host from the actual URL for SigV4 signing
        let parsed_url =
            url::Url::parse(url).map_err(|e| KiroExecutorError::SigningError(e.to_string()))?;
        let host = parsed_url
            .host_str()
            .unwrap_or("runtime.us-east-1.kiro.dev");
        let region = KIRO_REGION;
        let service = KIRO_SERVICE;

        let x_amz_date = HeaderName::from_bytes(b"x-amz-date").unwrap();
        headers.insert(
            x_amz_date,
            HeaderValue::from_str(&date_time).map_err(KiroExecutorError::InvalidHeader)?,
        );

        let x_amz_content_sha256 = HeaderName::from_bytes(b"x-amz-content-sha256").unwrap();
        headers.insert(
            x_amz_content_sha256,
            HeaderValue::from_str(content_hash).map_err(KiroExecutorError::InvalidHeader)?,
        );

        let nonce = generate_nonce();
        let x_amz_nonce = HeaderName::from_bytes(b"x-amz-nonce").unwrap();
        headers.insert(
            x_amz_nonce,
            HeaderValue::from_str(&nonce).map_err(KiroExecutorError::InvalidHeader)?,
        );

        if let Some(ref session_token) = credentials.session_token {
            let x_amz_security_token = HeaderName::from_bytes(b"x-amz-security-token").unwrap();
            headers.insert(
                x_amz_security_token,
                HeaderValue::from_str(session_token).map_err(KiroExecutorError::InvalidHeader)?,
            );
        }

        let method = "POST";
        let parsed_url =
            url::Url::parse(url).map_err(|e| KiroExecutorError::SigningError(e.to_string()))?;
        let path = parsed_url.path();
        let query = parsed_url.query().unwrap_or("");

        let canonical_headers = format!(
            "accept:application/vnd.amazon.eventstream\ncontent-type:application/json\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\nx-amz-nonce:{}{}",
            host,
            content_hash,
            date_time,
            nonce,
            if let Some(token) = &credentials.session_token {
                format!("\nx-amz-security-token:{token}")
            } else {
                String::new()
            }
        );

        let signed_headers_str = if credentials.session_token.is_some() {
            "accept;content-type;host;x-amz-content-sha256;x-amz-date;x-amz-nonce;x-amz-security-token"
        } else {
            "accept;content-type;host;x-amz-content-sha256;x-amz-date;x-amz-nonce"
        };
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, path, query, canonical_headers, signed_headers_str, content_hash
        );

        let canonical_request_hash = sha256_hex(canonical_request.as_bytes());

        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            date_time, credential_scope, canonical_request_hash
        );

        let mut k_date =
            HmacSha256::new_from_slice(format!("AWS4{}", credentials.secret_key).as_bytes())
                .expect("HMAC key length is valid");
        k_date.update(date_stamp.as_bytes());
        let k_date = k_date.finalize().into_bytes();

        let mut k_region = HmacSha256::new_from_slice(&k_date).expect("HMAC key length is valid");
        k_region.update(region.as_bytes());
        let k_region = k_region.finalize().into_bytes();

        let mut k_service =
            HmacSha256::new_from_slice(&k_region).expect("HMAC key length is valid");
        k_service.update(service.as_bytes());
        let k_service = k_service.finalize().into_bytes();

        let mut k_signing =
            HmacSha256::new_from_slice(&k_service).expect("HMAC key length is valid");
        k_signing.update(b"aws4_request");
        let k_signing = k_signing.finalize().into_bytes();

        let mut signature =
            HmacSha256::new_from_slice(&k_signing).expect("HMAC key length is valid");
        signature.update(string_to_sign.as_bytes());
        let signature = hex::encode(signature.finalize().into_bytes());

        let auth_header = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            credentials.access_key, credential_scope, signed_headers_str, signature
        );

        let authorization = HeaderName::from_bytes(b"authorization").unwrap();
        headers.insert(
            authorization,
            HeaderValue::from_str(&auth_header).map_err(KiroExecutorError::InvalidHeader)?,
        );

        Ok(headers)
    }

    pub fn pool(&self) -> &Arc<ClientPool> {
        &self.pool
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsCredentials {
    pub access_key: String,
    pub secret_key: String,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub expiration: Option<String>,
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn generate_nonce() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    hex::encode(bytes)
}

/// Strip <thinking>...</thinking> blocks from Kiro streamed SSE content.
/// 9router open-sse/executors/kiro.js:~L165-180 parity.
fn strip_thinking_tags(data: &str) -> String {
    // Fast path: no thinking tags
    if !data.contains("<thinking") {
        return data.to_string();
    }
    let mut result = String::with_capacity(data.len());
    let mut remaining = data;
    while let Some(start) = remaining.find("<thinking") {
        // Append everything before <thinking
        result.push_str(&remaining[..start]);
        // Find the closing tag
        if let Some(end) = remaining[start..].find("</thinking>") {
            let close = start + end + "</thinking>".len();
            remaining = &remaining[close..];
        } else {
            // Unclosed tag — remove from <thinking to end
            break;
        }
    }
    result
}

pub struct EventStreamDecoder;

impl EventStreamDecoder {
    /// AWS EventStream v1 binary message prelude: 12 bytes.
    const PRELUDE_LEN: usize = 12;
    /// Trailing message CRC: 4 bytes.
    const TRAILING_CRC_LEN: usize = 4;

    /// Decode one or more complete AWS EventStream v1 binary frames into
    /// structured events. Partial trailing frames are ignored (the caller
    /// buffers across chunks).
    pub fn decode_chunk(data: &[u8]) -> Result<Vec<KiroEvent>, KiroExecutorError> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        let mut offset = 0;

        while offset + Self::PRELUDE_LEN <= data.len() {
            // Parse the 12-byte prelude
            let prelude = &data[offset..offset + Self::PRELUDE_LEN];
            let total_length =
                u32::from_be_bytes([prelude[0], prelude[1], prelude[2], prelude[3]]) as usize;
            let headers_length =
                u32::from_be_bytes([prelude[4], prelude[5], prelude[6], prelude[7]]) as usize;
            let prelude_crc =
                u32::from_be_bytes([prelude[8], prelude[9], prelude[10], prelude[11]]);

            // Validate total length
            if !(Self::PRELUDE_LEN + Self::TRAILING_CRC_LEN..=MAX_EVENTSTREAM_MESSAGE_LENGTH)
                .contains(&total_length)
            {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "invalid message total_length={}",
                    total_length
                )));
            }

            // Validate headers length
            if headers_length > total_length - Self::PRELUDE_LEN - Self::TRAILING_CRC_LEN {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "invalid headers_length={} for total_length={}",
                    headers_length, total_length
                )));
            }

            // Verify prelude CRC (CRC32 of first 8 bytes)
            let expected_crc = crc32fast::hash(&prelude[..8]);
            if prelude_crc != expected_crc {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "prelude CRC mismatch: got {:#010x}, expected {:#010x}",
                    prelude_crc, expected_crc
                )));
            }

            // Check we have enough data for the full message
            if offset + total_length > data.len() {
                break;
            }

            let payload_start = offset + Self::PRELUDE_LEN + headers_length;
            let payload_end = offset + total_length - Self::TRAILING_CRC_LEN;
            let crc_start = offset + total_length - Self::TRAILING_CRC_LEN;

            // Verify message CRC (CRC32 of everything except the trailing 4 bytes)
            let message_crc = u32::from_be_bytes([
                data[crc_start],
                data[crc_start + 1],
                data[crc_start + 2],
                data[crc_start + 3],
            ]);
            let expected_message_crc = crc32fast::hash(&data[offset..crc_start]);
            if message_crc != expected_message_crc {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "message CRC mismatch: got {:#010x}, expected {:#010x}",
                    message_crc, expected_message_crc
                )));
            }

            // Decode headers (the `:event-type`, `:message-type`, `:content-type`
            // etc.) and the JSON payload. 9router parseEventFrame parity.
            let headers =
                decode_eventstream_headers(&data[offset + Self::PRELUDE_LEN..payload_start])?;
            let payload: Option<Value> = if payload_end > payload_start {
                let raw = &data[payload_start..payload_end];
                let text = std::str::from_utf8(raw).ok();
                match text.map(str::trim) {
                    Some(t) if !t.is_empty() => Some(serde_json::from_str(t).map_err(|e| {
                        KiroExecutorError::EventStreamDecode(format!(
                            "EventStream payload is not valid JSON: {}",
                            e
                        ))
                    })?),
                    _ => None,
                }
            } else {
                None
            };

            events.push(KiroEvent {
                message_type: headers.get(":message-type").cloned().unwrap_or_default(),
                event_type: headers.get(":event-type").cloned().unwrap_or_default(),
                content_type: headers.get(":content-type").cloned().unwrap_or_default(),
                payload,
            });

            offset += total_length;
        }

        Ok(events)
    }
}

/// A decoded AWS EventStream v1 event frame.
#[derive(Debug, Clone)]
pub struct KiroEvent {
    pub message_type: String,
    pub event_type: String,
    pub content_type: String,
    pub payload: Option<Value>,
}

/// Decode the AWS EventStream v1 headers section (9router parseEventFrame
/// header loop). Returns a map of header-name → string value. Binary/other
/// typed headers are stringified; UUID (type 9), blob (6), byte/int (0-4)
/// are preserved as their text/bool/integer representation where meaningful.
fn decode_eventstream_headers(
    data: &[u8],
) -> Result<std::collections::HashMap<String, String>, KiroExecutorError> {
    let mut headers = std::collections::HashMap::new();
    let mut offset = 0usize;
    let header_end = data.len();

    let require_bytes = |offset: usize, count: usize| -> Result<(), KiroExecutorError> {
        if offset + count > header_end {
            return Err(KiroExecutorError::EventStreamDecode(
                "AWS EventStream header exceeds its declared bounds".to_string(),
            ));
        }
        Ok(())
    };

    while offset < header_end {
        require_bytes(offset, 1)?;
        let name_len = data[offset] as usize;
        offset += 1;
        require_bytes(offset, name_len + 1)?;
        let name = std::str::from_utf8(&data[offset..offset + name_len])
            .map_err(|e| {
                KiroExecutorError::EventStreamDecode(format!(
                    "AWS EventStream header name is not UTF-8: {}",
                    e
                ))
            })?
            .to_string();
        offset += name_len;
        if headers.contains_key(&name) {
            return Err(KiroExecutorError::EventStreamDecode(format!(
                "AWS EventStream contains duplicate header: {}",
                name
            )));
        }
        let ty = data[offset];
        offset += 1;

        match ty {
            0 | 1 => {
                headers.insert(
                    name,
                    if ty == 0 {
                        "false".to_string()
                    } else {
                        "true".to_string()
                    },
                );
            }
            2 => {
                require_bytes(offset, 1)?;
                headers.insert(name, data[offset].to_string());
                offset += 1;
            }
            3 => {
                require_bytes(offset, 2)?;
                let v = u16::from_be_bytes([data[offset], data[offset + 1]]) as i16;
                headers.insert(name, v.to_string());
                offset += 2;
            }
            4 => {
                require_bytes(offset, 4)?;
                let v = i32::from_be_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]);
                headers.insert(name, v.to_string());
                offset += 4;
            }
            5 | 8 => {
                // Byte array (5) / long (8) — skip, not semantically needed.
                require_bytes(offset, 8)?;
                offset += 8;
            }
            6 | 7 => {
                // Blob (6) / string (7).
                require_bytes(offset, 2)?;
                let value_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
                offset += 2;
                require_bytes(offset, value_len)?;
                let raw = &data[offset..offset + value_len];
                let value = if ty == 7 {
                    String::from_utf8_lossy(raw).to_string()
                } else {
                    format!("{} bytes", raw.len())
                };
                headers.insert(name, value);
                offset += value_len;
            }
            9 => {
                // UUID.
                require_bytes(offset, 16)?;
                offset += 16;
            }
            other => {
                return Err(KiroExecutorError::EventStreamDecode(format!(
                    "AWS EventStream header {} has unknown type {}",
                    name, other
                )));
            }
        }
    }

    Ok(headers)
}

/// Number of complete EventStream bytes consumed from a buffer, stopping at
/// the first incomplete frame (so the caller can keep the tail for the next
/// chunk). Mirrors `decode_chunk`'s framing logic without decoding payloads.
pub fn consumed_eventstream_bytes(data: &[u8]) -> usize {
    let mut offset = 0usize;
    while offset + 12 <= data.len() {
        let total_length = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if !(16..=MAX_EVENTSTREAM_MESSAGE_LENGTH).contains(&total_length) {
            return offset;
        }
        if offset + total_length > data.len() {
            return offset;
        }
        offset += total_length;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_aws_credentials() {
        let json = r#"{"access_key":"AKIAIOSFODNN7EXAMPLE","secret_key":"secret123","session_token":"token"}"#;
        let creds = KiroExecutor::parse_aws_credentials(json).unwrap();
        assert_eq!(creds.access_key, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(creds.secret_key, "secret123");
        assert_eq!(creds.session_token, Some("token".to_string()));
    }

    #[test]
    fn test_event_stream_decoder_empty() {
        let events = EventStreamDecoder::decode_chunk(&[]).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_event_stream_decoder_parses_headers_and_payload() {
        // Build a minimal AWS EventStream frame:
        //   prelude: total_length=12+headers+4, headers_length, crc(8 bytes)
        //   headers: nameLen ":event-type" ty=7 len valueLen "assistantResponseEvent"
        //   payload: JSON {"content":"hi"}
        let header_bytes = {
            let name = b":event-type";
            let value = b"assistantResponseEvent";
            let mut v = Vec::new();
            v.push(name.len() as u8);
            v.extend_from_slice(name);
            v.push(7u8); // string
            v.extend_from_slice(&(value.len() as u16).to_be_bytes());
            v.extend_from_slice(value);
            v
        };
        let payload = br#"{"content":"hi"}"#;
        let total = 12 + header_bytes.len() + payload.len() + 4;
        let mut frame = Vec::new();
        frame.extend_from_slice(&(total as u32).to_be_bytes());
        frame.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
        let prelude_crc = crc32fast::hash(&frame[..8]);
        frame.extend_from_slice(&prelude_crc.to_be_bytes());
        frame.extend_from_slice(&header_bytes);
        frame.extend_from_slice(payload);
        let msg_crc = crc32fast::hash(&frame);
        frame.extend_from_slice(&msg_crc.to_be_bytes());

        let events = EventStreamDecoder::decode_chunk(&frame).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "assistantResponseEvent");
        assert_eq!(events[0].payload.as_ref().unwrap()["content"], "hi");
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"hello");
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_generate_nonce() {
        let nonce = generate_nonce();
        assert_eq!(nonce.len(), 32);
    }

    #[test]
    fn test_regionalize_host_eu_west_1() {
        assert_eq!(
            regionalize_host(
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
                "eu-west-1"
            ),
            "https://q.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
        assert_eq!(
            regionalize_host(
                "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
                "eu-west-1"
            ),
            "https://codewhisperer.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
    }

    #[test]
    fn test_regionalize_host_noop_for_us_east_1_or_non_aws() {
        // Default region (us-east-1) leaves the host unchanged.
        assert_eq!(
            regionalize_host(
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
                "us-east-1"
            ),
            "https://q.us-east-1.amazonaws.com/generateAssistantResponse"
        );
        // Non-amazonaws host is untouched.
        assert_eq!(
            regionalize_host(
                "https://runtime.us-east-1.kiro.dev/generateAssistantResponse",
                "eu-west-1"
            ),
            "https://runtime.us-east-1.kiro.dev/generateAssistantResponse"
        );
    }

    #[test]
    fn test_build_url_regionalizes_q_host() {
        // 9router getOrderedBaseUrls: api_key surface regionalizes the AWS
        // host to the account's region and orders the q.* surface first.
        let executor = KiroExecutor::new(Arc::new(ClientPool::default()), None).unwrap();
        let mut psd = std::collections::BTreeMap::new();
        psd.insert("authMethod".to_string(), serde_json::json!("api_key"));
        psd.insert("region".to_string(), serde_json::json!("eu-west-1"));
        let credentials = ProviderConnection {
            provider_specific_data: psd,
            api_key: Some("key".to_string()),
            access_token: None,
            ..Default::default()
        };
        let urls = executor.build_url("amazon-nova-pro-v1.0", false, &credentials);
        assert_eq!(
            urls[0],
            "https://q.eu-west-1.amazonaws.com/generateAssistantResponse"
        );
        assert!(urls[0].contains("q.eu-west-1.amazonaws.com"));
        assert!(urls
            .iter()
            .any(|u| u.contains("codewhisperer.eu-west-1.amazonaws.com")));
    }

    #[test]
    fn test_is_ellipsis_only() {
        assert!(is_ellipsis_only("..."));
        assert!(is_ellipsis_only("…"));
        assert!(is_ellipsis_only("  ...  "));
        assert!(!is_ellipsis_only("... and more"));
        assert!(!is_ellipsis_only("complete answer"));
        assert!(!is_ellipsis_only(""));
    }

    #[test]
    fn test_is_short_future_action() {
        // English future-action announcement.
        assert!(is_short_future_action("I'll verify the deployment now"));
        assert!(is_short_future_action("Let me check the logs"));
        assert!(is_short_future_action("Next, I will confirm the checksum"));
        // With a result clause → already completed.
        assert!(!is_short_future_action("I'll verify the status is green"));
        // Too long (over 800 chars) → not short.
        assert!(!is_short_future_action(&"I'll check ".repeat(120)));
        // Completed-language → not a future action.
        assert!(!is_short_future_action("done, verified and confirmed"));
        // Chinese future action.
        assert!(is_short_future_action("接下來我會檢查日誌"));
        assert!(is_short_future_action("我會檢查日誌"));
        assert!(!is_short_future_action("驗證完成，無錯誤"));
    }

    #[test]
    fn test_classify_attempt() {
        // Ellipsis-only content → repair.
        let mut output = KiroAttemptOutput::default();
        output.content = "...".to_string();
        assert_eq!(classify_attempt(&output), KiroRepairKind::Ellipsis);

        // Short future action → repair.
        let mut output2 = KiroAttemptOutput::default();
        output2.content = "I'll check the logs next".to_string();
        assert_eq!(classify_attempt(&output2), KiroRepairKind::ShortFinal);

        // Complete answer → no repair.
        let mut output3 = KiroAttemptOutput::default();
        output3.content = "The checksum matches and the deployment is green.".to_string();
        assert_eq!(classify_attempt(&output3), KiroRepairKind::None);

        // Tool calls → no repair (tools are legitimately terminal).
        let mut output4 = KiroAttemptOutput::default();
        output4.content = "...".to_string();
        output4.has_tool_calls = true;
        assert_eq!(classify_attempt(&output4), KiroRepairKind::None);
    }

    #[test]
    fn test_append_repair_instruction() {
        // JS parity (kiro.js 130-143): the instruction goes into the current
        // user turn, never into a top-level `systemPrompt` (400
        // REQUEST_BODY_INVALID).
        let body = serde_json::json!({
            "conversationState": {
                "currentMessage": { "userInputMessage": { "content": "hi" } }
            }
        });
        let repaired = append_repair_instruction(&body, KiroRepairKind::Ellipsis);
        assert!(repaired.get("systemPrompt").is_none());
        let content = repaired["conversationState"]["currentMessage"]["userInputMessage"]
            ["content"]
            .as_str()
            .unwrap();
        assert!(content.starts_with("hi"));
        assert!(content.contains("ellipsis"));
        // Original body untouched.
        assert_eq!(
            body["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "hi"
        );

        // Empty current content → instruction becomes the whole content.
        let bare = serde_json::json!({
            "conversationState": {
                "currentMessage": { "userInputMessage": { "content": "" } }
            }
        });
        let repaired2 = append_repair_instruction(&bare, KiroRepairKind::InvalidTool);
        assert!(repaired2.get("systemPrompt").is_none());
        assert!(
            repaired2["conversationState"]["currentMessage"]["userInputMessage"]["content"]
                .as_str()
                .unwrap()
                .contains("tool_call")
        );
    }

    fn oauth_credentials() -> ProviderConnection {
        let mut psd = std::collections::BTreeMap::new();
        psd.insert("authMethod".to_string(), serde_json::json!("oauth"));
        ProviderConnection {
            provider_specific_data: psd,
            access_token: Some("tok".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn test_build_bearer_headers_runtime_surface() {
        // JS parity (kiro.js 235-282): x-amz-sso-bearer, agent-mode=spec,
        // machine-id present; X-Amz-Target only on the codewhisperer surface.
        let executor = KiroExecutor::new(Arc::new(ClientPool::default()), None).unwrap();
        let creds = oauth_credentials();
        let cw = executor
            .build_bearer_headers(
                &creds,
                "https://codewhisperer.us-east-1.amazonaws.com/generateAssistantResponse",
            )
            .unwrap();
        assert_eq!(
            cw.get("x-amz-target").unwrap(),
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse"
        );
        assert_eq!(cw.get("x-amz-sso-bearer").unwrap(), "tok");
        assert_eq!(cw.get("x-amzn-kiro-agent-mode").unwrap(), "spec");
        assert_eq!(
            cw.get("x-amzn-codewhisperer-machine-id").unwrap(),
            "kiro-desktop"
        );

        let q = executor
            .build_bearer_headers(
                &creds,
                "https://q.us-east-1.amazonaws.com/generateAssistantResponse",
            )
            .unwrap();
        assert!(q.get("x-amz-target").is_none());
        assert_eq!(q.get("x-amz-sso-bearer").unwrap(), "tok");
        assert_eq!(q.get("x-amzn-kiro-agent-mode").unwrap(), "spec");
    }

    #[test]
    fn test_build_url_oauth_q_first() {
        // JS parity (kiro.js 299-328): kiro.dev must never be first for any
        // auth method — q → codewhisperer → others, incl. OAuth/social.
        let executor = KiroExecutor::new(Arc::new(ClientPool::default()), None).unwrap();
        let urls = executor.build_url("m", false, &oauth_credentials());
        assert!(urls[0].contains("://q."));
        assert!(urls[1].contains("codewhisperer."));
        assert!(urls[2].contains("kiro.dev"));
    }

    #[test]
    fn test_inspect_sse_body() {
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{}]}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let mut output = KiroAttemptOutput::default();
        inspect_sse_body(sse.as_bytes(), &mut output);
        assert_eq!(output.content, "hello");
        assert_eq!(output.reasoning, "think");
        assert!(output.has_tool_calls);
        assert!(!output.saw_error);

        // An error frame marks saw_error.
        let err_sse = "data: {\"error\":{\"message\":\"boom\"}}\n\n";
        let mut out2 = KiroAttemptOutput::default();
        inspect_sse_body(err_sse.as_bytes(), &mut out2);
        assert!(out2.saw_error);
    }

    #[test]
    fn encode_sse_error_emits_kiro_code() {
        let bytes = encode_sse_error("kiro_ellipsis_retry_failed", "repair failed", None);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("kiro_ellipsis_retry_failed"));
        assert!(text.contains("\"type\":\"upstream_error\""));
        assert!(text.contains("repair failed"));
        assert!(text.contains("data: [DONE]"));
    }

    #[test]
    fn stop_disposition_classifies() {
        assert_eq!(stop_disposition(None, false), StopDisposition::Complete);
        assert_eq!(stop_disposition(None, true), StopDisposition::ToolUse);
        assert_eq!(
            stop_disposition(Some("tool_use"), false),
            StopDisposition::ToolUse
        );
        assert_eq!(
            stop_disposition(Some("length"), false),
            StopDisposition::Length
        );
        // Guardrail-flavored stops are terminal refusals (JS line 162).
        assert_eq!(
            stop_disposition(Some("content_filter"), false),
            StopDisposition::TerminalRefusal
        );
        assert_eq!(
            stop_disposition(Some("refusal"), false),
            StopDisposition::TerminalRefusal
        );
        assert_eq!(
            stop_disposition(Some("malformed_function_call"), false),
            StopDisposition::UnknownFailure
        );
        assert_eq!(
            stop_disposition(Some("end_turn"), false),
            StopDisposition::Complete
        );
        assert_eq!(
            stop_disposition(Some("mystery"), false),
            StopDisposition::UnknownFailure
        );
        // Truncation stop reasons (9router v0.5.55).
        assert_eq!(
            stop_disposition(Some("model_context_window_exceeded"), false),
            StopDisposition::TerminalIncomplete
        );
        assert_eq!(
            stop_disposition(Some("cancelled"), false),
            StopDisposition::TerminalIncomplete
        );
        assert_eq!(
            stop_disposition(Some("pause_turn"), false),
            StopDisposition::TerminalIncomplete
        );
        // max_tokens: Length without tool calls, TerminalIncomplete with tool calls.
        assert_eq!(
            stop_disposition(Some("max_tokens"), false),
            StopDisposition::Length
        );
        assert_eq!(
            stop_disposition(Some("max_tokens"), true),
            StopDisposition::TerminalIncomplete
        );
    }

    #[test]
    fn classify_buffered_body_uses_decode_first() {
        // The decode-first fix: a raw kiro binary EventStream body carrying an
        // assistantResponseEvent with content "..." must decode to SSE and
        // classify as Ellipsis. Build a minimal AWS EventStream frame.
        fn make_frame(event_type: &str, payload: &str) -> Vec<u8> {
            let mut header_bytes = Vec::new();
            let name = b":event-type";
            header_bytes.push(name.len() as u8);
            header_bytes.extend_from_slice(name);
            header_bytes.push(7u8); // string
            header_bytes.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
            header_bytes.extend_from_slice(event_type.as_bytes());
            let payload_bytes = payload.as_bytes();
            let total = 12 + header_bytes.len() + payload_bytes.len() + 4;
            let mut frame = Vec::new();
            frame.extend_from_slice(&(total as u32).to_be_bytes());
            frame.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
            let prelude_crc = crc32fast::hash(&frame[..8]);
            frame.extend_from_slice(&prelude_crc.to_be_bytes());
            frame.extend_from_slice(&header_bytes);
            frame.extend_from_slice(payload_bytes);
            let msg_crc = crc32fast::hash(&frame);
            frame.extend_from_slice(&msg_crc.to_be_bytes());
            frame
        }

        // Ellipsis-only content → Ellipsis.
        let ellipsis_body = make_frame("assistantResponseEvent", r#"{"content":"..."}"#);
        let kind = classify_buffered_body(&ellipsis_body);
        assert_eq!(
            kind,
            KiroRepairKind::Ellipsis,
            "binary assistantResponseEvent with content '...' must classify as Ellipsis"
        );

        // A normal completion does not repair.
        let ok_body = make_frame("assistantResponseEvent", r#"{"content":"all done"}"#);
        let kind = classify_buffered_body(&ok_body);
        assert_eq!(kind, KiroRepairKind::None);
    }

    #[test]
    fn normalize_stop_reason_aliases() {
        // 9router normalizeStopReason (kiro.js:145-151).
        assert_eq!(
            normalize_stop_reason(Some("stop")),
            Some("end_turn".to_string())
        );
        assert_eq!(
            normalize_stop_reason(Some("endTurn")),
            Some("end_turn".to_string())
        );
        assert_eq!(
            normalize_stop_reason(Some("tool_calls")),
            Some("tool_use".to_string())
        );
        assert_eq!(
            normalize_stop_reason(Some("length")),
            Some("max_tokens".to_string())
        );
        assert_eq!(
            normalize_stop_reason(Some("maxTokens")),
            Some("max_tokens".to_string())
        );
        assert_eq!(normalize_stop_reason(None), None);
        assert_eq!(normalize_stop_reason(Some("  ")), None);
    }

    #[test]
    fn stop_disposition_js_parity() {
        // malformed output is retryable, not terminal (JS line 160).
        assert_eq!(
            stop_disposition(Some("malformed_model_output"), false),
            StopDisposition::RetryableProtocolFailure
        );
        assert_eq!(
            stop_disposition(Some("invalid_model_output"), false),
            StopDisposition::RetryableProtocolFailure
        );
        // Guardrail-flavored refusals are terminal refusals (JS line 162).
        assert_eq!(
            stop_disposition(Some("guardrail_intervened"), false),
            StopDisposition::TerminalRefusal
        );
        assert_eq!(
            stop_disposition(Some("content-filtered"), false),
            StopDisposition::TerminalRefusal
        );
        // end_turn aliases stay complete.
        assert_eq!(
            stop_disposition(Some("stop_sequence"), false),
            StopDisposition::Complete
        );
        // camelCase input normalizes before classification.
        assert_eq!(
            stop_disposition(Some("endTurn"), false),
            StopDisposition::Complete
        );
        assert_eq!(
            stop_disposition(Some("toolUse"), false),
            StopDisposition::ToolUse
        );
    }

    #[test]
    fn merge_stop_reason_keeps_severe() {
        // 9router mergeStopReason (kiro.js:170-183).
        assert_eq!(
            merge_stop_reason(None, Some("end_turn")),
            Some("end_turn".to_string())
        );
        assert_eq!(
            merge_stop_reason(Some("end_turn"), None),
            Some("end_turn".to_string())
        );
        assert_eq!(merge_stop_reason(None, None), None);
        // Terminal refusal outranks terminal incomplete.
        assert_eq!(
            merge_stop_reason(Some("cancelled"), Some("refusal")),
            Some("refusal".to_string())
        );
        // Lower severity incoming does not replace current.
        assert_eq!(
            merge_stop_reason(Some("refusal"), Some("end_turn")),
            Some("refusal".to_string())
        );
        // Unknown outranks length.
        assert_eq!(
            merge_stop_reason(Some("max_tokens"), Some("mystery")),
            Some("mystery".to_string())
        );
    }

    #[test]
    fn truncation_keeps_streamed_output() {
        // JS derives the disposition with the turn's sawToolUse state, so a
        // truncation reason only keeps output on a tool turn.
        assert!(is_truncated_after_output(
            Some("model_context_window_exceeded"),
            true,
            true
        ));
        assert!(is_truncated_after_output(Some("max_tokens"), true, true));
        // Plain-text max_tokens is Length, not terminal → no truncation branch.
        assert!(!is_truncated_after_output(Some("max_tokens"), false, true));
        // No chunks yet → nothing to keep.
        assert!(!is_truncated_after_output(Some("max_tokens"), true, false));
        // Abandoned turns stay private even with output on a tool turn.
        assert!(!is_truncated_after_output(Some("cancelled"), true, true));
        assert!(!is_truncated_after_output(Some("pause_turn"), true, true));
    }

    #[test]
    fn should_retry_status_endpoint_only() {
        // 9router shouldRetry (kiro.js:338-342): 401/403/404 with fallback.
        assert!(should_retry_status(401, true));
        assert!(should_retry_status(403, true));
        assert!(should_retry_status(404, true));
        // 400 is terminal even with fallback left.
        assert!(!should_retry_status(400, true));
        // No fallback left → no retry.
        assert!(!should_retry_status(401, false));
        assert!(!should_retry_status(429, true));
        assert!(!should_retry_status(500, true));
    }

    #[test]
    fn recovery_action_matches_js_gate() {
        assert_eq!(
            recovery_action(IntegrityAttemptKind::Complete, true),
            RecoveryAction::Complete
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::TerminalStop, true),
            RecoveryAction::FailTerminal
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::UpstreamError, true),
            RecoveryAction::FailTerminal
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::InvalidTool, false),
            RecoveryAction::FailInvalidToolDisabled
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::InvalidTool, true),
            RecoveryAction::Repair(KiroRepairKind::InvalidTool)
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::Ellipsis, true),
            RecoveryAction::Repair(KiroRepairKind::Ellipsis)
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::ShortFinal, true),
            RecoveryAction::Repair(KiroRepairKind::ShortFinal)
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::RetryableStop, true),
            RecoveryAction::RetryUnmodified
        );
        assert_eq!(
            recovery_action(IntegrityAttemptKind::MissingTerminal, true),
            RecoveryAction::RetryUnmodified
        );
    }

    #[test]
    fn integrity_failure_codes() {
        // Buffer cap has its own code regardless of disposition.
        assert_eq!(
            integrity_failure_code(
                IntegrityAttemptKind::TerminalStop,
                Some("integrity_buffer_exceeded"),
                StopDisposition::TerminalIncomplete
            ),
            "kiro_integrity_buffer_exceeded"
        );
        assert_eq!(
            integrity_failure_code(
                IntegrityAttemptKind::UpstreamError,
                None,
                StopDisposition::UnknownFailure
            ),
            "kiro_upstream_eventstream_error"
        );
        assert_eq!(
            integrity_failure_code(
                IntegrityAttemptKind::TerminalStop,
                None,
                StopDisposition::TerminalRefusal
            ),
            "kiro_terminal_refusal"
        );
        assert_eq!(
            integrity_failure_code(
                IntegrityAttemptKind::TerminalStop,
                None,
                StopDisposition::TerminalIncomplete
            ),
            "kiro_terminal_incomplete"
        );
        assert_eq!(
            integrity_failure_code(
                IntegrityAttemptKind::TerminalStop,
                None,
                StopDisposition::UnknownFailure
            ),
            "kiro_unknown_stop_reason"
        );
    }

    #[test]
    fn retry_failure_codes_tail() {
        // 9router runIntegrityRecovery tail (kiro.js:493-499).
        assert_eq!(
            retry_failure_code(IntegrityAttemptKind::Ellipsis),
            "kiro_ellipsis_retry_failed"
        );
        assert_eq!(
            retry_failure_code(IntegrityAttemptKind::ShortFinal),
            "kiro_short_final_retry_failed"
        );
        assert_eq!(
            retry_failure_code(IntegrityAttemptKind::InvalidTool),
            "kiro_tool_call_repair_retry_failed"
        );
        assert_eq!(
            retry_failure_code(IntegrityAttemptKind::MissingTerminal),
            "kiro_missing_terminal_retry_failed"
        );
    }

    #[test]
    fn decode_body_to_sse_transforms_frames() {
        // The EventStream→SSE transform path (JS transformEventStreamToSSE):
        // a binary assistantResponseEvent frame decodes to OpenAI SSE text.
        fn make_frame(event_type: &str, payload: &str) -> Vec<u8> {
            let mut header_bytes = Vec::new();
            let name = b":event-type";
            header_bytes.push(name.len() as u8);
            header_bytes.extend_from_slice(name);
            header_bytes.push(7u8);
            header_bytes.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
            header_bytes.extend_from_slice(event_type.as_bytes());
            let total = 12 + header_bytes.len() + payload.len() + 4;
            let mut frame = Vec::new();
            frame.extend_from_slice(&(total as u32).to_be_bytes());
            frame.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
            let prelude_crc = crc32fast::hash(&frame[..8]);
            frame.extend_from_slice(&prelude_crc.to_be_bytes());
            frame.extend_from_slice(&header_bytes);
            frame.extend_from_slice(payload.as_bytes());
            let msg_crc = crc32fast::hash(&frame);
            frame.extend_from_slice(&msg_crc.to_be_bytes());
            frame
        }

        let body = make_frame("assistantResponseEvent", r#"{"content":"hello"}"#);
        let sse = decode_body_to_sse(&body);
        assert!(
            sse.contains("data: "),
            "expected SSE data lines, got: {sse}"
        );
        assert!(sse.contains("hello"), "expected content in SSE, got: {sse}");
    }
    #[test]
    fn test_normalize_kiro_model_body() {
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0-thinking-agentic"),
            "amazon-nova-pro-v1.0"
        );
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0-thinking"),
            "amazon-nova-pro-v1.0"
        );
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0-agentic"),
            "amazon-nova-pro-v1.0"
        );
        assert_eq!(
            normalize_kiro_model("amazon-nova-pro-v1.0"),
            "amazon-nova-pro-v1.0"
        );
    }
}
