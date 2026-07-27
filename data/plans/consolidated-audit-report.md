# OpenProxy — Consolidated Audit Report

**Date:** 2026-07-26
**Scope:** Full source audit (~150K lines, 180+ files, 4 Herdr investigator panes + 4 deep-dive subagents)
**Root pane:** w3T:p1 | **Project:** openproxy v0.2.0

---

## Executive Summary

Comprehensive audit reveals **6+ CRITICAL**, **30+ HIGH**, **50+ MEDIUM**, and **30+ LOW** findings across all subsystems. The most critical issues involve **hardcoded OAuth secrets in source code**, **A2A credentials leak (no auth)**, **15+ unauthenticated OAuth routes**, **default HMAC API key secret**, **MCP admin auth bypass**, and **critical combo routing bugs**.

---

## 🔴 CRITICAL / BLOCKER (7 issues)

| # | Area | Issue | File:Line |
|---|------|-------|-----------|
| C1 | Auth | **Hardcoded default HMAC secret** — `API_KEY_SECRET` falls back to well-known string `"endpoint-proxy-api-key-secret"` | `core/auth/mod.rs:15-23` |
| C2 | OAuth | **Hardcoded iflow client_secret** `"4Z3YjXycVsQvyGF1etiNlIBB4RsqSDtW"` | `oauth/providers.rs:166-168` |
| C3 | OAuth | **Hardcoded Antigravity Google OAuth credentials** | `oauth/antigravity.rs:21-28` |
| C4 | OAuth | **Hardcoded Gemini CLI client_secret** `"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"` | `oauth/gemini_cli.rs:21-23` |
| C5 | Server | **A2A `handle_provider_skill` leaks ALL credentials** — no auth, serializes full `ProviderConnection` with tokens | `core/a2a.rs:336-345` |
| C6 | Server | **15+ OAuth routes have NO authentication** — anyone can import tokens, create providers | `server/api/oauth.rs:5313-5361` |
| C7 | Combo | **`check_fallback_error` always returns `should_fallback: true`** — ErrorClassification has no permanent-error variant, so 400/401 burns through all combo members | `combo/mod.rs:384-407` |

---

## 🟠 HIGH (30+ issues)

### Security & Auth (12)
| # | Issue | File:Line |
|---|-------|-----------|
| H1 | Default dashboard password "123456" if neither bcrypt hash nor INITIAL_PASSWORD set | `server/api/auth.rs:105-109` |
| H2 | Fallback password comparison NOT constant-time (== instead of timing_safe_eq) | `server/api/auth.rs:105-108` |
| H3 | API key CRC only 32 bits (1:4B collision, 65K guesses for 50%) | `core/auth/mod.rs:103-108` |
| H4 | Main API key auth NOT constant-time (only CLI token path uses timing_safe_eq) | `server/auth/mod.rs:284-292` |
| H5 | O(n) linear scan of API keys on every authenticated request | `server/auth/mod.rs:284-292` |
| H6 | OIDC rejects ES256/EdDSA tokens (only RS256/384/512 accepted) | `server/auth/oidc.rs:202-211` |
| H7 | OIDC `expect()` panic on malformed IdP URL | `server/auth/oidc.rs:138` |
| H8 | JTI revocation set is unbounded (`DashSet` entries never evicted) | `server/auth/mod.rs:45` |
| H9 | Secure cookie trusts spoofable `X-Forwarded-Proto` header | `server/api/auth.rs:150-155` |
| H10 | LOCAL_ONLY guard falls back to `0.0.0.0` instead of `127.0.0.1` | `server/api/guard.rs:154-159` |
| H11 | CORS spec violation: credentials + wildcard origin (*) breaks browsers | `server/api/cors.rs:7-28` |
| H12 | `require_admin` allows any valid API key, not just management keys | `server/api/guard.rs:127-145` |

### Server API (6)
| # | Issue | File:Line |
|---|-------|-----------|
| H13 | **`get_tts_voices()` has NO authentication, reads DB credentials** | `server/api/media_providers.rs:828` |
| H14 | Real-IP middleware strips legitimate forwarding headers from trusted proxies | `server/api/guard.rs:78-93` |
| H15 | MCP endpoint has no auth — anyone can call any MCP tool | `server/api/mcp_server.rs:154-160` |
| H16 | `models_availability.clear_cooldown` has no auth | `server/api/models_availability.rs:145-153` |
| H17 | 7 API handler functions defined but never registered in router | `server/api/mod.rs:1117-1667` |
| H18 | 4 CLI tool settings files (1284 lines) dead code — never compiled | `cli_tools/codex_settings.rs`, `cursor_settings.rs`, `droid_settings.rs`, `roo_settings.rs` |

### Data & Combo (5)
| # | Issue | File:Line |
|---|-------|-----------|
| H19 | Full database rewrite on every `update()` — deletes ALL rows and reinserts | `db/mod.rs:253-258` |
| H20 | AES-256-CBC with SHA-256 key derivation (no KDF, no authenticated encryption) | `db/crypto.rs:36-43` |
| H21 | `backoff_level` hardcoded to 0 — transient error backoff never escalates within a request | `combo/mod.rs:742` |
| H22 | Shadow tasks never cancelled on primary success (wastes CPU/quota) | `combo/shadow.rs:225-276` |
| H23 | `upstream_body` never populated — provider error details always discarded | `combo/mod.rs:134,732` |

### MCP (3)
| # | Issue | File:Line |
|---|-------|-----------|
| H24 | MCP admin operations (create/delete providers/keys) with ZERO auth | `core/mcp/server.rs:204-573` |
| H25 | `block_in_place` + `block_on` deadlock risk on single-threaded runtime | `core/mcp/server.rs:193-201` |
| H26 | `provider_delete` matches by name OR ID — accidental bulk delete | `core/mcp/server.rs:276-285` |

### OAuth (2)
| # | Issue | File:Line |
|---|-------|-----------|
| H27 | Antigravity & Gemini CLI OAuth both bind to fixed port 8080 (conflict) | `oauth/antigravity.rs:367-369`, `gemini_cli.rs:274-276` |
| H28 | No concurrency protection on token refresh (dual refresh race) | `oauth/token_refresh.rs` |

### Media (3)
| # | Issue | File:Line |
|---|-------|-----------|
| H29 | **STT subsystem fully duplicated** — `core/media/stt/` and `server/api/stt.rs` are near-identical implementations | Multiple files |
| H30 | OpenRouter TTS silent model discard: single-slash `model/voice` parsing broken | `media/tts/openrouter.rs:31-42` |
| H31 | No HTTP request timeouts on any TTS/STT/Image adapter | All media adapters |

---

## 🟡 MEDIUM (~50+ issues — categorized)

### Combo/Routing
- Quarantine entries never expire if no read occurs (`combo/mod.rs:106-107`)
- Shadow tasks leaked with no abort mechanism (`shadow.rs:235-276`)
- Round-robin rotation state lost on restart (`combo/mod.rs:93-94`)
- Hedging continues collecting after first success (`hedging.rs:165-185`)
- `SlotGuard` in_flight permanently leaked on task cancellation (`account_fallback/mod.rs:52-58`)
- `FillFirst` greedily picks highest quota, hammers one account (`account_fallback/mod.rs:195-197`)
- Sticky strategy picks first available without health check (`account_fallback/mod.rs:235-240`)
- `prioritize_capacity` in hedging config is dead code (`hedging.rs:65`)

### Database
- Snapshot + SQLite update not atomically consistent on crash (`db/mod.rs:253-260`)
- `looks_like_ciphertext()` heuristic false-positive risk — could clear valid data (`db/crypto.rs:224-237`)
- No incremental migration path exists (`db/sqlite/migrations.rs:43-51`)
- SQLite writes via `spawn_blocking` with no dedicated pool (`db/mod.rs:96-100`)

### OAuth
- Device code flow has no TLS pinning (`oauth/mod.rs:111-140`)
- `PendingFlowStore` expired flows accumulate in memory (`oauth/pending.rs:80-87`)
- Kiro OAuth client registration expires in 1 hour with no re-registration (`oauth/mod.rs:299-352`)

### RTK
- Compressed payloads cloned from body unnecessarily — doubles memory (`rtk/mod.rs:529-673`)
- Auto-detection runs per-tool-result, not per-turn — O(n*m) (`rtk/mod.rs:758-761`)
- Wenyan prompts produce mixed-language output with English conversations (`rtk/mod.rs:81-94`)

### MCP
- Input sizes not validated on tool creation (`mcp/server.rs:226-263`)
- No audit logging for mutating MCP operations (`mcp/server.rs:204-573`)
- No pagination on list operations (`mcp/server.rs:208-215`)

### Server API
- Double-serialization bug in tags endpoint (`tags.rs:48`)
- Concurrent Headroom `start()` can spawn multiple PIDs (`headroom.rs:393-496`)
- Concurrent MITM `start()` races (`mitm_config.rs:364-380`)
- Usage stream mutex held for entire SSE lifetime (blocks concurrent subscribers) (`usage.rs:152-161`)
- `chat.rs` `expect()` panics if state machine inconsistent (`chat.rs:1844, 1870`)
- `provider_validate.rs` `no_auth` check runs before empty provider check (`provider_validate.rs:34-49`)
- `v1_api_chat.rs` doesn't forward auth headers downstream (`v1_api_chat.rs:75-77`)
- `shutdown.rs` uses Node.js `NODE_ENV` convention (`shutdown.rs:20`)

### CLI Tools
- 5 test functions in `compat.rs` missing `#[test]` attribute (never execute) (`compat.rs:2563-2637`)
- Continue settings hardcodes `localhost:4623/v1` (`continue_settings.rs:19`)
- Kilo VSCode path is Linux-only (macOS/Windows missing) (`kilo_settings.rs:298-304`)
- OpenClaw config endpoint exposes raw API key in response (`openclaw_settings.rs:68`)
- `fetch_latest_dashboard_version()` is dead code (never called) (`mod.rs:467`)
- `version_update_api()` permanently returns `success:false` (`mod.rs:442`)

### CLI Commands
- Duplicate Tokio runtime creation per dispatch arm (`mod.rs:610+`)
- `db_snapshot()` creates throwaway runtime + unwrap/expect (`mod.rs:1924-1930`)
- Key rotation generates random UUID instead of actual machine ID (`key_ext.rs:98-116`)
- `process_alive` PID reuse race (`server.rs:58-69`)
- `coerce_value_str` auto-type coercion produces wrong types (`settings.rs:430-450`)
- Wildcard matching O(n*m) per request (`payload_rules.rs:248-272`)
- Auth headers sent in clear over HTTP to remote server (`runtime.rs:287-299`)

### Media
- **SSE parsers inconsistent** — 3 different implementations with different assumptions (`codex.rs:96-98`, `stream_to_json.rs:97,28`)
- Cloudflare image: duplicate `steps` fields and both `image_b64` + `image` sent simultaneously (`cloudflare_ai.rs`)
- Codex image: `prompt_cache_key` is a fresh UUID every request (defeats caching) (`codex.rs:271`)
- Nanobanana typo: `"IMAGETOIAMGE"` misspelling (`nanobanana.rs:65`)
- ComfyUI sends `{"prompt": prompt}` instead of graph workflow — doesn't work (`comfyui.rs:33-34`)
- Image handler claims 401 retry-after-refresh in doc but never implements it (`handler.rs:4-6 vs 92-105`)
- Codex image: `instructions`, `reasoning`, `store` hardcoded/ignored (`codex.rs:262-274`)
- Search: URL encoding failures silently produce empty query strings (`search/providers.rs:171+`)
- Binary output fetches arbitrary URLs from upstream response data (SSRF-like) (`handler.rs:143-154`)
- Duplicate `parse_model_voice()` and `pcm_to_wav()` in Gemini TTS (`gemini.rs:22-36,38-58`)
- ElevenLabs voice settings hardcoded (`elevenlabs.rs:47`)
- Deepgram `smart_format`/`punctuate` forced to true (`stt/mod.rs:198-199`)
- AssemblyAI upload URL hardcoded regardless of `base_url` (`stt/mod.rs:247`)
- RunwayML uses `api.dev.runwayml.com` (dev endpoint, not production) (`runwayml.rs:13`)
- Mistyped provider ID for `cerabras` vs `cerebras` in executor configs
- Codex version strings hardcoded (`codex.rs:20-24`) — will become stale
- Embeddings: GitHub Models and NVIDIA endpoints may be wrong (`embeddings/base.rs:97,101`)
- Tags `Box::leak` + outer `json!()` — memory leak on every request (`tags.rs:48`)

### Core
- ThinkingSuffix may inject into non-streaming responses incorrectly (`utils/thinking_suffix.rs:789L`)
- Session manager has no crash recovery (`utils/session_manager.rs`)
- Circuit breaker lacks half-open state — tripped breaker stays open (`circuit_breaker.rs`)
- Quota fetcher races on concurrent refresh (`usage/quota_fetcher.rs`)
- Response cache eviction is O(n log n) on every `set()` (`cache/mod.rs:233-256`)
- Usage tracking: type mismatch `Vec` vs `VecDeque` in request_logger (`request_logger.rs:31-39`)
- `infer_provider_from_model_name` defaults unknown models to "openai" — wrong routing for 20+ model families (`model/mod.rs:285-303`)
- Prompt injection regex ReDoS potential (22-alternation + bounded quantifiers) (`guardrails/mod.rs:88-109`)
- PII guardrail only covers US-centric patterns (`guardrails/mod.rs:180-201`)
- `a2a.rs` hardcodes `0.0.0.0:4623` in agent card URLs (`a2a.rs:38,44`)

---

## 🟢 LOW (30+ issues — highlights)

- Missing pagination/filtering/sorting on list commands across CLI
- Inconsistent `--robot` output shapes across commands
- 3-4 different JSON error envelopes across the API surface
- ~60 duplicate `/v1/v1/...` routes for backward compatibility
- ComfyUI is a stub (39 lines, no real workflow)
- Project ID cache grows unbounded
- 20+ instances of `unwrap()`/`expect()` in production hot paths
- OAuth error messages potentially reveal too much
- Backup shrink-detection suppresses legitimate deletions
- Auto-resolves API key from local DB without warning in remote mode
- Unbounded stdin reads (`runtime.rs:539-545`)
- Pattern of `emit_error` + `std::process::exit` bypasses RAII destructors
- Inconsistent `op-` vs `sk-` key prefix formats
- `NODE_ENV` in Rust binary (`shutdown.rs`)
- Codex shell profile mutations hardcoded to bash/zsh (misses fish)
- Missing `#[test]` attribute on 5 functions
- Embeddings: Gemini usage always reported as 0 tokens
- Size-to-aspect-ratio covers only 5 sizes

---

## Cross-Cutting Observations

1. **Security is layered but porous**: Route-tier auth (PUBLIC/PROTECTED/LOCAL_ONLY/ADMIN) is well-designed, but the "remaining" handler group has no middleware enforcement. The A2A endpoint is the most critical unguarded path — it leaks ALL provider credentials.

2. **Hardcoded secrets everywhere**: 3 OAuth client secrets in source, 1 default HMAC secret, default password "123456". The hardcoded approach to credentials suggests the 9router origins were never hardened for security.

3. **MCP is powerful but dangerously exposed**: The embedded MCP server can do anything (create/delete providers, keys, combos) with zero authentication. This is a production blocker.

4. **Full DB rewrite on every config change**: The `update()` function serializes all data to JSON, deletes all SQLite rows, and reinserts them. This defeats SQLite's incremental advantages and will be slow at scale.

5. **Media subsystem has clean architecture but orphaned code**: Duplicated STT module, missing HTTP timeouts, inconsistent SSE parsing, and ComfyUI stub suggest the media subsystem was built rapidly without consolidation.

6. **Combo routing has critical logic bugs**: `check_fallback_error` never says "don't fall back", shadow tasks leak, upstream error details are discarded. The combo system wastes API quota on 400/401 errors.

7. **Test coverage gap**: Zero per-adapter tests for image/TTS adapters, 5 test functions missing `#[test]`, no integration tests for combo fallback logic.

8. **Code quality signals**: Type mismatch (Vec vs VecDeque) that should fail compilation, dead variables with `_` prefix, Node.js conventions in Rust code, Box::leak on tags endpoint.

---

## Priority Recommendations

### 🔴 Fix Immediately
1. Move all 3 hardcoded OAuth secrets to env vars (C2-C4)
2. Add auth to A2A endpoint and credential redaction (C5)
3. Add auth to all OAuth import routes (C6)
4. Fix `check_fallback_error` to handle permanent errors (C7)
5. Generate random `API_KEY_SECRET` at startup (C1)

### 🟠 Fix This Week
6. Replace default password "123456" with random generation (H1)
7. Support ES256/EdDSA in OIDC (H6)
8. Add auth to MCP operations (H24)
9. Add auth to `get_tts_voices()` (H13)
10. Convert DB `update()` to incremental SQL (H19)
11. Upgrade to AES-256-GCM with Argon2id (H20)
12. Add token refresh concurrency protection (H28)
13. Remove header-only files that shadow real ones (H18)
14. Add HTTP request timeouts to all media adapters (H31)
15. Fix OpenRouter TTS model/voice parsing (H30)

### 🟡 Fix Next Release
16. Consolidate duplicated STT module
17. Add cancellation to polling loops
18. Standardize error envelopes across API
19. Implement half-open state in circuit breaker
20. Add pagination to all list commands
21. Add per-image/TTS adapter tests
22. Fix `BackoffLevel` hardcoded to 0 (H21)
23. Fix SSE parsers inconsistency
24. Normalize `--robot` output shapes
25. Expand `infer_provider_from_model_name` coverage
26. Implement migration framework for SQLite
27. Fix Cloudflare image adapter (duplicate/simultaneous fields)
28. Remove dead CLI tool settings files (H18)

### 📋 Security Hardening Checklist
- [ ] Replace hardcoded OAuth secrets with env vars
- [ ] Generate random default for `API_KEY_SECRET`
- [ ] Auth for all OAuth import routes
- [ ] Auth for A2A endpoint + credential redaction
- [ ] Auth for MCP operations
- [ ] Auth for `get_tts_voices()`
- [ ] Auth for `models_availability.clear_cooldown`
- [ ] Fix CORS spec violation (credentials + wildcard)
- [ ] Add constant-time comparison to all auth paths
- [ ] Support ES256/EdDSA in OIDC
- [ ] Remove `expect()`/`unwrap()` from production paths
- [ ] Fix LOCAL_ONLY guard fallback to 127.0.0.1
- [ ] Add HTTP request timeouts to all media adapters
- [ ] Hardening: AES-256-GCM + Argon2id for DB crypto
- [ ] Hardening: Unbounded JTI growth → periodic cleanup

---

*Generated by Root orchestration — 4 Herdr panes + 4 deep-dive agents, ~150K lines analyzed, 7 findings files consolidated.*
