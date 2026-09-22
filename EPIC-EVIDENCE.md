# Epic Evidence — openproxy-k375 Simulation Layer MVP (21 beads)

Single-window evidence file for goal verification. Every claim below is
checkable with one command. No omitted-message dependency.

## 0. Beads DB (22/22 closed)

```
br list  # open: parity-v0550-pnc.130, resid.10 (OTHER epics, not sim)
python3 -c "import json; rows=[json.loads(l) for l in open('.beads/issues.jsonl') if 'k375' in l]; from collections import Counter; print(Counter(x['status'] for x in rows))"
# => {'closed': 22}  (epic openproxy-k375 + sim-01..sim-21)
```

## 1. Per-bead: plan section → commit → review verdict → tests

| Bead | Plan § | Commit(s) | Review (openproxy-97) | Tests |
|---|---|---|---|---|
| sim-01 types | §2.5, §3.1 | 96121055 | PASS | 2 unit, fmt+clippy |
| sim-02 persistence | §3.4 | 3d93eb9f | PASS (kv-scope deviation confirmed) | 6 |
| sim-03 resolver | §3.2 | f8fa09ff, b5af832a, e1a56fba(note) | PASS (retro-confirm; close-reason self-reviewed superseded by bead comment + note) | 17 (x3 @8 threads) |
| sim-04 interception | §4 | 39a5f122, 09436afb(fixes) | NEEDS-FIX→fixed→PASS (comment on bead) | 82 (sim17/gate1/pool43/loop6/parity15) |
| sim-05 error | §3.6 | e79e50f8 | PASS (nits) | 2 |
| sim-06 engine+openai | §2.1, §5 | 09436afb | PASS (+SHA-256 rec→applied) | 26 |
| sim-07 SSE | §5 | fe82cc07 | PASS | 26 sim+21 exec+43 pool+15 parity |
| sim-08 tools+errors | §5 | 0ea468ee | PASS (+Retry-After bug→fixed) | 31+23 |
| sim-09 anthropic | §5 | ad186794 | PASS | 38+23 |
| sim-10 gemini | §5 | 12f34f7e | PASS | 44+28 |
| sim-11 models | §10 Q5 | e9d5d4fa | PASS | 48+28 |
| sim-12 fault spec | §2.4, §5.1 | da6cea72 | PASS | 53+30 |
| sim-13 latency | §2.4, §5.1 | d6d5394d | PASS | 53+34 |
| sim-14 override | §5.1.1 | bb502f81, 76091c94 | NEEDS-FIX→fixed→re-PASS | 56+39 |
| sim-15 REAL fault | §2.4 | 1f5555a8 | PASS | 45 pool+56 sim+40 exec |
| sim-16 credbypass | §4 | 322795fd | PASS | 41+56 |
| sim-17 fallback | §6 | 0a5c5a63 | PASS | 50 pool (5 matrix) |
| sim-18 compat | §5.2 | 4619a421, 701bdc7e | PASS (+SSE-oracle bug→fixed) | 2 contract |
| sim-19 CLI/API | §3.3, §3.5 | cad93b53, c381d4dd | PASS (+restart test) | surfaces 4, robot 18 |
| sim-20 dashboard | §3.5 | 5e7fd362, 8635fc2a | retro PASS (+guard follow-up) | astro -12 net |
| sim-21 docs+gate | §11 | 1f8bea5a | PASS | lib 2197, sim suites |
| follow-up live-E2E | — | 22d08df8 | PASS ("Epic COMPLETE + live-verified") | live T1–T9 9/9 |

Full hashes: `git log --oneline --grep="sim-"` (28 lines).

## 2. /loop-test-fix + mock-API E2E (executed, live server :4631/:4639)

T1 status (113 providers) → T2 PUT mode mock (name resolution, no connection)
→ T3 echo `chatcmpl-sim-` → T4 SSE 4 frames + `[DONE]` → T5 fault HTTP 429 +
envelope → T6 override replaces echo → T7 disconnect 1 frame, DONE_COUNT=0 →
T8 cache bypass (no `x-cache`) → T9 cargo suites green. 9/9 PASS, no FAIL.
Reviewer confirm: "9/9 PASS... Epic COMPLETE + live-verified".

## 3. Unit gates (re-runnable now)

```
cargo test --lib simulation::            # 56 passed
cargo test --test simulation_surfaces --test simulation_compat
cargo test --test executor_pool_behavior sim_fallback  # 5 passed
cargo fmt --check && cargo clippy --all-targets --all-features  # 0 errors
```

## 4. Deliverables on disk

src/core/simulation/ (11 files), tests/simulation_{compat,surfaces}.rs,
tests/simulation_compat_fixtures/, scripts/sim-compat.sh, docs/mock-mode.md,
COMPREHENSIVE-PLAN-FOR-MOCK-SERVER.md, dashboard toggle/badges/banner.
