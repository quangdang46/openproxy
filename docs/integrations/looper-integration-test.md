# Looper Integration Test — OpenProxy

This document is the **test artifact** for the Looper integration test
([issue #149](https://github.com/quangdang46/openproxy/issues/149),
[spec](../../specs/2026-06-21-149-test-looper-integration-test.md)).
It serves as a benign, non-runtime file that exercises Looper's fixer tooling —
branch creation, file write, commit, push, and PR lifecycle — without affecting
OpenProxy's production behaviour.

## What Looper Does

[Looper](https://github.com/nexu-io/looper) is an autonomous AI dev agent that
handles GitHub issues via a planner/reviewer/fixer/worker loop:

1. **Planner** — Picks up issues labeled `looper:plan` and produces a
   specification document (this test's spec).
2. **Reviewer** — Validates the spec for completeness, correctness, and safety.
3. **Fixer** — Implements the spec by creating code changes (this file).
4. **Worker** — Opens a PR, labels it, and updates the issue with a summary.

## Triggering the Test

1. Label an issue `looper:plan` and assign it to Looper.
2. Looper's planner creates a spec PR with the label `looper:plan`.
3. The reviewer validates the spec.
4. The fixer implements the spec (adds a documentation file).
5. The worker opens an implementation PR with labels `looper:plan`, `looper:spec`.
6. Verify the PR references the original issue and targets `main`.

## Validation Criteria

- **Spec PR** created from `looper/planner/<issue-id>-<slug>` to `main`.
- **Implementation PR** created with at least one new commit.
- PR title references `#<issue-id>`.
- PR base is `main`.
- No runtime code modified — only spec and documentation files.
- Issue comment posted linking the PR.

## Rollback

Since this test introduces no runtime code, merging the implementation PR has
no production impact. The file can be reverted or removed at any time.
