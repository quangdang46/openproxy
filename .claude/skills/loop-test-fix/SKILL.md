---
name: loop-test-fix
description: End-to-end self-healing loop for openproxy — build+install locally, wire up a free opencode model, point codex at openproxy, smoke-test via `codex exec`, sweep every CLI subcommand for real coverage, run the repo's unit/integration tests, and if anything fails go back to research → fix → test → rebuild → reinstall until everything passes. Use when the user asks to "loop test fix openproxy", validate a local openproxy build end-to-end with codex/opencode, exercise "all CLI features/flags", or run the full research-fix-test-build-install cycle.
---

# openproxy loop-test-fix

Self-driving verification loop. No orchestration script — just follow these steps in order, each
turn, using your own judgment for the "research → fix" part when something breaks. Keep looping
until the exit condition is met or you're blocked and need the user.

## Loop body

1. **Build + install openproxy locally**
   - `cargo build --release` (or the project's usual release build).
   - Install/link the freshly built binary so it's the one on PATH / used by codex config
     (check `install.sh` / `update-openproxy.sh` for the project's own install convention —
     prefer those over ad-hoc copying).
   - If the build fails: that's a bug — go to step 5 (research → fix) before continuing.

2. **Set up the `opencode` provider (free/no-auth) — NOT `opencode-go` or `opencode-zen`**
   - openproxy has **three distinct providers** that all sound alike — do not conflate them:
     - `opencode` (alias `oc`) — the free, no-auth one. **This is the one to use for this loop.**
     - `opencode-go` — a separate provider, different base URL (`https://opencode.ai/zen/go/v1`).
     - `opencode-zen` — requires a real API key (confirmed 401 without one) — never treat this as
       "free".
     Using the wrong one invalidates the whole test — a response from `opencode-go`/`opencode-zen`
     does not prove the free no-auth path (`opencode`) works.
   - Before wiring anything up, verify `opencode` still has quota/isn't rate-limited (hit its
     `/v1/models` or do a tiny throwaway call). **Do not** fire the real smoke-test prompt blind —
     a burned-out free quota should be treated as "provider temporarily unavailable, retry later",
     not a failure of openproxy itself.
   - In openproxy's own config (provider setup / combo selection — see `AGENTS.md` dashboard
     workflow: configure provider → customize models → create combo → select for opencode CLI),
     make sure a combo backed by the `opencode` provider is what's selected/exposed.

3. **Point Codex at openproxy — via openproxy's own CLI, not manual config.toml edits**
   - openproxy runs locally on **port 4623** by default (`http://127.0.0.1:4623`).
   - Use openproxy's built-in CLI integration to write Codex's `config.toml` for you:
     ```
     openproxy tool apply codex --model "<combo/model id that resolves to the opencode provider>" \
       --endpoint http://127.0.0.1:4623
     ```
     - Omit `--endpoint` if the server is already running on the default port (4623) — it defaults
       to the running server's URL.
     - Use `--dry-run` first if you want to eyeball the JSON body before it POSTs.
     - `openproxy tool show codex` afterwards confirms what got saved.
   - If unsure which model id resolves to the `opencode` provider, check `openproxy tool run
     provider-list` / the model catalog before running `apply` — don't guess.
   - To undo/reset after testing: `openproxy tool revert codex`.

4. **THE critical gate: smoke test via codex exec**
   - This is the step that actually proves openproxy works end-to-end — everything before it is
     just setup.
   - Now that step 3's `tool apply codex` has written the config, run the plain command (it will
     use whatever `openproxy tool apply codex` just configured — no need to override model/provider
     inline):
     ```
     codex exec "Please list for me all available MCP servers"
     ```
   - **If it responds** with a plausible MCP server list (no error, no empty/garbled response, no
     auth/connection failure, and it actually went through openproxy on :4623 / the `opencode`
     provider — not silently fell back to something else) → **that's it, this gate is done.**
     Don't second-guess a clean response or keep re-running it.
   - If it fails (error, timeout, empty, wrong provider used, quota exhausted) → step 5.

5. **Full CLI feature sweep — literally every leaf subcommand and its documented flags, not a sample**
   - The MCP smoke test in step 4 only proves the `/v1/chat|responses` path works. It says
     nothing about the rest of the CLI surface. "Test all features" means all — picking one
     representative call per top-level group is NOT sufficient and does not satisfy this step.
   - **Build the exhaustive checklist first, mechanically, before running anything:**
     1. `openproxy --help` → list every top-level group.
     2. For every group, `openproxy <group> --help` → list every subcommand in it.
     3. For every subcommand that itself groups further (e.g. `media providers`, `mitm cert`,
        `db cloud`, `translator preset`), recurse: `openproxy <group> <sub> --help` until you hit
        actual leaves with no further subcommands.
     4. For every leaf, `openproxy <group> [<sub>...] <leaf> --help` → note every flag/option it
        takes, required or not.
     - Write this checklist down (a scratch file or your own running notes) as you discover it —
       don't hardcode a fixed command list in this skill file, and don't rely on memory of a
       previous run; the CLI surface can grow between runs and a stale mental list will silently
       skip new commands.
   - **Then execute every single leaf** against the locally running server from step 1, in
     `--robot` mode, and confirm each returns a real, well-formed envelope — not just that the
     binary parses the flags. For a leaf with multiple meaningfully different flag combinations
     (e.g. `models list` vs `models list --kind image`, `db dump <resource>` for each resource
     kind, `sync nine-router` plain vs `--dry-run` vs `--prune`), exercise each combination that
     changes behavior — not just the bare invocation with defaults.
   - Prefer non-destructive/idempotent calls (`list`/`get`/`show`/`status`/`--dry-run`) where a
     leaf offers one. For anything that mutates state (`create`, `apply`, `delete`, `set`), use a
     throwaway resource and round-trip it (`create` → `get`/`list` confirms it → `delete`) so nothing
     is left behind, or `apply` → capture the result → `revert`/restore the prior value.
   - Track results in a table as you go — id, exact command run, result (pass/fail) or reason for
     skip — rather than trusting memory. You need this table intact, complete, and covering every
     leaf discovered in the enumeration pass, for the exit-condition report in step 8. A partial
     table (e.g. "checked provider, key, combo" while `media`, `mitm`, `translator`, `sync`,
     `pxpipe`, `db`, etc. are unaddressed) does not satisfy this step — go back and finish it.
   - Any subcommand that errors, hangs, or returns a malformed/empty envelope is a bug — go to
     step 6. A subcommand that genuinely requires interactive/browser setup (OAuth device flows,
     cert-generation prompts, a media provider you don't have credentials for) is not a failure —
     record it in the table as "requires manual setup, not exercised" with the specific reason;
     don't fabricate a pass for it, and don't silently drop it from the table either.

6. **Research → fix (only when something broke)**
   - Diagnose using the actual error (build error, codex error, CLI error, wrong/missing/malformed
     response).
   - Check `parity-report.md`, `docs/`, and recent commits for related known issues before
     assuming it's novel.
   - Apply the fix in `src/`.
   - Go back to step 1 (rebuild → reinstall) — don't skip ahead, and re-run the full step-5 sweep
     afterward, not just the one subcommand that broke (a fix can regress a sibling command).

7. **Run the existing test suite**
   - Once the smoke test and CLI sweep both pass, run the repo's unit/integration tests:
     `cargo test` (add `--release` if that's how CI runs them — check for a CI workflow to match).
   - If anything fails, that's a real regression: go to step 6, fix it, then repeat from step 1.

8. **Exit condition**
   - Loop ends only when: the smoke test in step 4 passes, **every** subcommand exercised in
     step 5 is accounted for (pass, or explicitly noted as requiring manual/environment setup —
     never silently omitted), and `cargo test` in step 7 is fully green.
   - Report what was fixed along the way, plus the full step-5 pass/fail/skipped table, and stop.
   - If you get stuck twice in a row on the same failure with no new lead, stop and ask the user
     instead of spinning.

## Notes

- This is meant to be invoked as a normal skill turn (optionally under `/loop` if the user wants
  it to keep re-firing on a schedule) — there is no separate script to maintain here.
- Never mark the loop "done" on a green smoke test alone — the step 5 CLI sweep and the step 7
  full test suite must also pass (or be accounted for) before declaring success.
- Leave the machine as you found it: revert `openproxy tool apply codex` (and any other CLI-tool
  integration you applied) and delete any throwaway combos/keys/pools created for the sweep once
  verification is done, unless the user asked you to leave them in place.
