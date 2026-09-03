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
Parity work: epic `openproxy-9router-parity-v0550-pnc` (9router v0.5.50 → openproxy, 122 specs) (+ children). Prior v0.5.30 epic `openproxy-9router-parity-mj1` is closed. See `br ready` / `bv --robot-next`.

## Key References
- `docs/parity-9router.md` — intentional divergences, pipeline order, executor dispatch
- 9router reference: `/tmp/9router` (open-sse) — do NOT copy JS bugs blindly
- **OmniRoute v3.8.50** (`/tmp/omniroute_v3850`, SHA `6cd4d38`) — authoritative reference for provider parity. When a provider behaves unexpectedly, compare against OmniRoute's `src/managed/` implementations, not the legacy JS `9router`. Key files: `credentialHealth.ts` (`HealthStatus`: 200/401/429/503/500), `healthCheck.ts`, `modelDiscovery.ts` (`discoverProviderModels` — GET `/v1/models` with provider-specific headers), `managedModelImport.ts` (merge/sync remote catalog → local config, `preserveRemovedCustomModelCompat`). Header-fidelity rules for parity: OpenRouter must send `HTTP-Referer` + `X-Title`; nvidia/llm7 omit `HTTP-Referer`; gemini sends `x-goog-api-key`; kiro/opencode/free-providers use the appropriate OAuth token flow. Always check `/tmp/omniroute_v3850` before making provider-specific decisions.

## Dev Workflow — backend + dashboard rebuild

Single smooth loop — backend and dashboard are **separate builds** served by the same binary:

```bash
./scripts/dev.sh              # incremental cargo build --bin openproxy + run on :4623 (foreground)
./scripts/dev.sh detach       # build + run detached
./scripts/dev.sh build        # only cargo build, don't run
```

**Dashboard is not live-reloaded.** `web/src` → `web/dist` (Astro) is what the Rust server serves.
After any `web/src` change you **must** rebuild the dashboard or the feature will be invisible
(past "feature not found" confusion was a missing rebuild, not a missing backend):

```bash
cd web && pnpm install        # once
pnpm build                    # rebuild web/dist after every web/src change
# or during iteration:
pnpm dev                      # Astro dev on :4624 (proxy API to :4623)
```

Full loop for a feature touching both layers:

```bash
./scripts/dev.sh build && (cd web && pnpm build) && ./scripts/dev.sh detach
curl -s http://127.0.0.1:4623/health
open http://127.0.0.1:4623/dashboard/providers
```

**After ANY backend change, you MUST rebuild the binary and restart the server so the user can test it directly.** The running server does not hot-reload Rust. A common failure is leaving a stale binary running (an earlier build predating your fix) while reporting "done" — the user then tests the old behavior. The mandatory loop for a Rust change:

```bash
./scripts/dev.sh build                 # compile the new binary
pkill -f 'target/.*/openproxy' || true # stop the stale server
./scripts/dev.sh detach                # start the freshly built binary
curl -s http://127.0.0.1:4623/health   # confirm it is up
```

If the change also touched `web/src`, run `pnpm build` before restarting so the dashboard reflects it. Never report a fix as "done" or "ready to test" without completing this rebuild+restart.

## Contributing & Git Hygiene

Systematic, not arbitrary — all contributions follow two documents linked from the intelligence brief:

- **Workflow & expectations:** [`CONTRIBUTING.md`](CONTRIBUTING.md) — prerequisites, `scripts/dev.sh` quick/full, project layout, coding standards, testing matrix, beads parity workflow, secrets policy, releases.
- **Enforceable git rules:** [`docs/git-conventions.md`](docs/git-conventions.md) — branch naming (`<type>/<kebab>`), Conventional Commits (`<type>(<scope>): <subject>`), atomic bisectable commits, verification before each commit (`cargo fmt --check` + `cargo clippy --all-targets --all-features`), history hygiene (rebase, no `git add .`), PR hygiene (template, ≤400 lines, CI `web` → `rust` must be green), issue/beads discipline, tagging.

PRs use [`.github/pull_request_template.md`](.github/pull_request_template.md); bugs/features use [`.github/ISSUE_TEMPLATE/`](.github/ISSUE_TEMPLATE/). CI (`.github/workflows/ci.yml`) enforces `web: astro check + build → rust: fmt + clippy + tests` on `ubuntu` + `macos`. The checklist in `docs/git-conventions.md` §10 is the gate — all green means systematic.

## Core Product Surfaces (TOP PRIORITY)

These 4 surfaces ARE the product. Everything else is optional. They must be flawless, reliable, and mutually consistent — always prioritize regressions and improvements here:

1. **Providers page** — `/dashboard/providers/<provider>` (e.g. kilocode): user controls Available Models (disable/enable/custom). Configuration is user data, persisted in SQLite — must survive binary rebuilds/updates.
2. **CLI tools config** — `/dashboard/cli-tools/opencode` (opencode is the primary client).
3. **Combos page** — `/dashboard/combos`.
4. **`web/src/shared/components/ModelSelectModal.tsx`** — the single model-picker used everywhere; must exactly mirror the provider page's Available Models (same disabled map + custom rows + catalog merge). Any change to model-list logic MUST be applied consistently to both the provider page and this modal.

Core workflow that must never break: configure provider → customize available models → create combos → select models for opencode CLI config.

## Status
Active parity port. Run `cargo test -p openproxy --lib parity_tests stream_flags` for smoke.

## Local Config & Secrets — Never Commit
- **Do not commit** local user config or secrets: `opencode.json`, `.env`, `.env.*`, `*.pem`, `~/.openproxy/db.json`, `~/.openproxy/admin.key`, API keys, `provider_specific_data` with live credentials, or any file containing `sk-`, `Bearer`, `refresh_token`.
- `opencode.json` is local agent config (model, MCP keys like `CONTEXT7_API_KEY`, permissions) — keep untracked. `scripts/dev.sh` builds locally; real secrets live in SQLite (`db.json` encrypted) + `OPENPROXY_API_KEY` env, not in git.
- Before `git add`/`commit`, run `git status` and `git diff --cached`; if a file contains secrets or is machine-local, `git restore --staged <file>` and add it to `.gitignore`. Prefer `git check-ignore -v <file>` to verify.
- If a secret is accidentally committed, rotate it immediately and purge history (`git filter-repo` or BFG) — do not just revert.

## Schema stability (`openproxy.v1.*`)

The `openproxy.v1.*` envelope namespace is a **frozen, additive-only contract**. Every JSON envelope emitted by `--robot` carries a `schema` field matching `openproxy.v1.<area>.<action>`. Existing fields keep their names, types, and meanings across releases. New fields are additive only — no renames or removals. A new `openproxy.v2.*` namespace will be opened before any breaking change.

Run `openproxy schema stability` to see the current stability promise:

```bash
openproxy --robot schema stability
# → {"schema":"openproxy.v1.schema.stability","data":{"namespace":"openproxy.v1","stability":"stable","policy":"..."}}
```

The `schema` subcommand provides four operations:

| Command | Purpose |
|---|---|
| `openproxy schema list` | List all resource kinds with schema and example support |
| `openproxy schema show <resource>` | Print JSON Schema for a resource (provider, key, combo, etc.) |
| `openproxy schema example <resource>` | Print an example payload for a resource |
| `openproxy schema stability` | Print the v1 namespace stability contract |

13 resources are covered: `provider`, `provider-node`, `combo`, `key`, `pool`, `settings`, `custom-model`, `model-alias`, `usage-event`, `log-event`, `chat-event`, `quota`, `oauth-status`. Each has both a schema and an example — enforced by tests.
