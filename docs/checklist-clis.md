# CLI Composite Checklist

> **Parent**: [Master Plan: OpenProxy = 9Router (Source of Truth Parity)](MASTER_PLAN_9ROUTER_PARITY.md)
> **Issue**: #P-CLI (sub-checklist of #P3.1-#P3.9)
> **Scope**: All 11+ CLI tools + 3 MITM tools
> **Reference**: 9router `v0.4.66-1-gcce8a50`, `src/shared/constants/cliTools.js`

---

## Acceptance criteria (Definition of Done)

For each CLI tool the following must be true:

- [ ] **`GET /api/cli-tools/<id>-settings`** returns correct path and has-flag.
- [ ] **`POST /api/cli-tools/<id>-settings`** writes the exact file/env the real CLI accepts.
- [ ] **All env var names match 9router** (`ANTHROPIC_AUTH_TOKEN` not `ANTHROPIC_API_KEY`, `OPENAI_BASE_URL` no `/v1`, etc.).
- [ ] **All settings-file paths are OS-correct** (Linux/macOS/Windows).
- [ ] **Real CLI boots with OpenProxy config** and successfully routes 1 prompt to OpenProxy logs.
- [ ] **Reset endpoint (`DELETE`)** cleans up exactly the keys OpenProxy wrote, no more.

---

## 11 Standard CLI Tools

### Phase 3.1 — CLI registry refactor

- [ ] Add `CLI_TOOLS` registry struct mirroring 9router `cliTools.js`: id, name, image, color, description, configType, envVars, settingsFile, guideSteps, modelAliases, defaultModels.
- [ ] Replace hardcoded `tools` Vec in `src/server/api/cli_tools.rs` with registry.
- [ ] All 11 CLI entries present in registry.
- [ ] Registry supports `custom`, `env`, `guide`, and `mitm` configTypes.

---

### Phase 3.2 — Codex CLI settings (NEW)

- [ ] `src/server/api/cli_tools/codex_settings.rs` created.
- [ ] Env: `OPENAI_BASE_URL=http://localhost:4623` (NO `/v1` suffix — CLI appends it).
- [ ] Env: `OPENAI_API_KEY=op-...`
- [ ] File: `~/.codex/config.json` with keys `baseUrl`, `apiKey`, `defaultModel` (lowercase).
- [ ] Config format: `{"baseUrl": "http://localhost:4623", "apiKey": "op-...", "defaultModel": "..."}`.
- [ ] Guide steps include UI button: "Open Codex CLI and run `export OPENAI_BASE_URL=http://localhost:4623`".
- [ ] `GET /api/cli-tools/codex-settings` returns correct response.
- [ ] `POST /api/cli-tools/codex-settings` writes both env and config.json.
- [ ] `DELETE /api/cli-tools/codex-settings` cleans up.
- [ ] Filesystem test: verifies written files match expected format.
- [ ] OS path correctness: Linux/macOS/Windows.

---

### Phase 3.3 — Cursor IDE settings (NEW, guide-only)

- [ ] `src/server/api/cli_tools/cursor_settings.rs` created.
- [ ] Guide steps only — no file writing (Cursor has no CLI-settable config file).
- [ ] Steps: Cursor Settings > Models > OpenAI API Base URL = `{{baseUrl}}`, API Key = `{{apiKey}}`, then "Add Custom Model".
- [ ] Explicit warning: **Requires Cursor Pro + cloud endpoint (NOT localhost)** — Cursor proxies all requests through its own backend. Localhost does not work.
- [ ] Note: model must be added manually in the UI (e.g., `cc/claude-sonnet-4-6`).
- [ ] `GET /api/cli-tools/cursor-settings` returns guide steps.
- [ ] `POST /api/cli-tools/cursor-settings` returns guide steps (no-op on file system).
- [ ] `DELETE /api/cli-tools/cursor-settings` returns guide steps (no-op).

---

### Phase 3.4 — Continue VSCode settings (NEW, JSON merge)

- [ ] `src/server/api/cli_tools/continue_settings.rs` created.
- [ ] File: `~/.continue/config.json` (Linux/macOS), `%USERPROFILE%\.continue\config.json` (Windows).
- [ ] Merge into `models[]`: `{"title":"{{model}}","provider":"openai","model":"{{model}}","apiKey":"{{apiKey}}","apiBase":"{{baseUrl}}"}`.
- [ ] Provider literal MUST be `"openai"` (NOT `"openrouter"` — that triggers Continue's special handling).
- [ ] `apiBase` MUST include `/v1` suffix.
- [ ] `GET /api/cli-tools/continue-settings` returns correct response.
- [ ] `POST /api/cli-tools/continue-settings` merges into existing config (reads, adds entry if not present, writes).
- [ ] `DELETE /api/cli-tools/continue-settings` removes only entries written by OpenProxy.
- [ ] Filesystem test: verifies merge behavior.
- [ ] OS path correctness: Linux/macOS/Windows.

---

### Phase 3.5 — Roo AI Assistant settings (NEW, globalState)

- [ ] `src/server/api/cli_tools/roo_settings.rs` created.
- [ ] VSCode globalState, extension `RooVeterinaryInc.roo-cline`.
- [ ] Same path pattern as Cline: `~/.config/Code/User/globalStorage/RooVeterinaryInc.roo-cline/settings/globalState.json` (Linux), `~/Library/Application Support/Code/...` (macOS), `%APPDATA%\Code\...` (Windows).
- [ ] Cline-like Ollama-compatible API provider type.
- [ ] Keys: same shape as Cline (actMode + planMode pairs for provider/model/baseUrl/apiKey).
- [ ] `GET /api/cli-tools/roo-settings` returns correct path.
- [ ] `POST /api/cli-tools/roo-settings` writes globalState.
- [ ] `DELETE /api/cli-tools/roo-settings` cleans up.
- [ ] OS path correctness: Linux/macOS/Windows.

---

### Phase 3.6 — Factory Droid settings (NEW)

- [ ] `src/server/api/cli_tools/droid_settings.rs` created.
- [ ] File: `~/.factory/settings.json`.
- [ ] Config format matches what Factory Droid CLI accepts.
- [ ] `GET /api/cli-tools/droid-settings` returns correct response.
- [ ] `POST /api/cli-tools/droid-settings` writes config.
- [ ] `DELETE /api/cli-tools/droid-settings` cleans up.
- [ ] OS path correctness: Linux/macOS/Windows.

---

### Phase 3.7 — OpenClaw one-click apply (NEW)

- [ ] `src/server/api/cli_tools/openclaw_settings.rs` created.
- [ ] One-click apply from dashboard — no user-facing file path (OpenClaw's own config format).
- [ ] Port 4623-aware configuration.
- [ ] `GET /api/cli-tools/openclaw-settings` returns apply steps.
- [ ] `POST /api/cli-tools/openclaw-settings` applies configuration.
- [ ] `DELETE /api/cli-tools/openclaw-settings` resets to previous config.

---

### Phase 3.8 — Verify 6 existing CLI modules against 9router

#### 1. claude (Claude Code) — `claude_settings.rs`

- [ ] Env var: `ANTHROPIC_BASE_URL` (NOT `ANTHROPIC_URL` or other).
- [ ] Env var: `ANTHROPIC_AUTH_TOKEN` (NOT `ANTHROPIC_API_KEY` — Claude Code rejects `API_KEY`).
- [ ] Env var: `ANTHROPIC_MODEL` present.
- [ ] Env var: `ANTHROPIC_DEFAULT_OPUS_MODEL` — value is a real `cc/<model>` model ID.
- [ ] Env var: `ANTHROPIC_DEFAULT_SONNET_MODEL` — value is a real `cc/<model>` model ID.
- [ ] Env var: `ANTHROPIC_DEFAULT_HAIKU_MODEL` — value is a real `cc/<model>` model ID.
- [ ] Env var: `API_TIMEOUT_MS` present (9router default).
- [ ] Env var: `DISABLE_TELEMETRY` present (9router default).
- [ ] Env var: `DISABLE_ERROR_REPORTING` present (9router default).
- [ ] File path: `~/.claude/settings.json` (OS-correct).
- [ ] Settings file contains only `{"env": {...}}` block.
- [ ] `GET /api/cli-tools/claude-settings` returns correct response.
- [ ] `POST /api/cli-tools/claude-settings` writes correct env block.
- [ ] `DELETE /api/cli-tools/claude-settings` removes only OpenProxy-written keys.
- [ ] Real CLI test: `claude --version` + `claude -p "hello"` routes to OpenProxy.

#### 2. cline (VSCode Cline) — `cline_settings.rs`

- [ ] File path: `~/.config/Code/User/globalStorage/saoudrizwan.claude/settings/globalState.json` (Linux).
- [ ] File path: `~/Library/Application Support/Code/User/globalStorage/saoudrizwan.claude/settings/globalState.json` (macOS).
- [ ] File path: `%APPDATA%\Code\User\globalStorage\saoudrizwan.claude\settings\globalState.json` (Windows).
- [ ] API provider type: "Ollama" (OpenAI-compatible, must include `/v1` in base URL).
- [ ] Mode keys: `actModeApiProvider`, `actModeApiModelId`, `actModeOpenAiBaseUrl`, `actModeOpenAiModelId`, `actModeOpenAiApiKey`.
- [ ] Mode keys: `planModeApiProvider`, `planModeApiModelId`, `planModeOpenAiBaseUrl`, `planModeOpenAiModelId`, `planModeOpenAiApiKey`.
- [ ] All 10 keys (act/plan x 5) present after write.
- [ ] `GET /api/cli-tools/cline-settings` returns correct path.
- [ ] `POST /api/cli-tools/cline-settings` writes globalState correctly.
- [ ] `DELETE /api/cli-tools/cline-settings` cleans up exactly the 10 keys.
- [ ] OS path correctness: Linux/macOS/Windows.

#### 3. kilo (Kilo Code) — `kilo_settings.rs`

- [ ] VSCode extension id: `kilocode.kilo-code` (NOT `kilocode`).
- [ ] Same globalState path as Cline but under `kilocode.kilo-code` extension.
- [ ] Auth keys live under this ext's storage.
- [ ] `GET /api/cli-tools/kilo-settings` returns correct path.
- [ ] `POST /api/cli-tools/kilo-settings` writes globalState correctly.
- [ ] `DELETE /api/cli-tools/kilo-settings` cleans up.
- [ ] OS path correctness: Linux/macOS/Windows.

#### 4. hermes (Nous Hermes Agent) — `hermes_settings.rs`

- [ ] File path: `~/.hermes/settings.json`.
- [ ] Verify exact config schema matches what Hermes CLI reads.
- [ ] `GET /api/cli-tools/hermes-settings` returns correct response.
- [ ] `POST /api/cli-tools/hermes-settings` writes config.
- [ ] `DELETE /api/cli-tools/hermes-settings` cleans up.

#### 5. cowork (Claude Cowork) — `cowork_settings.rs`

- [ ] File path: `~/Library/Application Support/Claude/Cowork.json` (macOS).
- [ ] File path: `~/.config/Claude/Cowork.json` (Linux).
- [ ] File path: `%APPDATA%\Claude\Cowork.json` (Windows).
- [ ] `GET /api/cli-tools/cowork-settings` returns correct path.
- [ ] `POST /api/cli-tools/cowork-settings` writes config.
- [ ] `DELETE /api/cli-tools/cowork-settings` cleans up.
- [ ] OS path correctness: Linux/macOS/Windows.

---

## 3 MITM Proxy Tools

### Phase 3.9 — Verify MITM domains + model aliases

#### 1. antigravity IDE MITM

- [ ] CA cert install step documented.
- [ ] Route `daily-cloudcode-pa.googleapis.com` to MITM proxy.
- [ ] Route `cloudcode-pa.googleapis.com` to MITM proxy.
- [ ] Route `cloudcode-pa.sandbox.googleapis.com` to MITM proxy.
- [ ] Model alias: `gemini-3.5-flash-low` (mandatory — unmapped = pass-through).
- [ ] Model alias: `gemini-3-flash-agent`.
- [ ] Model alias: `gemini-3.5-flash-extra-low`.
- [ ] Model alias: `gemini-3.1-pro-low`.
- [ ] Model alias: `gemini-pro-agent`.
- [ ] Model alias: `claude-sonnet-4-6`.
- [ ] Model alias: `claude-opus-4-6-thinking`.
- [ ] Model alias: `gpt-oss-120b-medium`.
- [ ] Model alias: `gemini-3-flash`.
- [ ] `MODEL_NO_MAP` list for `tab_*` models (never re-routed).
- [ ] `GET /api/cli-tools/antigravity-settings` returns correct response.
- [ ] Unit tests: all aliases present in config.

#### 2. copilot (GitHub Copilot IDE) MITM

- [ ] Route `api.individual.githubcopilot.com` (NOT `api.githubcopilot.com` — that is for VSCode extension).
- [ ] Header: `x-github-api-version: 2025-04-01`.
- [ ] Model alias: `gpt-4o`.
- [ ] Model alias: `gpt-4.1`.
- [ ] Model alias: `gpt-5-mini`.
- [ ] Model alias: `claude-haiku-4.5`.
- [ ] `GET /api/cli-tools/copilot-settings` returns correct response.
- [ ] Unit tests: all aliases present in config.

#### 3. kiro IDE MITM

- [ ] Route `q.us-east-1.amazonaws.com` to MITM proxy.
- [ ] Model alias: `auto` (mandatory — unmapped = pass-through).
- [ ] Model alias: `claude-sonnet-4.5`.
- [ ] Model alias: `claude-sonnet-4`.
- [ ] Model alias: `claude-haiku-4.5`.
- [ ] Model alias: `deepseek-3.2`.
- [ ] Model alias: `minimax-m2.5`.
- [ ] Model alias: `minimax-m2.1`.
- [ ] Model alias: `glm-5`.
- [ ] Model alias: `simple-task`.
- [ ] `GET /api/cli-tools/kiro-settings` returns correct response.
- [ ] Unit tests: all aliases present in config.

---

## Quirks Summary (must not regress)

| # | CLI | Key quirk |
|---|---|---|
| 1 | claude | Env var `ANTHROPIC_AUTH_TOKEN` (NOT `ANTHROPIC_API_KEY`). Opus/sonnet/haiku defaults must be real `cc/<model>`. |
| 2 | codex | `OPENAI_BASE_URL` has NO `/v1` suffix. Config JSON keys are lowercase: `baseUrl`, `apiKey`, `defaultModel`. |
| 3 | cursor | localhost does NOT work — need tunnel/cloud endpoint. Cursor Pro required. No file write, guide only. |
| 4 | cline | "Ollama" provider type. `/v1` suffix required in base URL. Two modes: actMode + planMode, both need settings. |
| 5 | continue | Provider literal `"openai"` (NOT `"openrouter"`). `apiBase` includes `/v1`. JSON merge into existing config. |
| 6 | roo | Identical to Cline but ext id `RooVeterinaryInc.roo-cline`. |
| 7 | kilo | Ext id `kilocode.kilo-code` (fully qualified). |
| 8 | openclaw | One-click apply from dashboard. No user-facing file path. |
| 9 | hermes | Nous Research self-improving agent. Verify exact config schema. |
| 10 | droid | Factory Droid. Verify path and config format. |
| 11 | cowork | Claude Desktop third-party inference. Verify OS-specific paths. |
| 12 | antigravity | MITM. Mandatory alias `gemini-3.5-flash-low`. `tab_*` models never re-routed. |
| 13 | copilot | MITM. Domain `api.individual.githubcopilot.com` (not `api.githubcopilot.com`). |
| 14 | kiro | MITM. Mandatory alias `auto`. Route `q.us-east-1.amazonaws.com`. |

---

## File-by-file action list

| File | Phase | Status | Action |
|---|---|---|---|
| `src/server/api/cli_tools.rs` | P3.1 | REFACTOR | Replace hardcoded tools Vec with `CLI_TOOLS` registry struct. |
| `src/server/api/cli_tools/codex_settings.rs` | P3.2 | NEW | Create. Env + `~/.codex/config.json` with lowercase keys, no `/v1` suffix. |
| `src/server/api/cli_tools/cursor_settings.rs` | P3.3 | NEW | Create. Guide-only. Warning: requires Pro + cloud endpoint. |
| `src/server/api/cli_tools/continue_settings.rs` | P3.4 | NEW | Create. JSON merge into `~/.continue/config.json`. Provider `"openai"`. |
| `src/server/api/cli_tools/roo_settings.rs` | P3.5 | NEW | Create. VSCode globalState, ext `RooVeterinaryInc.roo-cline`. |
| `src/server/api/cli_tools/droid_settings.rs` | P3.6 | NEW | Create. `~/.factory/settings.json`. |
| `src/server/api/cli_tools/openclaw_settings.rs` | P3.7 | NEW | Create. One-click apply, port 4623-aware. |
| `src/server/api/cli_tools/claude_settings.rs` | P3.8 | VERIFY | Add `ANTHROPIC_AUTH_TOKEN`, verify all env var names, model IDs. |
| `src/server/api/cli_tools/cline_settings.rs` | P3.8 | VERIFY | Verify OS paths, all 10 keys (act/plan). |
| `src/server/api/cli_tools/kilo_settings.rs` | P3.8 | VERIFY | Verify ext id `kilocode.kilo-code`. |
| `src/server/api/cli_tools/hermes_settings.rs` | P3.8 | VERIFY | Verify config path and schema. |
| `src/server/api/cli_tools/cowork_settings.rs` | P3.8 | VERIFY | Verify OS-specific Cowork.json paths. |
| `src/core/mitm/config.rs` | P3.9 | VERIFY | Verify domains + aliases. Add `MODEL_NO_MAP` (`tab_*` models). |
| `src/core/mitm/handlers/copilot.rs` | P3.9 | VERIFY | Verify domain + header. |
