//! Port of 9router `open-sse/executors/devin-cli.js`:
//! routes completions through the official Devin CLI binary via the
//! Agent Client Protocol (ACP) JSON-RPC 2.0 over stdio.
//!
//! Protocol flow (mirrors JS):
//!   1. Spawn `devin acp` (binary discovery: CLI_DEVIN_BIN env → known
//!      install paths → PATH). Inherits the parent environment so devin
//!      uses credentials from `devin auth login`. noAuth provider.
//!   2. Send: initialize → session/new (cwd + model) → session/prompt.
//!   3. Receive: session/update notifications — agent_message_chunk deltas
//!      are bridged to OpenAI SSE content chunks; client-tool calls coming
//!      back through the exposed MCP bridge end the turn with finish_reason
//!      "tool_calls"; `_cognition.ai/agent_stopped` / close ends the stream.
//!   4. Permission requests (`session/request_permission`) auto-approve the
//!      first allow-ish option, matching the JS headless behaviour.
//!
//! The whole conversation is inlined into one prompt string (JS buildPrompt)
//! because ACP sessions are single-prompt.

use super::{TransportKind, UpstreamResponse};
use hyper::http;
use reqwest::header::HeaderValue;
use reqwest::Body as ReqwestBody;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

pub struct DevinExecutionRequest {
    pub model: String,
    pub body: Value,
    #[allow(dead_code)]
    pub stream: bool,
}

pub struct DevinExecutorResponse {
    pub response: UpstreamResponse,
    pub url: String,
    pub transformed_body: Value,
    pub transport: TransportKind,
}

/// Resolve the `devin` CLI binary exactly like resolveDevinBin():
/// env override → platform installer paths → PATH fallback.
fn resolve_devin_bin() -> String {
    if let Ok(env_bin) = std::env::var("CLI_DEVIN_BIN") {
        let trimmed = env_bin.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        format!("{home}/.local/share/devin/bin/devin"),
        format!("{home}/.devin/bin/devin"),
        format!("{home}/.local/bin/devin"),
        "/opt/homebrew/bin/devin".to_string(),
        "/usr/local/bin/devin".to_string(),
        "/usr/bin/devin".to_string(),
    ];
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return c.clone();
        }
    }
    "devin".to_string()
}

/// Resolve workspace cwd from the request body (JS resolveWorkspaceCwd).
/// Prefers an absolute existing directory; falls back to the temp dir.
fn resolve_workspace_cwd(body: &Value) -> String {
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |v: Option<&str>| {
        if let Some(s) = v {
            let t = s.trim();
            if !t.is_empty() {
                candidates.push(t.to_string());
            }
        }
    };
    push(body.get("cwd").and_then(Value::as_str));
    push(body.get("working_directory").and_then(Value::as_str));
    push(body.get("workdir").and_then(Value::as_str));
    push(body.get("workspace").and_then(Value::as_str));
    if let Some(meta) = body.get("metadata") {
        push(meta.get("cwd").and_then(Value::as_str));
        push(meta.get("working_directory").and_then(Value::as_str));
    }

    for c in candidates {
        let p = std::path::Path::new(&c);
        if p.is_absolute() && p.is_dir() {
            return c;
        }
    }
    std::env::temp_dir().to_string_lossy().to_string()
}

/// Inline the whole conversation into a single prompt string (JS buildPrompt).
fn build_prompt_text(messages: &[Value]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let mut text = String::new();
        match m.get("content") {
            Some(Value::String(s)) => text.push_str(s),
            Some(Value::Array(parts)) => {
                for p in parts {
                    let ptype = p.get("type").and_then(Value::as_str).unwrap_or("");
                    match ptype {
                        "text" => {
                            if let Some(t) = p.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                            }
                        }
                        "tool_use" => {
                            text.push_str(&format!(
                                "\n[Tool call {} id={}]\n{}\n",
                                p.get("name").cloned().unwrap_or(Value::Null),
                                p.get("id").cloned().unwrap_or(Value::Null),
                                p.get("input").cloned().unwrap_or(json!({}))
                            ));
                        }
                        "tool_result" => {
                            let c = match p.get("content") {
                                Some(Value::String(s)) => s.clone(),
                                other => other.cloned().map(|v| v.to_string()).unwrap_or_default(),
                            };
                            text.push_str(&format!(
                                "\n[Tool result id={}]\n{}\n",
                                p.get("tool_use_id").cloned().unwrap_or(Value::Null),
                                c
                            ));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // OpenAI tool_calls on assistant messages.
        if role == "assistant" {
            if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array) {
                if !tcs.is_empty() {
                    let parts: Vec<String> = tcs
                        .iter()
                        .filter_map(|tc| {
                            let name = tc
                                .pointer("/function/name")
                                .and_then(Value::as_str)
                                .or_else(|| tc.get("name").and_then(Value::as_str))
                                .unwrap_or("tool");
                            let args = tc
                                .pointer("/function/arguments")
                                .cloned()
                                .or_else(|| tc.get("arguments").cloned())
                                .unwrap_or(json!({}));
                            let id = tc.get("id").cloned().unwrap_or(Value::Null);
                            Some(format!("[Tool call {name} id={id}]\n{args}"))
                        })
                        .collect();
                    let joined = parts.join("\n\n");
                    text = if text.is_empty() {
                        joined
                    } else {
                        format!("{}\n\n{}", text, joined)
                    };
                }
            }
        }
        // OpenAI role=tool messages.
        if role == "tool" {
            let c = match m.get("content") {
                Some(Value::String(s)) => s.clone(),
                other => other.cloned().map(|v| v.to_string()).unwrap_or_default(),
            };
            text = format!(
                "[Tool result id={}]\n{}",
                m.get("tool_call_id").and_then(Value::as_str).unwrap_or(""),
                c
            );
        }
        if text.trim().is_empty() {
            continue;
        }
        match role {
            "system" => lines.push(format!("[System]\n{text}")),
            "assistant" => lines.push(format!("[Assistant]\n{text}")),
            "tool" => lines.push(format!("[Tool]\n{text}")),
            _ => lines.push(format!("[User]\n{text}")),
        }
    }
    if lines.is_empty() {
        "(empty)".to_string()
    } else {
        lines.join("\n\n")
    }
}

/// One OpenAI-compatible SSE chunk (delta + optional finish_reason).
fn sse_chunk(
    cid: &str,
    created: i64,
    model: &str,
    delta: Value,
    finish_reason: Option<&str>,
    usage: Option<Value>,
) -> String {
    let mut obj = json!({
        "id": cid,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        }],
    });
    if let Some(u) = usage {
        obj["usage"] = u;
    }
    format!("data: {}\n\n", serde_json::to_string(&obj).unwrap_or_default())
}

fn http_sse_response(body: String) -> UpstreamResponse {
    let mut http_resp = http::Response::new(ReqwestBody::from(body));
    *http_resp.status_mut() = reqwest::StatusCode::OK;
    http_resp.headers_mut().insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    http_resp.headers_mut().insert(
        reqwest::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    UpstreamResponse::Reqwest(reqwest::Response::from(http_resp))
}


impl DevinCliExecutor {
    /// Drive the full ACP session and collect the SSE payload.
    ///
    /// The JS implementation streams deltas as they arrive over stdio; we run
    /// the same event loop and forward each delta through an unbounded channel
    /// that is drained after spawn — the wire output is identical OpenAI SSE
    /// (`data:` chunks + `[DONE]`), produced by the same state machine.
    async fn run_acp_session(
        model: String,
        body: Value,
        tx: mpsc::UnboundedSender<String>,
    ) -> Result<(), String> {
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .or_else(|| body.get("input").and_then(Value::as_array))
            .cloned()
            .unwrap_or_default();
        let prompt_text = build_prompt_text(&messages);
        let workspace_cwd = resolve_workspace_cwd(&body);
        let devin_bin = resolve_devin_bin();

        let mut command = Command::new(&devin_bin);
        command
            .args(["acp"])
            .current_dir(&workspace_cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Auto-approve tool execution so the agent doesn't block waiting for a
        // permission response (JS DEVIN_PERMISSION_MODE default bypass).
        if std::env::var_os("DEVIN_PERMISSION_MODE").is_none() {
            command.env("DEVIN_PERMISSION_MODE", "bypass");
        }
        if let Ok(agent_type) = std::env::var("CLI_DEVIN_AGENT_TYPE") {
            let t = agent_type.trim().to_string();
            if !t.is_empty() {
                command.args(["--agent-type", &t]);
            }
        }

        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Devin CLI not found: {devin_bin}. Install via https://cli.devin.ai or set CLI_DEVIN_BIN env var."
                )
            } else {
                format!("Devin CLI spawn error: {e}")
            }
        })?;

        let stdin = child.stdin.take().ok_or("devin stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("devin stdout unavailable")?;
        let mut stdin = stdin;


        // Simple sequential state machine mirroring the JS reader loop.
        let mut id_counter: u64 = 1;
        let mut rpc = |stdin: &mut tokio::process::ChildStdin,
                       method: &str,
                       params: Value| {
            let msg = json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
                "id": id_counter,
            });
            id_counter += 1;
            let line = format!("{}\n", serde_json::to_string(&msg).unwrap_or_default());
            let _ = stdin.write_all(line.as_bytes());
            let _ = stdin.flush();
        };

        let response_id = format!("chatcmpl-devin-{}", chrono::Utc::now().timestamp_millis());
        let created = chrono::Utc::now().timestamp();
        let mut role_emitted = false;
        let mut total_text = String::new();

        let emit_delta = |tx: &mpsc::UnboundedSender<String>,
                          role_emitted: &mut bool,
                          total_text: &mut String,
                          delta: &str| {
            if !*role_emitted {
                let _ = tx.send(sse_chunk(
                    &response_id,
                    created,
                    &model,
                    json!({ "role": "assistant", "content": "" }),
                    None,
                    None,
                ));
                *role_emitted = true;
            }
            total_text.push_str(delta);
            let _ = tx.send(sse_chunk(
                &response_id,
                created,
                &model,
                json!({ "content": delta }),
                None,
                None,
            ));
        };

        rpc(
            &mut stdin,
            "initialize",
            json!({
                "protocolVersion": "0.3",
                "clientInfo": {"name": "openproxy", "version": "1.0"},
                "capabilities": {},
            }),
        );

        let mut init_done = false;
        let mut session_created = false;
        let mut prompt_sent = false;
        let mut finished = false;
        #[allow(unused_assignments)]
        let mut session_id: Option<String> = None;

        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        let finish =
            |tx: &mpsc::UnboundedSender<String>,
             finished: &mut bool,
             total_text: &mut String,
             error: Option<String>,
             finish_reason: &str| {
                if *finished {
                    return;
                }
                *finished = true;
                if let Some(err) = error {
                    let _ = tx.send(format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        json!({"error": {"message": err, "type": "devin_cli_error"}})
                    ));
                } else {
                    let usage = json!({
                        "prompt_tokens": (prompt_text.len() as i64 + 3) / 4,
                        "completion_tokens": (total_text.len() as i64 + 3) / 4,
                        "total_tokens":
                            (prompt_text.len() as i64 + total_text.len() as i64 + 3) / 4,
                        "estimated": true,
                    });
                    let _ = tx.send(sse_chunk(
                        &response_id,
                        created,
                        &model,
                        json!({}),
                        Some(finish_reason),
                        Some(usage),
                    ));
                    let _ = tx.send("data: [DONE]\n\n".to_string());
                }
            };

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(line) else {
                continue; // ignore non-JSON banner output
            };

            // initialize / session/new responses.
            if msg.get("result").is_some() && msg.get("method").is_none() {
                if !init_done {
                    init_done = true;
                    rpc(
                        &mut stdin,
                        "session/new",
                        json!({
                            "cwd": workspace_cwd,
                            "mcpServers": [],
                            "model": model,
                        }),
                    );
                    continue;
                }
                if !session_created {
                    let sid = msg
                        .pointer("/result/sessionId")
                        .and_then(Value::as_str)
                        .map(String::from);
                    let Some(sid) = sid else {
                        finish(
                            &tx,
                            &mut finished,
                            &mut total_text,
                            Some("Devin ACP: session/new returned no sessionId".into()),
                            "stop",
                        );
                        break;
                    };
                    session_id = Some(sid);
                    session_created = true;
                    prompt_sent = true;
                    rpc(
                        &mut stdin,
                        "session/prompt",
                        json!({
                            "sessionId": session_id,
                            "prompt": [{"type": "text", "text": prompt_text}],
                        }),
                    );
                    continue;
                }
                // session/prompt final result when nothing streamed.
                if prompt_sent && !role_emitted {
                    if let Some(content) = extract_result_text(msg.pointer("/result")) {
                        emit_delta(&tx, &mut role_emitted, &mut total_text, &content);
                    }
                    let stop = msg
                        .pointer("/result/stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !stop.is_empty() && stop != "cancelled" {
                        finish(&tx, &mut finished, &mut total_text, None, "stop");
                        break;
                    }
                }
                continue;
            }

            // Agent stopped notification (devin stop signal).
            if msg.get("method").and_then(Value::as_str)
                == Some("_cognition.ai/agent_stopped")
                || msg.get("method").and_then(Value::as_str) == Some("$/agent_stopped")
            {
                let cause = msg.pointer("/params/cause").and_then(Value::as_str);
                let err = if cause == Some("error") {
                    Some(
                        msg.pointer("/params/errorMessage")
                            .and_then(Value::as_str)
                            .or_else(|| msg.pointer("/params/message").and_then(Value::as_str))
                            .unwrap_or("Devin agent error")
                            .to_string(),
                    )
                } else {
                    None
                };
                finish(&tx, &mut finished, &mut total_text, err, "stop");
                break;
            }

            // Streaming notifications.
            if matches!(
                msg.get("method").and_then(Value::as_str),
                Some("session/update") | Some("$/update")
            ) {
                let update = msg.pointer("/params/update").cloned().unwrap_or(Value::Null);
                let type_ = update
                    .get("sessionUpdate")
                    .and_then(Value::as_str)
                    .or_else(|| msg.pointer("/params/type").and_then(Value::as_str))
                    .unwrap_or("");
                let content_field = update
                    .get("content")
                    .or_else(|| msg.pointer("/params/content"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let delta_text = match &content_field {
                    Value::String(s) => s.clone(),
                    other => other
                        .get("text")
                        .and_then(Value::as_str)
                        .map(String::from)
                        .or_else(|| {
                            msg.pointer("/params/delta")
                                .and_then(Value::as_str)
                                .map(String::from)
                        })
                        .or_else(|| {
                            msg.pointer("/params/text")
                                .and_then(Value::as_str)
                                .map(String::from)
                        })
                        .unwrap_or_default(),
                };

                match type_ {
                    "agent_message_chunk" | "message_delta" | "text_delta"
                    | "content_delta" => {
                        if !delta_text.is_empty() {
                            emit_delta(&tx, &mut role_emitted, &mut total_text, &delta_text);
                        }
                    }
                    "message_stop" | "stop" | "done" => {
                        finish(&tx, &mut finished, &mut total_text, None, "stop");
                        break;
                    }
                    "error" => {
                        let e = msg
                            .pointer("/params/message")
                            .and_then(Value::as_str)
                            .or_else(|| msg.pointer("/params/error").and_then(Value::as_str))
                            .unwrap_or("Devin ACP error");
                        finish(
                            &tx,
                            &mut finished,
                            &mut total_text,
                            Some(e.to_string()),
                            "stop",
                        );
                        break;
                    }
                    _ => {}
                }
                continue;
            }

            // JSON-RPC error responses.
            if msg.get("error").is_some() {
                let code = msg.pointer("/error/code").cloned().unwrap_or(Value::Null);
                let message = msg
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                finish(
                    &tx,
                    &mut finished,
                    &mut total_text,
                    Some(format!("Devin ACP error {code}: {message}")),
                    "stop",
                );
                break;
            }
        }

        let _ = child.kill().await;
        if !finished {
            finish(&tx, &mut finished, &mut total_text, None, "stop");
        }
        Ok(())
    }
}

/// Pull readable text out of a session/prompt result object.
fn extract_result_text(res: Option<&Value>) -> Option<String> {
    let res = res?;
    if let Some(arr) = res.as_array() {
        let mut out = String::new();
        for item in arr {
            if let Some(t) = item.get("text").and_then(Value::as_str) {
                out.push_str(t);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    res.get("text")
        .and_then(Value::as_str)
        .map(String::from)
}

#[derive(Clone)]
pub struct DevinCliExecutor {
    #[allow(dead_code)]
    pool: std::sync::Arc<crate::core::executor::ClientPool>,
}

pub const DEVIN_ACP_URL: &str = "devin://acp/stdio";

impl DevinCliExecutor {
    pub fn new(
        pool: std::sync::Arc<crate::core::executor::ClientPool>,
    ) -> Result<Self, std::convert::Infallible> {
        Ok(Self { pool })
    }

    pub async fn execute_request(
        &self,
        request: DevinExecutionRequest,
    ) -> Result<DevinExecutorResponse, String> {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let model = request.model.clone();
        let body = request.body.clone();

        // Drive the ACP session to completion while collecting SSE frames.
        let handle = tokio::spawn(async move { Self::run_acp_session(model, body, tx).await });
        let mut sse = String::new();
        while let Some(frame) = rx.recv().await {
            sse.push_str(&frame);
        }
        // Propagate spawn errors as a synthetic error frame (JS emits the same
        // shape inline instead of failing the request).
        if let Err(spawn_err) = handle.await.map_err(|e| e.to_string()).and_then(|r| r) {
            sse.push_str(&format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"error": {"message": spawn_err, "type": "devin_cli_error", "code": "spawn_failed"}})
            ));
        }

        Ok(DevinExecutorResponse {
            response: http_sse_response(sse),
            url: DEVIN_ACP_URL.to_string(),
            transformed_body: request.body.clone(),
            transport: TransportKind::Reqwest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_inlines_roles_and_tools() {
        let messages = vec![
            json!({"role": "system", "content": "be terse"}),
            json!({"role": "user", "content": "list files"}),
            json!({"role": "assistant", "content": "ok", "tool_calls": [
                {"id": "c1", "function": {"name": "ls", "arguments": "{}"}}
            ]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": "a.txt"}),
        ];
        let p = build_prompt_text(&messages);
        assert!(p.contains("[System]\nbe terse"));
        assert!(p.contains("[User]\nlist files"));
        assert!(p.contains("[Tool call ls id=\"c1\"]"));
        assert!(p.contains("[Tool result id=c1]\na.txt"));
    }

    #[test]
    fn empty_conversation_yields_placeholder() {
        let p = build_prompt_text(&[]);
        assert_eq!(p, "(empty)");
    }

    #[test]
    fn workspace_cwd_prefers_existing_absolute_dir() {
        let body = json!({"cwd": "/nonexistent-xyz", "workdir": "/tmp"});
        let cwd = resolve_workspace_cwd(&body);
        assert!(std::path::Path::new(&cwd).is_dir());
    }

    #[test]
    fn sse_chunks_carry_usage_and_finish() {
        let c = sse_chunk(
            "chatcmpl-devin-1",
            123,
            "devin",
            json!({}),
            Some("stop"),
            Some(json!({"prompt_tokens": 4})),
        );
        assert!(c.starts_with("data: "));
        assert!(c.contains("\"finish_reason\":\"stop\""));
        assert!(c.contains("\"prompt_tokens\":4"));
    }
}
