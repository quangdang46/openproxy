<!--
Thank you for contributing to OpenProxy.
Fill in the sections below — delete the comments. Keep PRs ≤ ~400 lines where possible.
See CONTRIBUTING.md and docs/git-conventions.md for the full workflow.
-->

## Summary
<!-- What changed in 1–3 sentences. Link the bead/issue: Fixes #123 or beads: openproxy-xyz -->

## Test plan
<!-- Exact commands + evidence. CI runs the same — show it was green locally. -->
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] `cargo test -p openproxy --lib provider_models` / `./scripts/dev.sh --full`
- [ ] `pnpm --dir web run build` (if dashboard touched)
- [ ] Manual: `curl -sf http://127.0.0.1:4623/health` → `{"ok":true}` / `openproxy --robot doctor`

Evidence:
```
<paste relevant output>
```

## Risk & rollback
<!-- Safe to revert? Needs migration? Additive schema change only? -->

## Checklist
- [ ] Branch: `<type>/<kebab-description>` (Conventional Commits, atomic commits)
- [ ] `git status` / `git diff --cached` reviewed — no secrets (`opencode.json`, `.env`, `*.pem`, `sk-`, `Bearer`)
- [ ] Docs updated if workflow/architecture/parity changed (`AGENTS.md`, `CONTRIBUTING.md`, `docs/parity-9router.md`)
- [ ] Linked issue/bead; added `Fixes #…` if it closes an issue
