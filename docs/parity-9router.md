# 9router → OpenProxy logic parity

Evidence-based parity against 9router v0.5.30 (`open-sse`). Beads: `openproxy-9router-parity-mj1*`.

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

## Remaining intentional backlog

None for P0/P1 parity beads. P3: full PXPIPE port; wire hedging/shadow/auto-combo into chat dispatcher when needed.
