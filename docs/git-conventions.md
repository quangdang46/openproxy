# Git Conventions — OpenProxy

Systematic git hygiene keeps `main` bisectable, revertible, and reviewable. This is the enforceable complement to [`CONTRIBUTING.md`](../CONTRIBUTING.md). If in doubt, follow this doc — not habit.

> **Rule of thumb:** every commit should be independently understandable, buildable, and revertible. If you need `and` to describe a commit, split it.

---

## 1. Branching

### Naming

```
<type>/<short-kebab-description>
```

`<type>` mirrors the primary commit type on the branch:

| Type | When |
|------|------|
| `feat/` | New provider, endpoint, dashboard feature |
| `fix/` | Bug fix, parity gap, regression |
| `docs/` | Docs only |
| `chore/` | Tooling, CI, deps, `scripts/dev.sh` |
| `refactor/` | Behavior-preserving restructure |
| `test/` | Tests / snapshots only |
| `build/` | Build / packaging |
| `ci/` | GitHub Actions |
| `perf/` | Performance |

Examples from this repo (real):
- `fix/provider-parity-claude-scopes`
- `fix/oauth-secrets`
- `fix/zen-catalog`
- `add-nvidia-model-aliases`
- `pr-335` / `pr-374` (numbered PR branches — acceptable for small fixes)

Keep branches short-lived. Rebase onto `main` before opening a PR; don't merge `main` into the branch.

### Lifecycle

```bash
git checkout main && git pull
git checkout -b feat/kilocode-discovery
# ... atomic commits ...
git push -u origin feat/kilocode-discovery
gh pr create --fill   # or open via GitHub UI with the PR template
```

Delete the branch after merge. CI cancels in-flight runs on the same ref (`concurrency: cancel-in-progress: true`).

---

## 2. Commit Messages — Conventional Commits

### Format

```
<type>(<scope>): <imperative subject>

[body — what & why, 72-char wrapped]
[footer — Fixes #123, BREAKING CHANGE:, Co-authored-by:]
```

Rules:
- **Type** is lowercase, required: `feat`, `fix`, `docs`, `chore`, `test`, `refactor`, `perf`, `ci`, `build`, `revert`
- **Scope** is optional but recommended: `providers`, `dashboard`, `executor`, `api`, `agents`, `dev`, `catalog`, `auth`, etc. Lowercase, no spaces.
- **Subject** is imperative, lowercase, no trailing period, ≤ 72 chars: `support straightforward API key` not `Supported...`
- **Body** explains *why* if not obvious from the diff. Wrap at 72 chars.
- **Footer** for issue links and breaking changes.

### Real examples (from `git log`)

```
fix(kilocode): support straightforward API key (dual auth)
chore(dev): unify build+test+run into single scripts/dev.sh (quick default + auto-start)
fix(dashboard): show Added badge for untracked connections
feat(providers): add import catalog from live provider models
docs(agents): document dev workflow (cargo build + dashboard rebuild)
fix(kiro): lowercase regex consts to satisfy snake_case lint
```

Bad → Good:
- `fix bug` → `fix(executor): preserve upstream 502 status on retry exhaustion`
- `update providers` → `feat(providers): add kilocode to supports_models_discovery`
- `WIP` → (don't push WIP — see Atomic Commits below)

### Breaking changes

If the `openproxy.v1.*` JSON envelope changes in a non-additive way, or a CLI flag is removed, mark it:

```
feat(api)!: rename /api/providers/:id/import-models response field

BREAKING CHANGE: `imported` renamed to `added`; clients must update.
```

Add `!` after `type(scope)` and a `BREAKING CHANGE:` footer. This requires a `v2` schema namespace per `AGENTS.md`.

---

## 3. Atomic Commits — The Hard Rule

One logical change per commit. A commit is atomic if:

- It passes `cargo fmt --check` + `cargo clippy --all-targets --all-features` + relevant tests on its own
- It can be reverted without breaking the build
- Its message needs no `and`

```bash
# Good: three commits
feat(providers): add kilocode to supports_models_discovery
fix(executor): track last_gateway_status and return UpstreamStatus
docs(agents): describe import catalog provider list

# Bad: one blob
feat: add kilocode and fix executor and update docs
```

Stage intentionally — never `git add .` blindly:

```bash
git status
git diff
git add src/server/api/provider_models.rs web/src/shared/constants/providers.ts
git diff --cached   # verify only intended hunks are staged
git commit -m "feat(providers): add kilocode to supports_models_discovery"
```

If you touched secrets or machine-local files, **unstage before committing**:

```bash
git restore --staged opencode.json .env
git check-ignore -v opencode.json  # verify ignored
```

---

## 4. Verification Before Each Commit

Run the same checks CI runs — locally, every commit:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test -p openproxy --lib provider_models -- --nocapture   # quick smoke
# or: ./scripts/dev.sh --no-run   # build+test without starting server
```

Dashboard changes:

```bash
pnpm --dir web exec astro check   # typecheck (advisory but fix new errors)
pnpm --dir web run build          # must succeed — Rust embeds web/dist
```

If CI fails, fix it in a new commit — don't amend pushed history (see below).

---

## 5. History Hygiene

- **Before push:** rebase to keep history linear.
  ```bash
  git fetch origin
  git rebase origin/main
  # resolve conflicts, then:
  cargo fmt && cargo clippy && cargo test -p openproxy --lib provider_models
  git push --force-with-lease
  ```
- **After push:** don't amend/rebase public history. Fix with a new commit.
- **Squash?** Keep atomic commits on the branch; the maintainer may squash on merge — don't pre-squash into one blob unless the PR is truly a single change.
- **Fixup:** for local cleanup before push, use `git commit --fixup=<sha>` + `git rebase -i --autosquash`.

Never commit failing tests or WIP. Never bypass hooks with `--no-verify` unless you re-run checks immediately after.

---

## 6. Pull Requests

PRs use `.github/pull_request_template.md` — fill in:

- **Summary** — what changed, in 1–3 sentences
- **Test plan** — exact commands run + evidence (e.g. `curl -sf http://127.0.0.1:4623/health` → `{"ok":true}`)
- **Risk & rollback** — safe to revert? needs migration?
- **Linked issues/beads** — `Fixes #123` or `beads: openproxy-xyz`

Rules:

- Keep PLs ≤ ~400 lines where possible; split large work into stacked PRs.
- One concern per PR — don't mix refactors with features.
- CI must be green: `web` job (astro check+build+upload `web/dist`) → `rust` jobs (`cargo fmt --check` + `cargo clippy` + tests on `ubuntu` + `macos`).
- Request review; address comments with new commits (don't force-push review history away).
- Update docs (`AGENTS.md`, `CONTRIBUTING.md`, `docs/parity-9router.md`) when you change workflow, architecture, or parity behavior.

Draft PRs for early feedback: `gh pr create --draft`.

---

## 7. Issues & Beads

- **GitHub Issues:** search before creating (`gh search issues "kilocode"`). Use templates in `.github/ISSUE_TEMPLATE/` — Bug Report / Feature Request. Include repro steps, `openproxy --version`, and whether `./scripts/dev.sh --full` reproduces.
- **Beads (parity work):** epics like `openproxy-9router-parity-v0550-pnc` live in `br` / `bv --robot-next`. For provider parity, check OmniRoute (`/tmp/omniroute_v3850/src/managed/`) before filing — link the relevant file in the issue.

---

## 8. Releases & Tags

- Version lives in `Cargo.toml` (`0.2.0`). Tags `v*` trigger `.github/workflows/release.yml` (GHCR image + GitHub Release).
- Tag from green `main`:
  ```bash
  git checkout main && git pull
  cargo test -p openproxy --lib -- --test-threads=1  # or ./scripts/dev.sh --full
  git tag -a v0.2.1 -m "v0.2.1 — short changelog"
  git push origin v0.2.1
  ```
- `install.sh` / `install.ps1` pull from the same release — bump them together.

---

## 9. Secrets — The Non-Negotiable

- Never commit `opencode.json`, `.env*`, `*.pem`, `~/.openproxy/db.json`, `~/.openproxy/admin.key`, or any string matching `sk-`, `Bearer`, `refresh_token`.
- Before `git add`: `git status` → `git diff --cached` → `git restore --staged <file>` if needed → `git check-ignore -v <file>`.
- If a secret lands in history: **rotate it immediately** and purge with `git filter-repo` or BFG — don't just revert.

See `AGENTS.md` § Local Config & Secrets — Never Commit and `CONTRIBUTING.md` § Secrets.

---

## 10. Quick Checklist (copy into commit/PR)

```
- [ ] Branch name: <type>/<kebab-description>
- [ ] Commits: Conventional Commits, atomic, bisectable
- [ ] git status / git diff --cached reviewed — no secrets or stray files
- [ ] cargo fmt --check / cargo clippy --all-targets --all-features — green
- [ ] cargo test (quick or --full) — green
- [ ] pnpm --dir web run build — green (if web touched)
- [ ] PR template filled — summary, test plan, risk, linked issue/bead
```

When this list is all green, you are systematic — not arbitrary.
