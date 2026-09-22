# Comprehensive Plan — OpenProxy Provider Simulation Layer ("Mock Lab")

> Status: APPROVED — review closed, ready for implementation (21 beads, §7). No code changes yet.
> Goal: **OpenProxy provides a provider-aware simulation layer that intercepts provider
> execution before network I/O, reproduces protocol- and behavior-level contracts for
> supported providers, and feeds the resulting execution through the exact same downstream
> pipeline as real provider execution.**
> User requirement: in dev mode, any provider configured via dashboard/CLI behaves as if
> real, but execution is simulated — no API key / subscription needed. Client/app changes nothing.

---

## 1. Problem & Goal

Today every dev/test request to a provider without credentials fails at network/auth:

```text
App → OpenProxy → https://api.xxx.com → 401 / no key / no subscription
```

Desired:

```text
App (unchanged)
  │  POST /v1/chat/completions, POST /v1/messages, Gemini generateContent...
  ▼
OpenProxy
  │  per-provider mode decision (REAL | MOCK)
  ├── REAL → forward to provider (current code path, untouched)
  └── MOCK → SimulationEngine synthesizes protocol-faithful response, zero network
```

**MVP scope is strictly LLM formats: OpenAI, Anthropic, Gemini (+ their
`-compatible` variants).** Non-LLM providers (Exa, Tavily, Firecrawl, …) are explicitly
out of MVP — they belong to a future "Tools/Search simulation" phase and must not shape
MVP abstractions. (Earlier drafts mentioned Exa; removed to keep scope clean.)

Success criteria for MVP:

1. Providers whose protocol format has a registered simulator expose Real/Mock
   configuration from dashboard + CLI. (NOT "all 40+ toggle mock" — see §3.6.)
2. Global dev kill-switch (`OPENPROXY_DEV_MOCK=1`) forces all supported providers to
   mock and **cannot be bypassed by any request header** (safety boundary, §3.2).
3. Simulated responses pass through the **same** pipeline as real: translate →
   account/fallback/combo → usage tracking → SSE streaming.
4. Fault injection via request headers triggers real fallback/retry paths, including
   **mixed REAL↔MOCK** fallback in both directions (§5).
5. No API key required in mock mode; credential loading/refresh is bypassed
   **before** it can fail (§3.7).
6. Default is `real` — production path is untouched when simulation is off.

Non-goals (MVP): record/replay, multi-step scenarios, stateful simulation, chaos
probabilities, dashboard Mock Lab page, semantic intelligence emulation, Exa/search,
full `BehaviorProfile`. These are Phase 2+.

---

## 2. Architecture

```text
Request → translate → Provider Resolver ─┬─ REAL     → existing forward path ─┐
                                         └─ SIMULATE → SimulationEngine ──────┤
                                                                              ▼
                                                                       Fault Injector
                                                                              │
                                                                              ▼
                                                                       ExecutionResult
                                                                              │
                                                                              ▼
                                                                    fallback / combo /
                                                                    usage / SSE / client
```

Key points (post-review corrections):

- **Fault Injector sits AFTER the execution abstraction**, not inside the simulator,
  and wraps **both** REAL and MOCK branches from MVP day one (contract in §2.4).
- **Simulator returns the same `ProviderExecutionResponse`** as real execution, so all
  downstream stages are shared.
- Naming: the subsystem is **Simulation** (`src/core/simulation/`). `Mock` is one
  execution mode of it, not the subsystem name. Request headers use the
  `x-openproxy-sim-*` prefix (not `x-mock-*`) so replay/chaos/scenario controls later
  share one namespace.

### 2.1 Engine abstraction (over `MockEngine`)

```rust
// src/core/simulation/engine.rs
pub trait ProviderSimulator: Send + Sync {
    fn format(&self) -> ProviderFormat;          // which protocol it simulates
    async fn execute(&self, ctx: &SimContext) -> Result<ProviderExecutionResponse, SimError>;
}

pub struct SimulationEngine {
    simulators: HashMap<ProviderFormat, Arc<dyn ProviderSimulator>>,
    // Phase 2+: + replayer, scenario runner — same interface, no refactor.
}

pub struct SimContext<'a> {
    pub config: &'a ProviderExecutorConfig,      // format, model, base_url (unused)
    pub provider: &'a str,                       // "openai", "openrouter", ... (passthrough;
                                                 // provider-specific overrides arrive in Phase 3)
    pub request: &'a ProviderExecutionRequest,   // body, stream flag, model
    pub fault: FaultSpec,                        // parsed x-openproxy-sim-* headers
}
```

Concrete simulators in MVP: `OpenAiSimulator`, `AnthropicSimulator`, `GeminiSimulator`.
OpenAI-compatible / Anthropic-compatible formats reuse the same simulator
(**protocol reuse** — behavior differences deferred, see §2.2).

### 2.2 Protocol vs behavior (MVP: no extra abstraction)

Format-driven simulation is correct **at the protocol layer** but must not assume
`OpenAI-compatible == OpenAI behavior`. For MVP this needs **no new type**:

- The provider name (`"openrouter"`, `"groq"`, …) travels in `SimContext.provider`
  as a plain passthrough for logging, `effectiveReason`-style attribution, and future
  overrides.
- Format-specific quirks already live where they belong: `ProviderExecutorConfig`
  (`format`, `stream_path`/`chat_path`, `default_headers`) — the simulator reads them
  from `SimContext.config`, reusing the existing `PROVIDER_CONFIGS` table. No new
  registry.

A `BehaviorProfile` struct (latency models, per-model capabilities, streaming shapes)
is **deferred to Phase 3** and will be introduced only when code proves it necessary —
no speculative seam in MVP.

### 2.3 Simulated model registry (minimal capability discovery)

Dashboard/CLI must never offer MOCK for something the engine can't execute (§3.6).
Each simulator ships a small model list so unknown-model validation and the dashboard
model picker keep working in mock mode:

```rust
pub struct SimulatedModel {
    pub id: String,                    // "gpt-5", "claude-opus-4-6", "gemini-2.5-pro"
    pub context_window: Option<u64>,
    pub supports_streaming: bool,
    pub supports_tools: bool,
}
```

Values are approximate, documented as such, and cover only the headline models per
format. Unknown model id → provider-correct 404 (this validates the error path).

### 2.4 Fault Injector placement (normative — MVP implements the full contract)

```text
REAL ───────┐
            ├─ ExecutionResult → FaultInjector → downstream (fallback/combo/usage/SSE)
MOCK ───────┘
```

The injector wraps **both** branches from MVP day one — no "REAL+fault later":

```rust
// src/core/simulation/fault.rs
pub struct FaultInjector;
impl FaultInjector {
    /// Execution-result middleware: transforms BOTH complete responses and
    /// streaming response bodies. Operates on `ProviderExecutionResponse`,
    /// whose stream variant carries `Stream<Item = Chunk>`:
    /// latency delays the first item, disconnect-after-N does `take(N)` then
    /// terminates the stream. Never touches the simulator or forward path.
    pub async fn apply(
        result: ProviderExecutionResponse,
        fault: &FaultSpec,
    ) -> Result<ProviderExecutionResponse, ProviderExecutorError>;
}
```

`x-openproxy-sim-*` headers are parsed once into `FaultSpec` before the branch; the
REAL branch strips them before forwarding upstream. MVP tests the injector on both
branches (MOCK+fault extensive; REAL+fault minimal — one status test proving the
contract, e.g. fault-injected 429 on a wiremock-backed REAL provider triggers
fallback).

### 2.5 Module layout

```text
src/core/simulation/
├── mod.rs            — exports, engine wiring
├── mode.rs           — ProviderExecutionMode + resolve_effective_mode
├── engine.rs         — ProviderSimulator trait + SimulationEngine dispatch
├── models.rs         — SimulatedModel registry per format (§2.3)
├── openai.rs         — OpenAiSimulator (non-stream + SSE + tools + errors)
├── anthropic.rs      — AnthropicSimulator (named SSE events + tool_use + errors)
├── gemini.rs         — GeminiSimulator (generateContent + SSE + errors)
├── fault.rs          — FaultSpec parsing + FaultInjector (§2.4)
└── fixtures.rs       — canned bodies: echo, tool echo, error envelopes
```

---

## 3. Mode System

### 3.1 Type

```rust
pub enum ProviderExecutionMode { Real, Mock }   // Replay/Hybrid arrive in Phase 2
```

### 3.2 Resolution precedence (corrected — global force is a safety boundary)

```text
1. GLOBAL FORCE: OPENPROXY_DEV_MOCK=1 env OR settings.dev_mock_all=true
      → ALWAYS mock (if simulator registered; else error loudly, §3.6).
      → NOT overridable by any request header. No bypass.
2. Per-request:  x-openproxy-sim: mock
      → real→mock only. There is NO header that forces mock→real.
      → Ignored (debug-logged) when global force is active (already mock anyway).
      → Forcing real is a privileged action: CLI / config / admin API only.
3. Per-provider: providers.mode column (default 'real').
4. Default:      real.
```

Rationale: a client-controlled header must never be able to turn a safe dev/CI
environment into real paid API calls.

### 3.3 Configured vs effective mode (always distinguished)

Every surface shows both:

```json
{ "configuredMode": "real", "effectiveMode": "mock", "effectiveReason": "OPENPROXY_DEV_MOCK" }
```

- Dashboard provider page: `Configured: REAL / Effective: MOCK (DEV_MOCK)`.
- CLI `provider status`: `MODE(effective)` + `CONFIGURED` columns, or
  `real (effective: mock via DEV_MOCK)`.
- `GET /api/mock/status`: `{ forcedAll, providers: { name: {configured, effective, reason} } }`.
- `effectiveReason`: `provider-config | request-header | OPENPROXY_DEV_MOCK | settings-force | default`.

### 3.4 Persistence (SQLite)

- Migration: `ALTER TABLE providers ADD COLUMN mode TEXT NOT NULL DEFAULT 'real'`
  (stores **configured** mode only). Settings: `dev_mock_all INTEGER DEFAULT 0`.
- Provider mode is **user data** (per AGENTS.md core surface #1): survives rebuilds,
  hot-reloaded via existing watcher.
- No secrets stored; `x-openproxy-sim-*` values never persisted.

### 3.5 CLI / API / Dashboard (MVP-2 slice, §4)

```bash
openproxy provider mode <name> <real|mock>   # set configured mode
openproxy provider mode <name>               # show configured + effective + reason
openproxy provider status                    # MODE columns (configured + effective)
```

- `--robot` provider payload gains additive `configuredMode`, `effectiveMode`,
  `effectiveReason`, `simulationSupported` fields (schema stability preserved).
- `PATCH /api/providers/:id {mode}` accepts configured mode.
- Dashboard `/dashboard/providers/<provider>`: Real/Mock toggle (configured) +
  effective-mode banner when they differ. `ModelSelectModal.tsx`: `🧪 mock` badge
  mirrored from the same source. Settings page: force-all checkbox.
- **Rebuild `web/dist`** after any `web/src` change (per AGENTS.md).

### 3.6 Simulation support vs mode support (no overclaim)

> All providers can carry simulation **configuration**; only formats with a registered
> simulator can **execute** in MOCK mode.

```rust
impl SimulationEngine {
    pub fn supports(&self, format: ProviderFormat) -> bool;
}
```

- MVP registers: `OpenAI`, `OpenAICompatible`, `Anthropic`, `AnthropicCompatible`, `Gemini`.
- Effective-mode resolution for an unsupported format + mock requested → explicit error
  (`SimulationUnsupported { provider, format }`), surfaced in API/CLI/dashboard as
  "mock unavailable for this provider format" — never a silent crash, never a fake
  MOCK badge.
- `simulationSupported: bool` is part of every mode payload (§3.3).

---

## 4. Execution Flow (credential bypass ordering — normative)

```text
resolve_effective_mode()
  │
  ├── MOCK → synthetic credential context (no key needed, any/non key accepted)
  │          → SimulationEngine::execute()
  │
  └── REAL → credential/account resolution → refresh if needed → forward (existing code)
```

Credential loading, validation, and OAuth refresh happen **after** mode resolution and
**only on the REAL branch**. A provider with no key / expired OAuth / invalid
credentials must still succeed in mock mode. Test matrix in §5 covers this.

Interception (both branches wrapped by the injector, `src/core/executor/provider.rs`,
top of `UnifiedExecutor::execute()`):

```rust
let eff = resolve_effective_mode(&self.provider, &request.headers, &state);
let fault = FaultSpec::parse(&request.headers);   // parsed once, both branches
let result = if eff.mode == ProviderExecutionMode::Mock {
    let sim = engine.require(&self.config.format)?;   // §3.6 error if unsupported
    sim.execute(&ctx).await?
} else {
    strip_sim_headers(&mut forward_headers);          // never leak upstream
    existing_real_forward(...).await?
};
FaultInjector::apply(result, &fault).await            // AFTER execution, both branches
```

Headers in `x-openproxy-sim-*` are stripped before any REAL forward and redacted
from logs like auth headers.

---

## 5. Mock Behavior Spec (per format)

Determinism (kept from v1, now explicit contract):

```text
same request + same simulation config → same response bytes.
```

No randomness in MVP. IDs = `mock-<hash(request)>`, timestamps derived from hash.
Default content echoes the last user message (`"Echo: …"`) — proves prompt delivery
and is snapshot-friendly.

**OpenAI / OpenAI-compatible**: non-stream object + `usage`; SSE `data: {delta}`
word-boundary chunks + `data: [DONE]`; `stream_options.include_usage` honored;
tool echo (first offered tool → `tool_calls`, `finish_reason:"tool_calls"`); unknown
model → 404 envelope; `429` envelope + `Retry-After` header.

**Anthropic / compatible**: `msg_mock_<hash>` object + `usage{input,output}_tokens`;
SSE **named events** in order (`message_start … content_block_delta … message_stop`)
— required by our SSE translator; tool echo → `tool_use` block +
`stop_reason:"tool_use"`; Anthropic error envelope.

**Gemini**: `candidates[].content.parts[].text` + `usageMetadata`; SSE chunks in
Gemini shape; Gemini error envelope.

Validation enforced in mock (behavior, not just HTTP): malformed body → 400;
context-overflow simulation only via explicit `x-openproxy-sim-status: 400` +
`context_length_exceeded` body (no silent truncation).

### 5.1 Fault headers (renamed namespace)

| Header | Effect |
|---|---|
| `x-openproxy-sim-status: 429\|500\|503\|400` | provider-correct error envelope + status |
| `x-openproxy-sim-latency-ms: N` | sleep N ms before first byte |
| `x-openproxy-sim-response: <json\|string>` | override semantics per §5.1.1 (never raw protocol) |
| `x-openproxy-sim-disconnect-after-chunks: N` | close SSE after N chunks |
| `x-openproxy-sim: mock` | per-request real→mock (never mock→real) |

#### 5.1.1 `x-openproxy-sim-response` override semantics (normative)

The header carries a **content-level** override, never a raw protocol body. The
simulator always owns the protocol envelope (schema, SSE framing, usage, ids):

- **Non-stream**: value replaces the *complete provider response body*.
  - If the value parses as JSON object → used as the response body verbatim
    (caller takes responsibility for schema correctness; compat tests don't cover it).
  - Else (plain string) → inserted as the message content/text into a
    simulator-generated envelope (ids, usage, `finish_reason`/`stop_reason` intact).
- **Stream**: value replaces the *content payload only*. The simulator still
  generates all SSE framing (OpenAI `data:` chunks + `[DONE]`; Anthropic named
  events; Gemini chunks), splitting the value on word boundaries with the usual
  chunk timing. A caller **cannot** inject raw SSE frames or break event ordering.
- **Tool echo interaction**: if the request contains `tools` and the header value is
  a JSON object with a `tool_calls`/`tool_use` shape, it replaces the echoed tool
  call; a plain string forces a text answer (suppresses tool echo).
- Malformed JSON object values → provider-correct 400 (validates the error path
  instead of silently passing garbage downstream).

### 5.2 Provider compatibility contract (formal, not "future gate")

`tests/simulation_compat.rs` asserts per format, against pinned envelopes:

- HTTP status, JSON schema, SSE event ordering, tool-call shape, usage shape,
  error shape — for non-stream AND stream.
- Reference oracle: LLMock / llm-mock run locally; a CI script (`scripts/sim-compat.sh`)
  sends identical requests to oracle and engine and diffs **schema shape**
  (not bytes — IDs/timestamps are engine-deterministic by design).

---

## 6. Testing Strategy

Harness: existing `wiremock 0.6` / `mockito 1` dev-deps; ephemeral ports only.

| Test | Asserts |
|---|---|
| OpenAI non-stream echo | schema, `Echo:` content, deterministic id, usage |
| OpenAI SSE | chunk sequence, terminal `[DONE]`, `include_usage` chunk |
| Anthropic SSE | named event order through the real translator path |
| Gemini non-stream + stream | candidates shape, `usageMetadata` |
| `sim-status:429` on openai | 429 envelope + `Retry-After` → **fallback fires** |
| Mixed fallback ×4 | mock→mock, **mock→real, real→mock**, real→real — proves simulation doesn't break fallback arch |
| Latency header | TTFB ≥ N; timeout path when N > client timeout |
| Mid-stream disconnect | truncated stream surfaces error, no hang |
| Global force | `OPENPROXY_DEV_MOCK=1` → all supported mock; header cannot force real |
| Credential-less mock | no key / expired OAuth / invalid creds → mock still 200 |
| Unsupported format | mock requested → explicit `SimulationUnsupported`, UI/API flag |
| Mode persistence | configured mode survives restart (SQLite reread) |
| Configured/effective split | `provider mode` shows both + reason |
| Determinism | same request twice → byte-identical response |
| Schema stability | `openproxy.v1.provider` additive fields only |
| Compat contract | `simulation_compat.rs` + oracle diff script green |

Gate per commit: `cargo fmt --check`, `cargo clippy --all-targets --all-features`,
`cargo test` (incl. `parity_tests stream_flags` smoke).

---

## 7. Phased Delivery (MVP split — small commits)

MVP-0 — mode + interception skeleton (no behavior change possible to merge safely):

- `01` simulation types (`mode.rs`: enum, resolved-mode struct w/ reason)
- `02` DB migration (`providers.mode`, `settings.dev_mock_all`)
- `03` mode resolver (precedence §3.2 + support check §3.6)
- `04` executor interception branch (default-off → all tests green)
- `05` `SimulationUnsupported` error type + plumbing

MVP-1 — engine + faults:

- `06` OpenAI non-stream · `07` OpenAI SSE · `08` OpenAI tools+errors
- `09` Anthropic non-stream+SSE · `10` Gemini non-stream+stream
- `11` model registry (`models.rs`) + unknown-model 404s
- `12` fault status · `13` fault latency · `14` fault disconnect + response override (§5.1.1)
- `15` FaultInjector on REAL branch + REAL+fault status test (§2.4)
- `16` credential bypass ordering (§4) + credential-less tests
- `17` fallback integration incl. 4 mixed-mode tests
- `18` compat contract (`simulation_compat.rs` + oracle script)

MVP-2 — surfaces:

- `19` CLI/API (`provider mode`, status columns, robot/schema, PATCH, `/api/mock/status`)
- `20` dashboard (toggle, effective banner, modal badge, settings checkbox, `pnpm build`)
- `21` docs (`docs/mock-mode.md`) + full gate green

Each bead ~100–300 LOC, branch `feat/sim-*`, `feat(sim): …` commits, PR ≤400 lines.
(21 tasks total; epic `openproxy-simulation-layer`. P2 replay / P3 scenario+chaos+Mock Lab are separate epics.)

---

## 8. File Change Inventory (expected)

| Area | Files | Change |
|---|---|---|
| Exec branch | `src/core/executor/provider.rs` | ~10-line mode branch |
| New engine | `src/core/simulation/{mod,mode,engine,models,openai,anthropic,gemini,fault,fixtures}.rs` | new (~1000–1300 lines + tests; **no `behavior.rs`** — deferred to Phase 3) |
| Wiring | `src/core/mod.rs`, `src/core/executor/mod.rs` | exports |
| Config/state | `src/core/config/*`, settings load | `dev_mock_all`, `OPENPROXY_DEV_MOCK` env |
| DB | `src/db/sqlite/*` migration + queries | `providers.mode`, settings flag |
| CLI/schema | `src/cli/provider_ext.rs`, `schema.rs` | `provider mode`, columns, robot fields |
| API | providers routes | `PATCH mode`, `/api/mock/status` |
| Dashboard | provider page, `ModelSelectModal.tsx`, settings | toggle + banner + badge + checkbox → `pnpm build` |
| Docs/tests | `docs/mock-mode.md`, `tests/simulation_*.rs`, `scripts/sim-compat.sh` | per §6 |

Untouched: translators, real forward path, auth crypto, combo logic, usage schema
(only additive `mock:true` + `billable:false` markers — §9 Q3 resolved: **record, don't skip**).

---

## 9. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Mock drifts from real protocol | compat contract §5.2 + oracle; pinned envelopes |
| SSE translator breaks on mock frames | byte-identical frame shapes; stream tests use real translator |
| Mock leaks into prod | default real; force is opt-in; effective-mode banner everywhere; logs tag mock |
| Dashboard stale mode | same provider-page data flow + watcher; `web/dist` rebuilt in-loop |
| Overclaim "40+ mock" | `simulationSupported` flag; explicit unsupported error |
| Scope creep (replay/scenario/Exa) | hard boundary in §1; separate epics |

---

## 10. Open Questions (for reviewer — updated)

1. ~~`x-mock-mode: real` bypass~~ → **Resolved**: no header may force mock→real. Only `x-openproxy-sim: mock` (real→mock). Force-real is CLI/config/admin only.
2. ~~Configured vs effective~~ → **Resolved**: always show both + reason (§3.3).
3. ~~Mock usage: record or skip~~ → **Resolved**: record with `mock:true`, `billable:false`.
4. SSE `chunk_delay_ms` default: 0 (fast tests) vs ~20ms (realistic)? Proposal: 0 default, header-tunable.
5. Simulated model lists: hand-maintained headline models (proposal) vs import from provider catalog at build time? Proposal: hand-maintained, ~10 per format.
6. MVP-0 merge: land resolver+branch default-off early (proposal: yes) or hold until engine ready? Proposal: land early, it de-risks integration.

---

## 11. Verification Checklist (before merge)

- [ ] fmt + clippy + full test suite green
- [ ] Manual: `provider add openai --api-key test-key` → `provider mode openai mock` → non-stream + `stream:true` SSE valid
- [ ] Manual: `-H "x-openproxy-sim-status: 429"` → fallback observed in logs
- [ ] Manual: mixed fallback mock→real and real→mock both work
- [ ] Manual: no key at all → mock still 200
- [ ] Manual: `OPENPROXY_DEV_MOCK=1` → all mock; no header can force real; unset → back
- [ ] Dashboard toggle + effective banner; modal badge mirrors; `web/dist` rebuilt
- [ ] `git status` / `git diff --cached` checked — no secrets
