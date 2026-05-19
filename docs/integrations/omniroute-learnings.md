# OmniRoute — architectural learnings for OpenProxy

Analysis of [diegosouzapw/OmniRoute](https://github.com/diegosouzapw/OmniRoute)
(v3.8.0, ~120 providers) performed 2026-05-19.

## Patterns worth adopting

### 1. Single-source provider registry

OmniRoute's `open-sse/config/providerRegistry.ts` is the sole definition
for every provider. Adding a provider means adding one entry — the alias
map, model list, executor selection, and dashboard metadata are all
derived automatically via `generateModels()`, `generateAliasMap()`, and
`generateLegacyProviders()`.

**OpenProxy status:** We have three semi-independent sources of truth:
`provider_catalog.json` (alias map + static models), the Rust executor
registries in `src/core/executor/{default,provider}.rs`, and the Astro
dashboard constants in `web/`. The `openproxy sync` command added in this
PR is a first step toward consolidation — synced snapshots feed
`customModels` automatically instead of requiring manual edits in all
three places.

**Follow-up:** Unify the Rust executor `PROVIDER_CONFIGS` / `PROVIDER_REGISTRY`
maps and the JSON catalog into a single YAML or JSON source that the
build script ingests. Dashboard should read the catalog JSON at runtime.

### 2. LKGP — Last Known Good Provider

Combo routing in OmniRoute remembers the exact `connectionId` (not just
the provider name) that last succeeded for a model + user. On the next
request it tries that connection first, falling back to provider-level
selection only if the connection is gone or cooldown-expired.

**Benefit:** Reduces thrash when a single account is cooled or rate-limited
but other accounts for the same provider work fine.

**OpenProxy status:** Combos currently select the "first non-cooling
provider" without connection affinity. Adding LKGP would require a small
per-combo LRU in `combo_dispatch`.

### 3. Per-window quota cutoffs (cascade)

OmniRoute supports per-quota-window thresholds — e.g. session=95%,
weekly=80% — with a cascade from connection → provider default → global
98%. The gate is zero-latency when nothing is configured.

**OpenProxy status:** Quota Tracker (PR #51) implements a single threshold.
The cascade pattern is additive; the `QuotaEntry` struct just needs
optional per-window overrides.

### 4. New providers

| Provider | Type | Notes |
|---|---|---|
| **GitHub Models** (`ghm`) | free, apikey | GPT-4.1/4o/o1/o3/o4-mini, DeepSeek R1, Llama 4, Grok 3, Mistral Medium 3, embeddings. Auth via GitHub PAT. |
| **Hackclub AI** (`hc`) | free, optional apikey | 30+ passthrough models. No credit card. |
| **Microsoft Copilot Web** | WebSocket | Translates OpenAI chat → Copilot proprietary WS protocol. Per-token session pool. Complex — low priority for porting. |

GitHub Models and Hackclub AI have been added to `provider_catalog.json`
and the executor registries in this PR. They reuse the OpenAI-compatible
`DefaultExecutor` path.

### 5. Modular per-provider config

OmniRoute splits provider-specific knobs into dedicated files:
`anthropicHeaders.ts`, `codexClient.ts`, `glmProvider.ts`, etc.

**OpenProxy status:** We already follow this pattern with
`src/core/executor/<provider>.rs` per specialty provider. The
`DefaultExecutor` uses a `PROVIDER_CONFIGS` map for the bulk of
generic OpenAI-compatible providers. No action needed.

## Out of scope (potential future work)

- **Gamification & leaderboard** — fun but low ROI for a routing gateway.
- **Feature triage workflow** (`feature-triage.mjs`) — interesting CI
  pattern for managing community feature requests but specific to
  OmniRoute's GitHub workflow.
- **i18n (42 languages)** — OpenProxy serves a smaller audience; the
  dashboard currently only targets English and Vietnamese.
