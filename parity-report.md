# Parity Report: openproxy (Rust) → 9router v0.5.50 (Node.js)

> **Ngày:** 2026-08-12 · **Tham chiếu:** `.tmp/9router` = `decolua/9router` v0.5.50 (2026-08-05)
> **Cổng:** `openproxy` v0.2.0 (Rust) — khai báo "9router v0.5.30 full parity" (2026-07-10)
> **Cửa sổ gap:** v0.5.30 → v0.5.50 (4 release: .35, .40, .45, .50)

> 📄 **Báo cáo đầy đủ (implementation-ready, 122 specs):** xem **[`docs/parity-9router-FULL.md`](docs/parity-9router-FULL.md)** — mỗi spec có JS verbatim · Rust hiện tại · các bước implement · guard test · rủi ro · cross-check.
> 📄 Báo cáo gọn theo phần: **[`docs/parity-9router-impl.md`](docs/parity-9router-impl.md)**

---

## Tóm tắt kết quả audit

- **151 findings** từ 9 nhóm audit (160 subagents, ~5,498 tool calls)
- **146 CONFIRMED** (verify ngược — đọc lại JS lẫn Rust tại đúng dòng)
- **5 REFUTED** (Rust đã cover hoặc claim sai)
- → **122 implementation-specs** được tạo; **94 CONFIRMED + 24 PLAUSIBLE + 4 REFUTED** qua cross-check phản biện

| Ưu tiên | Số spec | Ý nghĩa |
|---|---|---|
| **P0** | 78 | VỠ request: 17 providers, URL sai, OAuth thiếu, stub executor |
| **P1** | 39 | Hành vi lệch: translator/oauth/media/web/features/MITM/RTK |
| **P2** | 5 | Tinh chỉnh: usage/observability/db/cli |

| Loại gap | Số findings | Ý nghĩa |
|---|---|---|
| `missing` | 61 | Tính năng/providers hoàn toàn thiếu trong Rust |
| `behavior-diff` | 74 | Có nhưng lệch chi tiết (sai 1 chi tiết là hỏng) |
| `wrong-url` | 6 | Sai URL/endpoint → request 404/fail |
| `partial` | 5 | Có một phần, thiếu nhánh còn lại |

> ⚠️ **Nguyên tắc vàng:** Đây là cổng JS→Rust — sai **1 chi tiết nhỏ nhất** (header, URL, field, thứ tự) sẽ làm hỏng runtime. Mọi task phải đối chiếu source tại `.tmp/9router` trước khi code.

---

## P0 — Cổng VỠ NGHIÊM TRỌNG (78 specs)

### A. Providers không với tới được (17 specs)
- **17 providers enabled v0.5.50 thiếu trong `default.rs PROVIDER_CONFIGS`** → HTTP 500 `UnsupportedProvider`: `alims-intl`, `api-airforce`, `baidu`, `bluesminds`, `clinepass`, `codebuddy-intl`, `featherless`, `kilo-gateway`, `perplexity-agent`, `poolside`, `selfhosted-embedding/stt/tts`, `tencent`, `tokenrouter`, `venice`, `zed`
- Config của `kilo-gateway`/`venice`/`featherless-ai`/`trae`/`devin-cli`/`windsurf`/`opencode-zen` nằm trong `PROVIDER_REGISTRY` (**dead code**) — phải chuyển sang `PROVIDER_CONFIGS`
- **Sai URL:** youcom (`ydc-index.io` vs `api.you.com`), tortoise TTS (5000 vs 8000), blackbox (`/api/` vs `/v1/`), siliconflow (`.cn` vs `.com`), iflow (mintlify), baidu (thiếu `/chat/completions`)
- **Qwen STALE:** 9router v0.5.50 đã xoá; Rust vẫn giữ executor + oauth

### B. Executor stub hỏng / thiếu (17 specs)
- `grok-web` / `perplexity-web`: **stub** — thiếu MODEL_MAP, payload, NDJSON→SSE, sai host
- `windsurf` (gRPC-web), `trae` (SOLO agent), `zed` (NDJSON + RSA auth), `codebuddy-intl`: **không executor**
- `kimchi`: strip `reasoning_content` **sai hướng** (response vs request)
- `kiro`: thiếu repair loop / stop-disposition
- `iflow` host sai, `azure` env names sai, `ollama-local` thêm `?stream=`, `mimo-free` sai marker, `qoder` thiếu `jt-` routing

### C. OAuth thiếu (nằm trong spec B/C)
- `trae`/`windsurf`/`zed`/`codebuddy-intl`/`kimchi browser_token`/`grok-cli`: thiếu hoàn toàn
- `xAI` thêm `prompt=login`; Codex refresh JSON vs form + mất `id_token`; `/register-session` route thiếu

### D. Media thiếu (29 specs)
- `selfhosted-stt/tts/embedding` (v0.5.50 headline) — thiếu + **rủi ro fallback api.openai.com**
- `xiaomi-mimo TTS`, `vercel-ai-gateway embeddings`, `xai` image adapter, `antigravity` image — thiếu
- Search: chat-based search (8 providers), failover, timeout/sanitize — thiếu

---

## P1 — Hành vi lệch (39 specs)

- **Translator (13 specs):** drop temperature cho Claude, `preserveCacheControl` hardcode false, Kiro `is_error`/`max_thinking_length`/`inferenceConfig`, `client_metadata` strip, `reasoning_effort→reasoning`, `reasoning_details` array, Antigravity envelope
- **Features v0.5.35-50 (13 specs):** capacity adapter, codex-tui/desktop detection, `X-9Router-Token-Saver`, GitHub monthly reset, IntelliJ h2c, forceStream cached tokens, headroom byte-report, adaptive thinking, Grok Build subagent, xai video CLI, Default Key, Exa MCP, Ollama quota
- **Combo/MITM/RTK (12 specs):** MITM handlers/DNS/CA, git-log filter, caveman directives, system injector shapes, headroom formats, find backslash, capacity pools, combo heuristics
- **Web (3 specs):** PXPIPE, Donate, OIDC chip (+ 20 findings chi tiết)

---

## P2 — Tinh chỉnh (5 specs)

- Usage history fields, request-log timestamp, observability default OFF, DB migrations, pending-request auto-clear, quota handlers (Vercel/CodeBuddy/Grok/Kiro)

---

## Đã xác nhận ĐÚNG (không phải gap — tránh làm lại)

- ✅ i18n: 32 locales (gồm km, th, fa)
- ✅ Kiro GPT-5.6 family (44 models)
- ✅ Cursor 3.12.17 · Kimi dual-auth · Qoder PAT · Antigravity stream_options strip
- ✅ Translator pairs đầy đủ request+response
- ✅ TTS/STT đa số providers

## Đã bị bác bỏ (REFUTED)

- Default Key auto-provision (Rust đã có server-side boot) · hunyuan (đã có) · perplexity endpoint · venice media (JS cũng không có) · endpoint fetch

---

## Thứ tự triển khai

1. **P0 (A + B executor + E media selfhosted):** 17 providers + URL fixes + xoá qwen + executor stubs → mọi provider chạy được, không còn 500/404
2. **P1 (C,D,E,F,G):** translator/oauth/media/web/features/MITM/RTK chi tiết → hành vi 1:1
3. **P2 (H):** usage/observability/db/cli

**Verify:** mỗi task có guard test trong FULL report. Chạy `cargo test --lib` sau mỗi cụm. Mở `.tmp/9router` tại dòng JS dẫn trước khi code.
