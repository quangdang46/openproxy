# Master Plan: Sync openproxy with 9router v0.4.59 → v0.4.66

**Created**: 2026-06-06
**Status**: Ready to Execute
**Estimated Effort**: 3-5 days

---

## Executive Summary

The openproxy Rust project is **28 commits behind** the 9router JavaScript upstream (v0.4.59 vs v0.4.66). This plan covers four phases: model snapshot sync, critical bug fix porting, new feature implementation, and testing/validation.

---

## Phase 1: Sync Model Snapshot (9router.json)

**Objective**: Update embedded model catalog from v0.4.59 to v0.4.66

**Command**:
```bash
cd /tmp/9router-upstream && git pull
cd /Users/tranquangdang21/Projects/openproxy
node scripts/sync/normalize-sources.mjs --only=9router --src-9router=/tmp/9router-upstream
```

**New Models to Sync**:

| Provider | Alias | Changes |
|----------|-------|---------|
| Claude Code | `cc` | Claude Opus 4.8 added |
| Codex | `cx` | GPT 5.4 Mini added; old 5.0/5.1/5.2 removed |
| Qoder | `qd` | **NEW PROVIDER** - 11 models |
| Antigravity | `ag` | Restructured models |
| Vertex | `vertex` | Grok-4, Perplexity, Qwen/GLM |
| AliCode | `alicode` | DeepSeek-V4, GLM-5.1, Kimi-K2.6 |
| SiliconFlow | `siliconflow` | MiMo v2/v2.5 |
| KiloCode | `kc` | GLM-5, Kimi K2.5/K2.6 |
| GitHub | `gh` | GPT 5.4 Mini, Claude Opus 4.7 |

**Risk**: Low (mechanical data sync)

---

## Phase 2: Port Critical Bug Fixes

### 2A: json_schema Fallback (#1343)

**Problem**: OpenAI-compatible providers without Structured Output support fail on `json_schema` requests

**Solution**: Inject schema into system prompt, downgrade to `json_object`

**Files to Modify**:
- `src/core/executor/default.rs` - Add `apply_json_schema_fallback()` method

**Logic**:
```rust
fn apply_json_schema_fallback(&self, provider: &str, body: &mut Value) {
    if !provider.starts_with("openai-compatible-") { return; }

    if let Some(schema) = body["response_format"]["json_schema"]["schema"].take() {
        // Inject schema instructions into system message
        let prompt = format!("You must respond with JSON matching this schema:\n{}", schema);

        // Find or create system message
        let messages = body["messages"].as_array_mut().unwrap();
        if let Some(sys) = messages.iter_mut().find(|m| m["role"] == "system") {
            sys["content"] = format!("{}\n\n{}", sys["content"], prompt).into();
        } else {
            messages.insert(0, json!({"role": "system", "content": prompt}));
        }

        // Downgrade response_format
        body["response_format"] = json!({"type": "json_object"});
    }
}
```

**Risk**: Low

---

### 2B: Read Tool Arg Sanitization (#1144, #1354)

**Problem**: Non-Anthropic models emit empty `pages: ""` causing Claude Code to reject

**Solution**: Sanitize tool arguments in response translator

**Files to Modify**:
- `src/core/translator/response/openai_to_claude.rs`

**Functions to Add**:
```rust
fn sanitize_tool_args(tool_name: &str, args_json: &str) -> String {
    // Parse JSON, return as-is if invalid
    // Strip proxy_ prefix from tool name
    // If tool == "Read", call sanitize_read_args()
    // Serialize back
}

fn sanitize_read_args(args: &mut Map<String, Value>) {
    // Coerce string limit/offset to numbers
    // Clamp limit to 1-2000
    // Clamp offset to >= 0
    // Remove pages if not valid PDF
}

fn is_valid_pdf_pages_arg(file_path: Option<&str>, pages: Option<&str>) -> bool {
    // file_path ends with .pdf (case insensitive)
    // pages matches \d+(-\d+)?
}
```

**Risk**: Low-Medium (must handle partial JSON in streaming)

---

## Phase 3: New Features

### 3A: Qoder Provider with COSY Signing (HIGH EFFORT)

**Problem**: Current Qoder executor uses outdated HMAC signing; upstream switched to COSY (RSA+AES+MD5)

**Upstream Changes**:
- New endpoint: `https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation`
- COSY signing: RSA-OAEP wraps AES key, AES-128-CBC encrypts payload, MD5 signs
- 17 `Cosy-*` headers per request

**Files to Modify**:
- `src/core/executor/qoder.rs` - Major rewrite
- `Cargo.toml` - Add dependencies: `aes`, `rsa`, `md-5`

**Implementation**:
```rust
// Generate random AES key
let aes_key = generate_random_aes_key();

// Encrypt user info with AES-128-CBC
let encrypted_payload = aes_cbc_encrypt(&aes_key, &user_info_json)?;

// Wrap AES key with RSA-OAEP
let wrapped_key = rsa_oaep_encrypt(&RSA_PUBLIC_KEY, &aes_key)?;

// Compute MD5 signature
let signature = md5_hash(&[&encrypted_payload, &wrapped_key, &timestamp, &body_hash, &sigPath].concat());

// Set headers
headers.insert("Cosy-Encryptkey", wrapped_key);
headers.insert("Cosy-Signature", signature);
headers.insert("Cosy-Timestamp", timestamp);
// ... 14 more headers
```

**Dependencies**: Phase 1 (model snapshot must include Qoder models)

**Risk**: HIGH - Complex cryptographic implementation

---

### 3B: Cloudflare Workers / Deno Deploy (DEFERRED)

**Reason**: Dashboard-only features requiring significant frontend work to adapt from Next.js to Astro

**Recommendation**: Defer to future release

---

## Phase 4: Testing & Validation

### Automated Tests
```bash
cargo test --all          # Rust backend
cd web && pnpm test       # Frontend
```

### Manual Verification
- [ ] New models appear in model list (claude-opus-4-8, gpt-5.4-mini, qoder models)
- [ ] json_schema fallback works with OpenAI-compat provider
- [ ] Read tool with empty pages="" doesn't cause errors
- [ ] Qoder OAuth + COSY signing works (if Phase 3A completed)

---

## Implementation Order

```
┌─────────────────────────────────────────────────────────────┐
│  Phase 1: Model Snapshot Sync                               │
│  └─ node scripts/sync/normalize-sources.mjs                 │
└─────────────────────────┬───────────────────────────────────┘
                          │
            ┌─────────────┴─────────────┐
            ▼                           ▼
┌───────────────────────┐   ┌───────────────────────┐
│ Phase 2A: json_schema │   │ Phase 2B: Read tool   │
│ fallback              │   │ sanitization          │
│ (default.rs)          │   │ (openai_to_claude.rs) │
└───────────┬───────────┘   └───────────┬───────────┘
            │                           │
            └─────────────┬─────────────┘
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 3A: Qoder COSY Signing                                │
│ └─ qoder.rs rewrite + new crypto deps                       │
└─────────────────────────┬───────────────────────────────────┘
                          ▼
┌─────────────────────────────────────────────────────────────┐
│ Phase 4: Testing & Validation                               │
└─────────────────────────────────────────────────────────────┘
```

---

## Commit Strategy

1. `sync: update 9router model snapshot to v0.4.66`
2. `fix: add json_schema fallback for openai-compatible providers`
3. `fix: sanitize Read tool args in OpenAI-to-Claude translator`
4. `feat(qoder): port COSY signing from upstream`

---

## Risk Matrix

| Risk | Severity | Mitigation |
|------|----------|------------|
| COSY signing errors | 🔴 High | Test against live API; compare with JS output |
| AES/RSA dep conflicts | 🟡 Medium | Use `ring` crate for AES; `rsa` is stable |
| Partial JSON in streaming | 🟢 Low | Catch serde errors, return as-is |
| Model sync unexpected output | 🟢 Low | Diff JSON before committing |

---

## Critical Files

| File | Phase | Action |
|------|-------|--------|
| `src/core/model/sources/9router.json` | 1 | Auto-generated by sync script |
| `src/core/executor/default.rs` | 2A | Add json_schema fallback |
| `src/core/translator/response/openai_to_claude.rs` | 2B | Add tool arg sanitization |
| `src/core/executor/qoder.rs` | 3A | Rewrite with COSY signing |
| `Cargo.toml` | 3A | Add crypto dependencies |
| `scripts/sync/normalize-sources.mjs` | 1 | Run to regenerate snapshot |

---

## Quick Start

```bash
# Phase 1: Sync models
cd /tmp/9router-upstream && git pull
cd /Users/tranquangdang21/Projects/openproxy
node scripts/sync/normalize-sources.mjs --only=9router --src-9router=/tmp/9router-upstream

# Verify changes
git diff src/core/model/sources/9router.json | head -100

# Phase 2: Implement bug fixes (manual edits)

# Phase 4: Test
cargo test --all
```

---

**Next Step**: Execute Phase 1 (model sync) immediately
