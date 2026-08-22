# 9router → OpenProxy logic parity

Evidence-based parity against 9router v0.5.55 (`decolua/9router` @ 699edac). Beads:
`openproxy-9router-parity-mj1*`, epic `openproxy-9router-parity-v0550-pnc`.

## 2026-08-22 deep-audit pass (8-agent swarm vs v0.5.55)

Fixed (commits e839d283, 9ef44223):

| Gap | Fix |
|-----|-----|
| xAI client_id duplicated `073a-` segment (4 files) — every xai/grok-cli OAuth broken | Corrected UUID + regression test |
| Fusion judge buffered JSON for `stream:true` clients | `handle_fusion_chat_deferred` envelope; chat.rs dispatches final leg with original stream flag |
| Fusion panels leaked `stream_options` (#3024) | stripped in `flatten_tool_history` |
| RR account rotation ignored stickyRoundRobinLimit | `select_with_sticky_limit` + per-provider override |
| FillFirst picked max-quota account | priority-first, quota tiebreak only |
| devin-cli executor missing entirely | new `devin_cli.rs` ACP stdio port + dispatch |
| iflow base URL pointed at mintlify docs site | `apis.iflow.cn/v1/chat/completions` |
| grok-web returned raw NDJSON, no error mapping | NDJSON→SSE/JSON converter + 401/403/429 messages |
| codebuddy-intl missing from forceStream list | added (+`cbai`) |
| codex `_compact` body flag ignored | routes to `/compact`, flag stripped pre-send |
| codex SSE retry exponential vs JS flat 2s | flat 2000ms |
| cursor composer `</thinking>` terminator | `</think>` via lastIndexOf + anchored composer match |
| vertex non-stream still called `:streamGenerateContent` | verb follows stream flag |
| DefaultExecutor dropped upstream error bodies | capped body text into `UpstreamStatus` |
| embeddings handler no 401/403 refresh-once | ported from embeddingsCore.js |
| video creation re-fired at next account on network error | immediate 502 (never re-bill) |
| usage tracker DELETE-all + rewrite per request | incremental row append |
| kimi device_id regenerated every boot | persisted psd.deviceId; headers take hostname + stable id |
| `/api/oauth/:p/refresh` generic form grant | routed through per-provider `dispatch_oauth_refresh` |
| MITM kiro detection missed IDE ≥1.0.228 header form | `is_chat_request()` checks `x-amz-target` |
| sync snapshot pinned v0.5.45 | regenerated against v0.5.55 (98 providers / 991 models) |
| search registry caps/timeouts partial | all 9 providers match JS (brave 20, linkup 50, google-pse 10, tavily 20…) |
| adaptive thinking sent only output_config.effort | sends `thinking:{type:adaptive}` too (Anthropic requires both) |
| settings PATCH rejected nested providerStrategies | `ProviderStrategyEntry` string-or-object |

Open beads track the remaining P1/P2 items (windsurf/trae/zed executors .35/.36/.102,
MITM intercept pipeline, background refresh scheduler, chat-search failover breadth,
cloudflare multipart, antigravity image adapter, TTS binary audio).

## How to test

```bash
./scripts/parity-smoke.sh
# or:
cargo test -p openproxy --lib stream_flags
cargo test -p openproxy --lib parity_tests
cargo test -p openproxy --lib claude_format
cargo test -p openproxy --lib combo
cargo test -p openproxy --lib chat::
```

Decision logs: `target: openproxy::chat|translator|combo|fusion|github`.

## Intentional Rust differences (do not “fix”)

| Behavior | Why |
|----------|-----|
| SSRF checks on image prefetch | Security |
| Fail-loud missing credentials | Avoid `Bearer undefined` |
| Refresh dedup does not cache null failures | 9router bug |
| Combo quarantine + RR capacity pre-skip | Reliability (CLI hang) |
| Encrypted SQLite secrets | Security |
| **PXPIPE token-saver** | Optional JS image-context compressor; requires external `pxpipe-proxy`. Not ported — use RTK + Headroom + Caveman/Ponytail. |
| **Hedging / Shadow / Auto-combo** | Modules scaffolded under `src/core/combo/{hedging,shadow,auto_combo}.rs`; chat dispatcher maps unknown names to **fallback** until product demand. |
| Combo capacity precheck | OpenProxy skips saturated members; optional future gate `capacity_precheck=false` for 9router try-anyway. |

## Key pipeline (current)

1. Detect format (endpoint body-aware + body heuristics)
2. Resolve **targetFormat**: `model catalog target_format` → **`resolve_transport(provider, source)`** → `get_target_format_for_provider` (incl. `anthropic-compatible-*` → Claude)
3. **upstreamModelId** + **stripList** from catalog; multi-endpoint `transport_base_url` → `runtime_transport`
4. Stream plan: forceStream / DeepSeek-TUI / Accept / imageGen → `stream` + `sse_to_json`
5. providerThinking on **source** body
6. stripList + modality strip; Claude → `normalize_claude_passthrough` when passthrough
7. Else translate: **direct route** or OpenAI pivot; prepare_claude / filter_openai
8. RTK → Headroom → Caveman/Ponytail → tool dedupe → TTS tool strip
9. Executor (specialized or Default)
10. 401/403 refresh with merge (expires_at); 429 → next fallback URL
11. forceStream SSE→JSON or stream/non-stream proxy; **non-SSE content-type guard** on stream path
12. Selective model-lock clear on success (not clear-all)

## Specialized executors (chat dispatch)

kiro, vertex, codex, cursor/cu, github, azure, qwen, iflow, gemini-cli, opencode, opencode-go, qoder, commandcode, antigravity, grok-web, perplexity-web, kimchi, **codebuddy-cn/cbcn**, **ollama/ollama-local**, **mimo-free/mmf**, else DefaultExecutor.

### Critical executor parity notes

| Executor | 9router behavior | OpenProxy |
|----------|------------------|-----------|
| GitHub | Codex/o-series → `/responses`; escalate on 400 | `github.rs` prefer + escalate |
| Cursor | `api2.cursor.sh` + `forceAgentMode` for Claude Code UA | `cursor.rs` |
| Codex | Always stream upstream; effort suffix strip | `codex.rs` force stream |
| Default | Dual-auth anthropic-compatible; 429 next URL; header cache | `default.rs` + `claude_header_cache` |
| Fusion | Quorum + independent grace timer via `select!` | `fusion.rs` |

## OAuth / refresh

- `should_refresh_credentials` + Codex **8d** max refresh age (`token_refresh.rs`)
- Kiro external_idp / Vertex service-account mint: **explicit unsupported** for SA JWT mint path — use standard OAuth / API key connections; document if connection fails with “unsupported refresh”.

## Settings: comboStrategies

Accepts legacy string **or** nested 9router object:

```json
{
  "comboStrategies": {
    "my-combo": "round-robin",
    "fuse-me": {
      "fallbackStrategy": "fusion",
      "judgeModel": "gpt-4o-mini",
      "fusionTuning": { "minPanel": 2, "stragglerGraceMs": 8000 }
    }
  }
}
```

## Multi-endpoint transports

`resolve_transport` static table (deepseek, kimi, kimi-coding, glm, minimax, minimax-cn): match client `source_format` → set plan target + full endpoint URL on `runtime_transport`.

**Not yet in table (open beads):** `xiaomi-mimo` / `mimo`, `xiaomi-tokenplan` Claude leg (OpenAI+region already in DefaultExecutor).

## Specialized executor gaps (post-close deep scan)

| Provider | Status | Bead |
|----------|--------|------|
| **grok-cli** (`gcli`/`gb`) | OAuth refresh + forceStream + format=responses exist; **no** specialized executor / chat branch → falls through Default/xai (wrong host: needs `cli-chat-proxy.grok.com/v1/responses`) | `openproxy-executor-grok-cli-gzt` P0 |
| **xiaomi-tokenplan** | Region OpenAI URL in DefaultExecutor; **missing** Claude `/anthropic/v1/messages` + resolve_transport | `openproxy-executor-xiaomi-tokenplan-2mz` P1 |
| **xiaomi-mimo** | OpenAI default only; dual transport not in `resolve_transport` | `openproxy-transport-xiaomi-mimo-lyp` P1 |
| perplexity-web | Implemented (lives in `grok_web.rs`, dispatched) | — |
| ollama-local | Wired as ollama | — |

## Other residual parity nits

| Item | Severity | Bead |
|------|----------|------|
| `web_fetch` still clear-**all** `modelLock_*` on success (chat is selective) | P1 | `openproxy-webfetch-selective-lock-1rb` |
| Global `model(high)` stripThinkingSuffix (only kiro/codex partial) | P2 | `openproxy-thinking-suffix-global-zya` |
| Vertex SA JWT mint | OK in `vertex.rs` executor (not OAuth dispatch) — design split, not a gap | — |
| PXPIPE | Intentional skip | — |
| Hedging / shadow / auto-combo chat wire | Intentional scaffold-only | — |

## Remaining intentional backlog

- **Fixed (this pass):** grok-cli specialized executor (`cli-chat-proxy`); xiaomi-tokenplan Claude dual path; xiaomi-mimo in `resolve_transport`; web_fetch selective lock; global `model(level)` strip via `thinking_suffix`.
- **P3 product-optional:** full PXPIPE port; wire hedging/shadow/auto-combo into chat dispatcher when needed.
