# OpenProxy — Rust AI Proxy Router

## What
OpenProxy is an AI proxy router written in Rust — OpenAI-compatible endpoint that routes requests to 40+ AI providers with format translation, account fallback, token refresh, usage tracking, and SSE streaming.

## Why
Replace 9router (Node.js) with a faster, safer Rust implementation that avoids 235+ bugs found in the JS version. Critical patterns: type-safe format handling, encrypted secrets, immutable data flow, thread-safe by design.

## How (Architecture)
- **Core**: model parsing → format detection → request translation → provider execution → response translation → SSE streaming
- **Account mgmt**: credential selection → token refresh → model-level fallback → combo/fusion
- **Executor trait**: `ProviderExecutor` with default+specialized impls
- **Persistence**: SQLite WAL + encrypted columns + usage tracking
- **Security**: HMAC API keys, bcrypt auth, SSRF protection

## Beads
23 beads in `.beads/` organized across 7 phases. See `bv --robot-next` or `br ready` for actionable items.

## Key References
- `9router-audit-plan.md` — full plan with 9router bugs avoided per bead
- 9router repo at `/tmp/9router` for reference implementation (do NOT copy JS patterns directly)

## Status
Phase 1-6 planned, no implementation started yet. Start with `bv --robot-next`.
