# Live E2E test report — openproxy ↔ real Codex CLI ↔ free provider

**Date:** 2026-09-03 · **Build tested:** openproxy 0.3.0 (release, freshly built from `main` + 2 fixes below) · **Client:** real `codex` CLI v0.118.0, real `opencode` binary present.

Scope requested: (1) prove openproxy works end-to-end with a real free-tier LLM provider and a real `codex exec` call, (2) sweep CLI vs Web feature parity, (3) flag any drift vs `decolua/9router`. This is a live-testing pass, not a re-run of the existing 146-item static parity audit (`parity-report.md` / beads epic `openproxy-mfs3`, which is already fully closed except one deferred P2).

## 1. Setup performed (real, not simulated)

1. `cargo build --release` → installed to `~/.local/bin/openproxy.exe` (0.2.0 → 0.3.0).
2. `openproxy provider add llm7-free '{"provider":"llm7", ...}'` — llm7.io is a genuinely key-less free OpenAI-compatible endpoint (`gpt-oss`, "turbo" tier, `tools_calling:true`).
3. `openproxy key add goaltest-key --auto` — minted a real client API key.
4. `openproxy server start` against a throwaway `--data-dir`.
5. Wired real `codex exec` at the model-provider config (`model_providers.openproxy.base_url = http://127.0.0.1:<port>/v1`, `wire_api = "responses"`), matching README's documented `OPENAI_BASE_URL` integration pattern.

## 2. Bugs found (real, reproduced, 2 fixed)

### 🔴 FIXED — `prompt_cache_key` (and other Responses-only fields) leaked into the Chat Completions body sent upstream
- **File:** `src/server/api/compat.rs`, `normalize_body()` / `CompatMode::Responses`.
- **Repro:** `codex exec` sends `prompt_cache_key` on *every* `/v1/responses` request (session-scoped cache key). openproxy's Responses→Chat normalization forwarded it verbatim to the underlying `/v1/chat/completions` call. Plain OpenAI-chat-compatible backends (llm7 confirmed; any non-OpenAI-native provider is equally exposed) reject the unknown field with a raw 400, which openproxy's generic error mapper turns into an opaque `502 "This model is not supported by the provider."` — actively misleading.
- **Impact:** every Codex request to any provider that isn't literally OpenAI's own API was broken. This is the single highest-impact finding of the session — it silently breaks the exact "codex + openproxy" flow the user asked to validate.
- **Fix applied:** strip `prompt_cache_key`, `store`, `include`, `parallel_tool_calls`, `background`, `previous_response_id`, `text`, `client_metadata`, and map `reasoning.effort → reasoning_effort` (then drop `reasoning`) in `normalize_body`'s Responses branch — mirroring the sibling translator that already does this correctly (`src/core/translator/request/openai_responses.rs:324-339`). Two independent code paths did the same conversion; only one had the strip list. **Verified fixed** with direct curl replay of a captured real Codex payload.

### 🔴 FOUND, NOT FIXED (design decision needed) — server never reloads API keys / provider connections added while running
- **Repro:** `openproxy key add` / `provider add` while the server is already up → new key returns `invalid_api_key`, new provider returns `No credentials for provider: X`, until the server process is restarted.
- **Root cause:** `chat.rs` only has a targeted stale-snapshot-recovery path for **combos** (`state.db.reload_snapshot()` — see comment at `chat.rs:290-307`, "combos created by the CLI process that bypasses the server's snapshot"). Keys and provider connections have no equivalent fallback.
- **Impact:** anyone driving openproxy purely from the CLI while the server is already running (exactly the workflow this session used, and exactly what "agent-first CLI" implies) hits silent auth failures that look like bad credentials, not staleness.
- **Suggested fix:** extend the same reload-on-miss pattern to key/provider lookups in `require_api_key` and `select_connection`, or make the CLI write path notify the running server (it already knows the pid/port via `openproxy.endpoint`).

### 🟡 FOUND, NOT FIXED (minor / cosmetic) — `provider test` requires a user-supplied `baseUrl` for providers that have a hardcoded default URL
- **File:** `src/server/api/providers.rs:766-794` (fallback branch of `test_provider_api`).
- **Repro:** `openproxy provider test llm7-free` → `FAIL llm7 (0ms) — Base URL required`, even though `PROVIDER_CONFIGS` in `default.rs` has a built-in default URL for `llm7` and real chat requests work fine.
- **Impact:** cosmetic/confusing for any of the ~20+ providers in `PROVIDER_CONFIGS` that ship a hardcoded URL and aren't in the connectivity-test's small hardcoded `match` (openai/anthropic/openrouter/...). Doesn't block actual traffic.

### ⚪ INVESTIGATED, NOT AN OPENPROXY BUG — llm7 free tier misclassifies Codex's real tool payload as "vision"
- Real `codex exec` against openproxy→llm7 still fails end-to-end: llm7 returns `400 Model 'gpt-oss' does not support vision input.` for Codex's actual ~13-tool / ~124 KB payload (shell, MCP resource tools, `view_image`, `spawn_agent`, ...), even after removing `view_image` specifically. Confirmed by replaying the **exact captured Codex request** directly against `api.llm7.io` (bypassing openproxy entirely) — same error. This is llm7's own (buggy) request classifier, not an openproxy translation defect. 9router would hit the identical failure against the identical upstream. Not actionable on the openproxy side; recorded here so it isn't mistaken for a router bug later.
- **Net result:** direct chat (`/v1/chat/completions`) and hand-built Responses-API calls (`/v1/responses`) through openproxy→llm7 **succeeded end-to-end** (streaming and non-streaming, verified with real HTTP round trips). The one remaining blocker for a clean full `codex exec` transcript is upstream-provider-side, not openproxy-side.

### ⚪ INVESTIGATED, REVERTED — `opencode-zen` is not actually a no-auth provider live
- Code has an `is_no_auth_provider()` allowlist (`chat.rs:2605`) covering `opencode`/`opencode-go` but not `opencode-zen`. Initially looked like a missing-entry bug matching the user's "opencode free" ask. Live-checked `https://opencode.ai/zen/v1/chat/completions` directly: it now returns `401 Missing API key` — it requires a real key today, so leaving it out of the no-auth allowlist is **correct**, not a bug. Change was reverted after verification.

## 3. CLI vs Web feature-parity sweep

Cross-checked `openproxy --help` command tree against every `web/src/pages/dashboard/*` page:

| Web page | CLI coverage |
|---|---|
| providers, combos, proxy-pools, media-providers, mitm, translator, usage, quota, console-log, db-backups, basic-chat, cli-tools | ✅ `provider`, `combo`, `pool`, `media`, `mitm`, `translator`, `usage`, `quota`, `logs`, `db`, `chat`, `tool` |
| compression, payload-rules, token-saver, settings/pricing | ✅ generic `settings set/get --key <camelCase>` covers these (no dedicated subcommand, but fully scriptable) |
| **pxpipe** | ❌ **Gap.** `/dashboard/pxpipe` page + `/api/pxpipe/*` routes exist server-side (`src/server/api/pxpipe.rs`), but there is **no `openproxy pxpipe` CLI subcommand at all** — not reachable from the CLI in any form. This is the one concrete "CLI can't do what the web can" gap found this session. |
| skills | Not a real gap — no backing `/api/skills` route exists either; it's a static/content page, not a CRUD feature, so nothing for the CLI to expose. |
| profile | Dashboard-session-only (password/OIDC identity chip) — not meaningfully CLI-shaped; `auth`/`settings` already cover the equivalent server-side knobs. |

## 4. 9router 1:1 status

The static/structural parity work (`docs/parity-9router-FULL.md`, epic `openproxy-mfs3`, 146 confirmed findings → 122 specs) is **already fully implemented and closed** — `br list --all` shows every P0/P1/P2 item in that epic done except one explicitly **deferred** P2 (`openproxy-mfs3.15`, SAML SSO, backend-only, not blocking). This session's live testing surfaced 3 *new* defects (1 fixed, 2 open) that the static audit didn't catch because they only manifest under a real agentic client's exact request shape (Codex's `prompt_cache_key`) or a real running-server workflow (stale key/provider snapshot) — i.e. dynamic/runtime gaps vs. the static JS↔Rust code-shape audit already done.

## 5. Recommended next actions (not done this session — sizing beyond current pass)

1. **P0:** Fix the stale-snapshot key/provider bug (#2 above) — breaks the CLI-first workflow this whole exercise was meant to validate.
2. **P1:** Add an `openproxy pxpipe` CLI subcommand mirroring `/api/pxpipe/*`, to close the one real CLI/Web gap found.
3. **P2:** Extend `provider test`'s upstream-URL resolution to fall back to `PROVIDER_CONFIGS`' built-in default URL instead of demanding a user-supplied one.
4. For further live Codex testing, use a provider with a real (even free-trial) API key rather than an anonymous key-less aggregator — llm7's classifier bug is upstream noise that will keep masking real signal.
