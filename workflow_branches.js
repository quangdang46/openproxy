export const meta = {
  name: '9router-parity-branches',
  description: 'Create 24 branches + PRs for the 9router parity plan, one per issue',
  phases: [
    { title: 'Setup', detail: 'Branch strategy & base' },
    { title: 'Phase1-Auth', detail: 'P1.1-P1.5 Auth per model' },
    { title: 'Phase2-Executors', detail: 'P2.1-P2.4 Upstream executors' },
    { title: 'Phase3-CLI', detail: 'P3.1-P3.9 CLI setup' },
    { title: 'Phase4-Translator', detail: 'P4.1-P4.2 Translator gaps' },
    { title: 'Phase5-E2E', detail: 'P5 + checklists' },
  ],
}

// Mapping: branch name -> issue title -> PR description (short)
const BRANCHES = [
  { name: 'p0-test-infra',         issue: '78',  title: '[P0] Test infrastructure',                    phase: 'Setup' },
  { name: 'p1.1-oauth-constants',  issue: '88',  title: '[P1.1] OAuth provider constants rewrite',      phase: 'Phase1-Auth' },
  { name: 'p1.2-missing-oauth',   issue: '89',  title: '[P1.2] Add 5 missing OAuth services',           phase: 'Phase1-Auth' },
  { name: 'p1.3-kiro-5-method',   issue: '90',  title: '[P1.3] Kiro 5-method refactor',                phase: 'Phase1-Auth' },
  { name: 'p1.4-token-refresh',    issue: '91',  title: '[P1.4] Token refresh dedup + lead times',       phase: 'Phase1-Auth' },
  { name: 'p1.5-cursor-checksum',  issue: '92',  title: '[P1.5] Cursor import + jyh checksum',           phase: 'Phase1-Auth' },
  { name: 'p2.1-kiro-codewhisperer', issue: '93', title: '[P2.1] Kiro executor → CodeWhisperer + SigV4', phase: 'Phase2-Executors' },
  { name: 'p2.2-antigravity-exec',issue: '94',  title: '[P2.2] Antigravity executor + Client-Metadata',  phase: 'Phase2-Executors' },
  { name: 'p2.3-gemini-cli-exec',  issue: '95',  title: '[P2.3] Gemini CLI executor Bearer + CM',        phase: 'Phase2-Executors' },
  { name: 'p2.4-xai-executor',     issue: '96',  title: '[P2.4] xAI executor NEW',                       phase: 'Phase2-Executors' },
  { name: 'p3.1-cli-registry',     issue: '108', title: '[P3.1] CLI tools registry refactor',            phase: 'Phase3-CLI' },
  { name: 'p3.2-codex-settings',   issue: '110', title: '[P3.2] Codex CLI settings',                     phase: 'Phase3-CLI' },
  { name: 'p3.3-cursor-guide',     issue: '97',  title: '[P3.3] Cursor IDE guide steps',                 phase: 'Phase3-CLI' },
  { name: 'p3.4-continue-merge',   issue: '98',  title: '[P3.4] Continue JSON merge',                    phase: 'Phase3-CLI' },
  { name: 'p3.5-roo-settings',     issue: '99',  title: '[P3.5] Roo AI Assistant settings',              phase: 'Phase3-CLI' },
  { name: 'p3.6-droid-settings',   issue: '100', title: '[P3.6] Factory Droid settings',                 phase: 'Phase3-CLI' },
  { name: 'p3.7-openclaw-apply',   issue: '101', title: '[P3.7] OpenClaw one-click apply',               phase: 'Phase3-CLI' },
  { name: 'p3.8-verify-6-clis',    issue: '102', title: '[P3.8] Verify 6 existing CLI modules',          phase: 'Phase3-CLI' },
  { name: 'p3.9-mitm-verify',      issue: '109', title: '[P3.9] Verify MITM domains + aliases',          phase: 'Phase3-CLI' },
  { name: 'p4.1-caveman-inject',   issue: '103', title: '[P4.1] Caveman prompt injection',              phase: 'Phase4-Translator' },
  { name: 'p4.2-commandcode',      issue: '104', title: '[P4.2] Format::CommandCode translators',        phase: 'Phase4-Translator' },
  { name: 'p5-e2e-smoke',          issue: '105', title: '[P5] E2E smoke test',                           phase: 'Phase5-E2E' },
  { name: 'p-chk-providers',       issue: '106', title: '[P-CHK] Composite checklist: providers',        phase: 'Phase5-E2E' },
  { name: 'p-cli-checklists',      issue: '107', title: '[P-CLI] Composite checklist: CLIs',             phase: 'Phase5-E2E' },
]

const BASE = 'main'

phase('Setup')
log(`Creating ${BRANCHES.length} branches from ${BASE}...`)

// Ensure we're on main and up to date
await agent(`Ensure the repo at /Users/tranquangdang21/Projects/openproxy is on the ${BASE} branch with no uncommitted changes. If there are uncommitted changes, stash them with "git stash push -m pre-workflow-stash" (do NOT commit).`, {
  label: 'setup-base',
  phase: 'Setup',
})

// Create all branches (sequentially; branching from main after each is fine)
for (const b of BRANCHES) {
  await agent(`
    In /Users/tranquangdang21/Projects/openproxy, create a branch named "${b.name}" from ${BASE}.
    The branch should exist locally only. Do NOT push yet.
    If the branch already exists locally, delete it first with "git branch -D ${b.name}" and recreate.
  `, {
    label: `branch-${b.name}`,
    phase: b.phase,
  })
  log(`Branch created: ${b.name}`)
}

log('All 24 branches created locally. Ready for implementation agents.')

phase('Phase1-Auth')
log('Phase 1 branches are ready for OAuth constants + provider implementation.')
log('Phase 2-5 branches also ready.')

return { branches: BRANCHES.map(b => b.name), count: BRANCHES.length }
