# Context: Open-sse Rust Completion

**Date**: 2026-05-04
**Feature**: open-sse-rust-completion
**Status**: Locked for Planning / Pending Approval
**Owner**: quangdang

---

## 1. Request Snapshot

- Requested outcome: complete the remaining backend/runtime port so the live project reduces to a Rust backend plus the existing Next.js dashboard, with `open-sse/*` no longer required on active request paths.
- Why it matters: the Rust server already owns a broad API surface, but the repo still carries a large JS runtime core for translation, RTK, provider specialization, and stream handling.
- Planning note: downstream work should port the delta from `open-sse/*` into the current Rust server rather than re-implement routes that are already in Rust.

---

## 2. Scope Boundaries

- In scope: request/response translator parity, chat-core orchestration extraction, missing specialized executors, RTK compression filters/autodetection, remaining runtime services/utilities needed to retire `open-sse/*`, and local cloud-sync handler parity needed by the Rust server.
- Out of scope: dashboard rewrite, new providers beyond the current openproxy matrix, the separate `cloud/` worker implementation, or speculative architecture unrelated to removing the JS runtime core.
- Deferred ideas: dashboard parity/polish, broad UI cleanup, and route-level refactors that do not materially help the Rust cutover.

---

## 3. Locked Decisions

| ID | Decision | Why It Is Locked | Impact |
|----|----------|------------------|--------|
| D1 | Target semantic parity with `/data/projects/openproxy` and the current JS runtime, not route-count parity. | The repo already has many Rust routes; the remaining work is behavior-level parity. | Verification must compare request/response behavior for compatibility routes and provider flows, not just route presence. |
| D2 | Dashboard work stays deferred; preserve the current Next.js UI and focus on backend/runtime parity first. | The current repo and prior direction already separate backend parity from dashboard follow-up work. | No phase in this plan should expand into dashboard redesign or UI parity work. |
| D3 | `open-sse/*` stays as the reference implementation until matching Rust verification exists. | Deleting or diverging from the JS core early would remove the only ground-truth implementation for several provider and translation paths. | Runtime cutover and JS pruning happen only at the end, after parity evidence exists. |
| D4 | Start with translator plus chat-core contract work, then fill executor gaps, then port RTK. | Translator/chat-core semantics are shared by all provider lanes; doing executors first would duplicate request/response logic. | Shared-core work blocks dependent executor beads; RTK is intentionally sequenced later. |
| D5 | Completion requires real verification: Rust tests/clippy, live curl parity checks against the running server, and browser-use dashboard smoke tests. | The user explicitly requires both curl-based backend checks and browser-use UI validation, not just static test gates. | Every phase must produce explicit pass/fail evidence from cargo tests, curl commands, and browser-use flows before cutover claims are made. |

---

## 4. Non-Negotiables

- Keep already-ported Rust endpoints working while parity expands; no regressions on the existing Rust route surface.
- Reuse existing Rust subsystems where they already exist, especially `src/core/account_fallback`, `src/core/usage`, `src/core/executor/*`, and `src/server/api/compat.rs`.
- Do not silently turn this into dashboard work.
- Do not delete `open-sse/*` until a verified Rust path replaces it.
- Preserve the repo contract that Rust owns the backend/runtime and Next.js remains the dashboard/web surface.

---

## 5. Repo Anchors From Initial Scout

- `src/server/api/mod.rs` - current Rust route surface already owns `/v1/chat/completions`, `/v1/messages`, `/v1/responses`, `/v1/responses/compact`, `/v1/web/fetch`, and many `/api/*` endpoints.
- `src/server/api/chat.rs` - monolithic Rust chat path already contains combo routing, preprocessing, fallback, and response bridging; this is the primary extraction seam.
- `src/core/translator/response_transform.rs` - response-side transformation groundwork exists, but request-side translator infrastructure is still missing.
- `src/core/rtk/mod.rs` - only caveman/context-pressure preprocessing is present; the JS RTK compression/filter pipeline is not.
- `open-sse/handlers/chatCore.js` and `open-sse/translator/*` - authoritative baseline for the runtime semantics still living in JS.

Keep this section lightweight.
This is not the full discovery write-up.

---

## 6. Open Questions That Must Not Be Silently Resolved

- [ ] Should the final cleanup phase delete dormant Next.js API route implementations under `src/app/api/**`, or only ensure traffic no longer depends on them? - This changes the cutover and cleanup scope.

---

## 7. Planning Handoff

- Context ready for technical planning: yes
- Next artifact: `plans/open-sse-rust-completion.md`
- Encode note: downstream beads should reference decision IDs when those decisions materially affect implementation
