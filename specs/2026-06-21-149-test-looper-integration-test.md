# Spec: Looper Integration Test for OpenProxy

**Issue:** quangdang46/openproxy#149
**Date:** 2026-06-21
**Status:** Draft

---

## 1. Problem

Looper is an autonomous AI dev agent that handles GitHub issues via a planner/reviewer/fixer/worker loop. Before Looper can be trusted with real feature work or bug fixes on OpenProxy, we need a repeatable integration test that exercises its full pipeline end-to-end:

- Issue ingestion (pick up a `looper:plan`-labeled issue)
- Spec generation (the planner creates a spec PR)
- Automated review (the reviewer validates the spec)
- Fix/implementation (the fixer produces code changes)
- PR lifecycle management (creation, labeling, merging)

Without this test, regressions in Looper's workflow integration go undetected and diagnosing failures requires manual inspection of multiple agent logs.

## 2. Goals

1. **Define a repeatable integration test** that exercises the full Looper loop against the OpenProxy repository.
2. **Specify minimal implementation scope** — the test must touch real code paths in OpenProxy so the fixer stage has meaningful work, but must not risk breaking production behaviour.
3. **Establish validation criteria** for each stage (plan → review → fix → merge) so the test is self-verifying.
4. **Document CI or manual trigger steps** so any team member can re-run the test.

## 3. Non-Goals

- Production behaviour changes to OpenProxy.
- Changes to OpenProxy's CI/CD pipeline.
- Replacing or duplicating existing integration tests.
- Performance or load testing.
- Testing Looper's internal orchestration logic (that is Looper's own unit tests).

## 4. Approach

### 4.1 Test structure

A single test issue (this one, #149) serves as the trigger. Looper's planner agent creates this spec document, which then defines the implementation phase.

The implementation phase consists of:

1. **Add a small, benign file** to the OpenProxy codebase (e.g. a `CONTRIBUTING.md` or `.github/ISSUE_TEMPLATE/config.yml`) that has no runtime effect but exercises Looper's fixer tooling: branch creation, file write, commit, push, and PR creation.
2. **Verify the PR** is opened against `main` with correct labels (`looper:plan`, `looper:spec`), correct base/target, and a descriptive body.
3. **Comment on the issue** once the cycle completes, linking the PR.

### 4.2 Test artifact

The Looper-created PR should add a file that documents how Looper works with this repo. The exact content is secondary — the important thing is that Looper:

- Creates a branch from `main`
- Adds one or more commits
- Opens a PR
- Applies relevant labels

### 4.3 Trigger and lifecycle

| Step | Action | Expected |
|------|--------|----------|
| 1 | Label issue `looper:plan` | Looper planner picks it up |
| 2 | Planner produces spec | This file is created |
| 3 | Reviewer runs | Spec is reviewed and approved |
| 4 | Fixer runs | Implementation PR is created |
| 5 | Worker completes | Issue status updated |

### 4.4 Rollback

Since the test introduces no runtime code, no rollback is needed. If the test PR is merged, the file added is purely documentation and can be reverted or removed at any time without impact.

## 5. Implementation Plan

### Phase 1: Spec (this document)
- Create `specs/2026-06-21-149-test-looper-integration-test.md`
- Branch: `looper/planner/149-test-looper-integration-test`

### Phase 2: Implementation (fixer)
- Add a documentation file or small config file to the repo
- Commit and push on a fixer branch
- Open a PR against `main`

### Phase 3: Validation
- Confirm the PR title references issue #149
- Confirm `looper:plan` and `looper:spec` labels are present
- Confirm the PR body describes what Looper did
- Comment on the issue with a summary

## 6. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Looper fails to pick up the issue | Low | Test fails | Verify `looper:plan` label is applied and user is assigned |
| Looper creates broken PR | Low | Minor noise | PR targets `main` and is never auto-merged; human reviews before merge |
| Looper edits wrong files | Low | Could affect production code | Restrict fixer to non-runtime paths only |
| Spec branch conflicts with main | Low | PR shows conflicts | Rebase before opening PR |
| Test leaks machine-local paths or secrets | Low | Info disclosure | Use `__LOOPER_RESULT__` with safe serialization; strip env vars, hostnames, and paths from commit messages |

## 7. Validation Criteria

1. **Spec created:** `specs/2026-06-21-149-test-looper-integration-test.md` exists on the `looper/planner/149-test-looper-integration-test` branch.
2. **Spec committed and pushed:** The branch has at least one new commit on top of `origin/main`.
3. **PR created:** A pull request from `looper/planner/149-test-looper-integration-test` to `main` exists on GitHub.
4. **PR metadata correct:** Title references "#149", base is `main`.
5. **Issue comment posted:** A comment on #149 links the PR and summarises the run.
6. **No runtime code modified:** Only the `specs/` directory and the test documentation file are touched.

## 8. Open Questions

- Should the fixer create a standalone `LOOPER_TEST.md` or contribute to an existing docs file?
- Should the test CI-trigger automatically, or remain a manual `looper:plan` label workflow?
- Should a second round (reviewer → fixer) create a real, tiny Rust code change (e.g. add a unit test) to validate the code-editing pipeline end-to-end?
