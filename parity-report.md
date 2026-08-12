# Parity Report: openproxy (Rust) → 9router v0.5.50 (Node.js)

> **Date:** 2026-08-12 · **Reference:** `.tmp/9router` = `decolua/9router` v0.5.50 (2026-08-05)
> **Port:** `openproxy` v0.2.0 (Rust) — claims "9router v0.5.30 full parity" (2026-07-10)
> **Gap window:** v0.5.30 → v0.5.50 (4 releases: .35, .40, .45, .50)

> 📄 **Full implementation-ready report (122 specs):** see **[`docs/parity-9router-FULL.md`](docs/parity-9router-FULL.md)** — every spec has verbatim JS · current Rust · implementation steps · guard test · risks · cross-check.
> 📄 Condensed per-dimension guide: **[`docs/parity-9router-impl.md`](docs/parity-9router-impl.md)**

---

## Audit Summary

- **151 findings** from 9 audit dimensions (160 subagents, ~5,498 tool calls)
- **146 CONFIRMED** (adversarially verified — read back both JS and Rust at the exact lines)
- **5 REFUTED** (Rust already covered it, or the claim was wrong)
- → **122 implementation specs** generated; **94 CONFIRMED + 24 PLAUSIBLE + 4 REFUTED** across adversarial cross-checks

| Priority | Specs | Meaning |
|---|---|---|
| **P0** | 78 | Broken requests: 17 providers, wrong URLs, missing OAuth, stub executors |
| **P1** | 39 | Behavior divergence: translator/oauth/media/web/features/MITM/RTK |
| **P2** | 5 | Polish: usage/observability/db/cli |

| Gap type | Findings | Meaning |
|---|---|---|
| `missing` | 61 | Feature/provider entirely absent in Rust |
| `behavior-diff` | 74 | Present but diverges in detail (one wrong detail breaks it) |
| `wrong-url` | 6 | Wrong URL/endpoint → request 404/fails |
| `partial` | 5 | Partially implemented, missing branch |

> ⚠️ **Golden rule:** This is a JS→Rust port — a single wrong detail (header, URL, field, order) breaks runtime. Every task must be checked against the source at `.tmp/9router` before coding.

---

## P0 — CRITICAL BROKEN (78 specs)

### A. Unreachable providers (17 specs)
- **17 enabled v0.5.50 providers missing from `default.rs PROVIDER_CONFIGS`** → HTTP 500 `UnsupportedProvider`: `alims-intl`, `api-airforce`, `baidu`, `bluesminds`, `clinepass`, `codebuddy-intl`, `featherless`, `kilo-gateway`, `perplexity-agent`, `poolside`, `selfhosted-embedding/stt/tts`, `tencent`, `tokenrouter`, `venice`, `zed`
- Config for `kilo-gateway`/`venice`/`featherless-ai`/`trae`/`devin-cli`/`windsurf`/`opencode-zen` sits in `PROVIDER_REGISTRY` (**dead code**) — must move to `PROVIDER_CONFIGS`
- **Wrong URLs:** youcom (`ydc-index.io` vs `api.you.com`), tortoise TTS (5000 vs 8000), blackbox (`/api/` vs `/v1/`), siliconflow (`.cn` vs `.com`), iflow (mintlify), baidu (missing `/chat/completions`)
- **Qwen STALE:** removed in 9router v0.5.50; Rust still ships executor + oauth

### B. Broken / missing executors (17 specs)
- `grok-web` / `perplexity-web`: **stubs** — missing MODEL_MAP, payload, NDJSON→SSE, wrong host
- `windsurf` (gRPC-web), `trae` (SOLO agent), `zed` (NDJSON + RSA auth), `codebuddy-intl`: **no executor**
- `kimchi`: strips `reasoning_content` **in the wrong direction** (response vs request)
- `kiro`: missing repair loop / stop-disposition
- `iflow` wrong host, `azure` wrong env names, `ollama-local` appends `?stream=`, `mimo-free` wrong marker, `qoder` missing `jt-` routing

### C. Missing OAuth (in sections B/C)
- `trae`/`windsurf`/`zed`/`codebuddy-intl`/`kimchi browser_token`/`grok-cli`: entirely missing
- `xAI` adds `prompt=login`; Codex refresh JSON vs form + drops `id_token`; `/register-session` route missing

### D. Missing media (29 specs)
- `selfhosted-stt/tts/embedding` (v0.5.50 headline) — missing + **api.openai.com fallback risk**
- `xiaomi-mimo TTS`, `vercel-ai-gateway embeddings`, `xai` image adapter, `antigravity` image — missing
- Search: chat-based search (8 providers), failover, timeout/sanitize — missing

---

## P1 — BEHAVIOR DIVERGENCE (39 specs)

- **Translator (13 specs):** drop temperature for Claude, `preserveCacheControl` hardcoded false, Kiro `is_error`/`max_thinking_length`/`inferenceConfig`, `client_metadata` strip, `reasoning_effort→reasoning`, `reasoning_details` array, Antigravity envelope
- **Features v0.5.35-50 (13 specs):** capacity adapter, codex-tui/desktop detection, `X-9Router-Token-Saver`, GitHub monthly reset, IntelliJ h2c, forceStream cached tokens, headroom byte-report, adaptive thinking, Grok Build subagent, xai video CLI, Default Key, Exa MCP, Ollama quota
- **Combo/MITM/RTK (12 specs):** MITM handlers/DNS/CA, git-log filter, caveman directives, system injector shapes, headroom formats, find backslash, capacity pools, combo heuristics
- **Web (3 specs):** PXPIPE, Donate, OIDC chip (+ 20 detailed findings)

---

## P2 — POLISH (5 specs)

- Usage history fields, request-log timestamp, observability default OFF, DB migrations, pending-request auto-clear, quota handlers (Vercel/CodeBuddy/Grok/Kiro)

---

## Confirmed CORRECT (not gaps — don't redo)

- ✅ i18n: 32 locales (incl. km, th, fa)
- ✅ Kiro GPT-5.6 family (44 models)
- ✅ Cursor 3.12.17 · Kimi dual-auth · Qoder PAT · Antigravity stream_options strip
- ✅ Complete translator request+response pairs
- ✅ Most TTS/STT providers

## REFUTED (skip)

- Default Key auto-provision (Rust has server-side boot) · hunyuan (present) · perplexity endpoint · venice media (JS doesn't have it either) · endpoint fetch

---

## Implementation Order

1. **P0 (A + B executors + E media selfhosted):** 17 providers + URL fixes + remove qwen + executor stubs → every provider works, no more 500/404
2. **P1 (C, D, E, F, G):** translator/oauth/media/web/features/MITM/RTK detail → 1:1 behavior
3. **P2 (H):** usage/observability/db/cli

**Verify:** every task has a guard test in the FULL report. Run `cargo test --lib` after each cluster. Open `.tmp/9router` at the cited JS line before coding.
