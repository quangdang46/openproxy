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
    format!(
        "data: {}\n\n",
        serde_json::to_string(&obj).unwrap_or_default()
    )
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

        // Client-tools → MCP bridge (JS buildClientToolsMcp): expose
        // body.tools as a stdio MCP server so devin can invoke client tools.
        // DEVIN_MCP_SERVERS provides extra user-configured servers; the
        // clientTools entry is merged in. (Phase 2 bridging of tool_call
        // events back to OpenAI tool_use is handled in the update loop.)
        let client_tools: Vec<Value> = body
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let client_tool_results = extract_client_tool_results(&messages);
        let has_client_tools = build_client_tools_mcp(&client_tools).is_some();
        let mut mcp_config_dir: Option<String> = None;
        if has_client_tools {
            // Throwaway XDG_CONFIG_HOME holds devin/config.json so the agent
            // auto-connects the clientTools server (session/new mcpServers
            // alone doesn't spawn them). Replaces global config for the
            // subprocess; cleaned up on finish.
            let dir = std::env::temp_dir().join(format!("devin-mcp-{}", std::process::id()));
            let cfg_dev = dir.join("devin");
            if std::fs::create_dir_all(&cfg_dev).is_ok() {
                let mut servers = serde_json::Map::new();
                if let Ok(json) = std::env::var("DEVIN_MCP_SERVERS") {
                    if let Ok(Value::Object(extra)) = serde_json::from_str::<Value>(json.trim()) {
                        servers.extend(extra);
                    }
                }
                // clientTools entry: devin spawns the MCP server described
                // here. The JS bridge uses a node stdio script; the Rust port
                // records the tool declarations + seeded results so the agent
                // discovers them, and tool_call events are bridged below.
                servers.insert(
                    "clientTools".to_string(),
                    json!({
                        "tools": build_client_tools_mcp(&client_tools).unwrap_or(Value::Null),
                        "results": Value::Object(client_tool_results.clone().into_iter().collect()),
                    }),
                );
                if std::fs::write(
                    cfg_dev.join("config.json"),
                    serde_json::to_string(&json!({"mcpServers": servers})).unwrap_or_default(),
                )
                .is_ok()
                {
                    mcp_config_dir = Some(dir.to_string_lossy().to_string());
                }
            }
        }

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
        if let Some(ref dir) = mcp_config_dir {
            command.env("XDG_CONFIG_HOME", dir);
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
        let mut rpc =
            async |stdin: &mut tokio::process::ChildStdin, method: &str, params: Value| {
                let msg = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params,
                    "id": id_counter,
                });
                id_counter += 1;
                let line = format!("{}\n", serde_json::to_string(&msg).unwrap_or_default());
                let _ = stdin.write_all(line.as_bytes()).await;
                let _ = stdin.flush().await;
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
        )
        .await;

        let mut init_done = false;
        let mut session_created = false;
        let mut prompt_sent = false;
        let mut finished = false;
        // Client-tool bridge state (JS Phase 2): toolCallId → original name.
        let mut tool_use_emitted = false;
        let mut pending_client_tools: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        #[allow(unused_assignments)]
        let mut session_id: Option<String> = None;

        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        let finish = |tx: &mpsc::UnboundedSender<String>,
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
                    )
                    .await;
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
                    )
                    .await;
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
            if msg.get("method").and_then(Value::as_str) == Some("_cognition.ai/agent_stopped")
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
                let update = msg
                    .pointer("/params/update")
                    .cloned()
                    .unwrap_or(Value::Null);
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
                    "agent_message_chunk" | "message_delta" | "text_delta" | "content_delta" => {
                        if !delta_text.is_empty() {
                            emit_delta(&tx, &mut role_emitted, &mut total_text, &delta_text);
                        }
                    }
                    // Client-tool bridge (JS Phase 2): devin calling a tool
                    // from our exposed MCP ("Calling mcp_<name> from
                    // clientTools") is bridged to an OpenAI tool_use and the
                    // turn ends with finish_reason "tool_calls". tool_call is
                    // upsert-by-id: title may arrive on an earlier event than
                    // rawInput, so track pending ids across notifications.
                    "tool_call" | "tool_call_update" => {
                        if has_client_tools && !tool_use_emitted {
                            let tool_call_id = update
                                .get("toolCallId")
                                .and_then(Value::as_str)
                                .map(String::from);
                            if let Some(title) = update.get("title").and_then(Value::as_str) {
                                if let Some(mcp_name) = parse_client_tool_title(title) {
                                    if let Some(id) = &tool_call_id {
                                        pending_client_tools
                                            .insert(id.clone(), from_mcp_tool_name(&mcp_name));
                                    }
                                }
                            }
                            let resolved = tool_call_id
                                .as_ref()
                                .and_then(|id| pending_client_tools.get(id))
                                .cloned();
                            if let Some(orig_name) = resolved {
                                if let Some(raw) = update.get("rawInput") {
                                    let args_str = match raw {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    };
                                    let call_id = tool_call_id.clone().unwrap_or_else(|| {
                                        format!("call_{}", chrono::Utc::now().timestamp_millis())
                                    });
                                    pending_client_tools.remove(&call_id);
                                    emit_tool_use(
                                        &tx,
                                        &mut role_emitted,
                                        &response_id,
                                        created,
                                        &model,
                                        &orig_name,
                                        &args_str,
                                        &call_id,
                                    );
                                    tool_use_emitted = true;
                                    finish(&tx, &mut finished, &mut total_text, None, "tool_calls");
                                    break;
                                }
                            }
                            continue;
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

/// Emit an OpenAI tool_call delta (JS emitToolUse): role frame first, then
/// the function-call delta. Ends the turn with finish_reason "tool_calls".
#[allow(clippy::too_many_arguments)]
fn emit_tool_use(
    tx: &mpsc::UnboundedSender<String>,
    role_emitted: &mut bool,
    response_id: &str,
    created: i64,
    model: &str,
    tool_name: &str,
    args_str: &str,
    tool_call_id: &str,
) {
    if !*role_emitted {
        let _ = tx.send(sse_chunk(
            response_id,
            created,
            model,
            json!({ "role": "assistant", "content": Value::Null }),
            None,
            None,
        ));
        *role_emitted = true;
    }
    let _ = tx.send(sse_chunk(
        response_id,
        created,
        model,
        json!({
            "tool_calls": [{
                "index": 0,
                "id": tool_call_id,
                "type": "function",
                "function": {"name": tool_name, "arguments": args_str},
            }],
        }),
        None,
        None,
    ));
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
    res.get("text").and_then(Value::as_str).map(String::from)
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

/// MCP tool-name prefix. devin only discovers MCP tools whose name carries
/// the `mcp_` prefix (JS MCP_TOOL_PREFIX); stripped back when bridging.
const MCP_TOOL_PREFIX: &str = "mcp_";

fn to_mcp_tool_name(name: &str) -> String {
    if name.starts_with(MCP_TOOL_PREFIX) {
        name.to_string()
    } else {
        format!("{MCP_TOOL_PREFIX}{name}")
    }
}

fn from_mcp_tool_name(name: &str) -> String {
    name.strip_prefix(MCP_TOOL_PREFIX)
        .unwrap_or(name)
        .to_string()
}

/// Map OpenAI tools to MCP tool declarations for the clientTools bridge.
/// Returns `None` when there are no usable tools.
fn build_client_tools_mcp(tools: &[Value]) -> Option<Value> {
    let mut mcp_tools = Vec::new();
    for t in tools {
        let f = t.get("function").unwrap_or(t);
        let Some(name) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        mcp_tools.push(json!({
            "name": to_mcp_tool_name(name),
            "description": f.get("description").and_then(Value::as_str).unwrap_or(""),
            "inputSchema": f.get("parameters")
                .or_else(|| f.get("input_schema"))
                .cloned()
                .unwrap_or(json!({"type": "object", "properties": {}})),
        }));
    }
    if mcp_tools.is_empty() {
        return None;
    }
    Some(json!(mcp_tools))
}

/// Extract tool_result content keyed by MCP tool name (`mcp_<original>`).
/// Walks messages: assistant.tool_calls id→name, role=tool tool_call_id→content,
/// plus Claude-style tool_use/tool_result blocks.
fn extract_client_tool_results(messages: &[Value]) -> serde_json::Map<String, Value> {
    let mut id_to_mcp: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut results = serde_json::Map::new();
    let content_text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    for m in messages {
        // OpenAI assistant.tool_calls.
        if m.get("role").and_then(Value::as_str) == Some("assistant") {
            if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    let name = tc
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .or_else(|| tc.get("name").and_then(Value::as_str));
                    if let (Some(id), Some(name)) = (tc.get("id").and_then(Value::as_str), name) {
                        id_to_mcp.insert(id.to_string(), to_mcp_tool_name(name));
                    }
                }
            }
        }
        // Claude-style tool_use blocks in content.
        if let Some(arr) = m.get("content").and_then(Value::as_array) {
            for b in arr {
                if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                    if let (Some(id), Some(name)) = (
                        b.get("id").and_then(Value::as_str),
                        b.get("name").and_then(Value::as_str),
                    ) {
                        id_to_mcp.insert(id.to_string(), to_mcp_tool_name(name));
                    }
                }
            }
        }
        // OpenAI role=tool results.
        if m.get("role").and_then(Value::as_str) == Some("tool") {
            if let Some(call_id) = m.get("tool_call_id").and_then(Value::as_str) {
                if let Some(mcp_name) = id_to_mcp.get(call_id) {
                    results.insert(
                        mcp_name.clone(),
                        Value::String(content_text(m.get("content").unwrap_or(&Value::Null))),
                    );
                }
            }
        }
        // Claude-style tool_result blocks in user content.
        if m.get("role").and_then(Value::as_str) == Some("user") {
            if let Some(arr) = m.get("content").and_then(Value::as_array) {
                for b in arr {
                    if b.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let key = b
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| id_to_mcp.get(id))
                        .cloned();
                    if let Some(k) = key {
                        let c = b.get("content").unwrap_or(&Value::Null);
                        results.insert(k, Value::String(content_text(c)));
                    }
                }
            }
        }
    }
    results
}

/// Parse a "Calling mcp_<name> from clientTools" title into the MCP tool name.
fn parse_client_tool_title(title: &str) -> Option<String> {
    let rest = title.strip_prefix("Calling ")?;
    let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let name = &rest[..end];
    if !name.starts_with(MCP_TOOL_PREFIX) {
        return None;
    }
    if !title[end.min(title.len() - "Calling ".len())..].contains("from clientTools") {
        return None;
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_name_prefix_roundtrip() {
        assert_eq!(to_mcp_tool_name("bash"), "mcp_bash");
        assert_eq!(to_mcp_tool_name("mcp_bash"), "mcp_bash");
        assert_eq!(from_mcp_tool_name("mcp_bash"), "bash");
        assert_eq!(from_mcp_tool_name("bash"), "bash");
    }

    #[test]
    fn client_tools_mcp_maps_openai_schemas() {
        let tools = vec![
            serde_json::json!({"type": "function", "function": {"name": "ls", "description": "list", "parameters": {"type": "object"}}}),
            serde_json::json!({"type": "function", "function": {}}),
        ];
        let mcp = build_client_tools_mcp(&tools).expect("one usable tool");
        assert_eq!(mcp[0]["name"], "mcp_ls");
        assert_eq!(mcp[0]["inputSchema"]["type"], "object");
        assert!(build_client_tools_mcp(&[]).is_none());
    }

    #[test]
    fn client_tool_results_map_by_call_id() {
        let messages = vec![
            serde_json::json!({"role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "ls", "arguments": "{}"}}]}),
            serde_json::json!({"role": "tool", "tool_call_id": "c1", "content": "a.txt"}),
        ];
        let results = extract_client_tool_results(&messages);
        assert_eq!(results.get("mcp_ls").and_then(Value::as_str), Some("a.txt"));
    }

    #[test]
    fn client_tool_title_parses_calling_shape() {
        assert_eq!(
            parse_client_tool_title("Calling mcp_ls from clientTools"),
            Some("mcp_ls".to_string())
        );
        assert!(parse_client_tool_title("plain text").is_none());
        assert!(parse_client_tool_title("Calling ls from clientTools").is_none());
    }

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
