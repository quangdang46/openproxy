# Fix Implementation Plan — OpenProxy Security & Quality Audit

**Date:** 2026-07-26
**Status:** active
**Root pane:** w3T:p1

## Packages

| # | Package | Owner | Scope | Priority |
|---|---------|-------|-------|----------|
| 1 | **security-hardening** | TBD | OAuth secrets → env vars, API_KEY_SECRET, A2A auth, OAuth route auth, default password, constant-time compars, OIDC, JTI, CORS, guard fixes, get_tts_voices auth | CRITICAL |
| 2 | **combo-mcp-db** | TBD | check_fallback_error, backoff_level, shadow cancel, upstream_body, token refresh mutex, DB incremental writes, AES-GCM, MCP auth/deadlock/delete-bulk | HIGH |
| 3 | **executors-translators** | TBD | Azure endpoint fix, Vertex URL templates, response_transform.rs dead code, iflow expect, OpenRouter TTS, Nanobanana typo, SSE inconsistency | HIGH |
| 4 | **server-media-cli** | TBD | Real-IP middleware, unused handlers, dead settings files, tags double-serialization, usage mutex, chat expect, STT dedup, HTTP timeouts, CLI runtime issues | HIGH |

## Integration plan
- Each package in its own worktree: `orch/fix-<package>`
- All worktrees branched from HEAD
- After all done: review and merge in dependency order (1 → 2 → 3 → 4)
- cargo fmt + cargo clippy on each before merge
- Evidence: `git log` per package, diff stats
