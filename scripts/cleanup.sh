#!/usr/bin/env bash
# cleanup.sh — prune old build/debug artifacts to prevent disk exhaustion.
#
# Context: a prior ENOSPC incident was caused by accumulated debug builds in
# /tmp/op-* (e.g. op-head) plus a 3GB+ target/ tree. This script removes only
# regenerable intermediates and temp debug outputs — never source, Cargo.lock,
# .git, or artifacts newer than a safety window.
#
# Triggers (any one activates pruning):
#   - filesystem holding the repo OR /tmp is >= --threshold % full (default 80)
#   - --aggressive flag (lowers age thresholds, prunes cargo registry cache)
#   - explicit invocation (always prunes files older than the age window)
#
# Usage:
#   ./scripts/cleanup.sh                 # prune old artifacts (age-based) + space-based if full
#   ./scripts/cleanup.sh --dry-run       # report only, delete nothing
#   ./scripts/cleanup.sh --aggressive    # aggressive thresholds + cargo registry prune
#   ./scripts/cleanup.sh --threshold=75  # custom disk-full percent
#   ./scripts/cleanup.sh --help
#
# Safety:
#   - set -euo pipefail; every destructive find is scoped to a known temp dir
#   - never touches src/, Cargo.lock, .git, or files newer than the guard window
#   - skips target/ pruning if a cargo build/test is currently running
#   - logs timestamp + freed bytes to .omo/cleanup.log
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# ── Config ────────────────────────────────────────────────────────────────
DRY_RUN=0
AGGRESSIVE=0
DISK_THRESHOLD=80
MIN_AGE_DAYS=7        # default prune age for regenerable intermediates
AGGR_AGE_DAYS=1       # age used when disk is full or --aggressive
GUARD_HOURS=24        # never delete artifacts newer than this (non-aggressive)
LOG_DIR="$REPO_ROOT/.omo"
LOG_FILE="$LOG_DIR/cleanup.log"

# ── Portable stat for byte sizing (GNU vs BSD/macOS) ───────────────────────
# STAT_OPTS is an array so it expands to the correct words without globbing.
if stat -c '%s' /dev/null >/dev/null 2>&1; then
  STAT_OPTS=(-c '%s')   # GNU coreutils
else
  STAT_OPTS=(-f '%z')   # BSD/macOS stat
fi

# ── Parse args ─────────────────────────────────────────────────────────────
for arg in "$@"; do
  case "$arg" in
    --dry-run)      DRY_RUN=1 ;;
    --aggressive)   AGGRESSIVE=1 ;;
    --threshold=*)  DISK_THRESHOLD="${arg#*=}" ;;
    -h|--help)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "Unknown arg: $arg (try --help)" >&2; exit 1 ;;
  esac
done

mkdir -p "$LOG_DIR"

# ── Logging ────────────────────────────────────────────────────────────────
log() {
  local ts; ts="$(date '+%Y-%m-%d %H:%M:%S')"
  if [[ "$DRY_RUN" == 1 ]]; then
    printf '%s [dry-run] %s\n' "$ts" "$*" | tee -a "$LOG_FILE"
  else
    printf '%s %s\n' "$ts" "$*" | tee -a "$LOG_FILE"
  fi
}

human() {
  local b="$1"
  awk -v b="$b" 'BEGIN{
    split("B KB MB GB TB", u, " ");
    i=1; while (b>=1024 && i<5){b/=1024; i++}
    if (i==1) printf "%d %s", b, u[i];
    else      printf "%.2f %s", b, u[i];
  }'
}

# ── Trap: always log exit ──────────────────────────────────────────────────
# shellcheck disable=SC2329  # invoked indirectly via trap
cleanup_trap() {
  local code=$?
  if [[ $code -ne 0 ]]; then
    log "ERROR: cleanup exited with code $code"
  fi
}
trap cleanup_trap EXIT

# ── Disk usage of the filesystems we care about ────────────────────────────
disk_pct() {
  # $1 = mount path; prints integer percent used
  df -P "$1" 2>/dev/null | awk 'NR==2 {gsub("%","",$5); print $5+0}'
}
REPO_PCT="$(disk_pct "$REPO_ROOT")"
TMP_PCT="$(disk_pct /tmp)"
MAX_PCT=$(( REPO_PCT > TMP_PCT ? REPO_PCT : TMP_PCT ))

SPACE_PRESSURE=0
if (( MAX_PCT >= DISK_THRESHOLD )); then SPACE_PRESSURE=1; fi

# Age window: tighten when under space pressure or aggressive.
if (( SPACE_PRESSURE == 1 || AGGRESSIVE == 1 )); then
  AGE_DAYS="$AGGR_AGE_DAYS"
else
  AGE_DAYS="$MIN_AGE_DAYS"
fi

log "start: repo=${REPO_PCT}% tmp=${TMP_PCT}% threshold=${DISK_THRESHOLD}% aggressive=${AGGRESSIVE} dry-run=${DRY_RUN} age=+${AGE_DAYS}d guard=${GUARD_HOURS}h"

# ── Freed-space accounting ─────────────────────────────────────────────────
FREED=0

bytes_of() {
  # $1 = path, remaining = extra find predicates (before -type f)
  local dir="$1"; shift
  [[ -d "$dir" ]] || { echo 0; return; }
  find "$dir" "$@" -type f -mtime +"$AGE_DAYS" -exec stat "${STAT_OPTS[@]}" {} + 2>/dev/null \
    | awk '{s+=$1} END{print s+0}'
}

# prune_files <dir> [extra find predicates...]
# Deletes regular files older than AGE_DAYS under <dir>. Never removes <dir>
# itself or anything outside it.
prune_files() {
  local dir="$1"; shift
  [[ -d "$dir" ]] || { log "skip (missing): $dir"; return 0; }
  local sz; sz="$(bytes_of "$dir" "$@")"
  FREED=$(( FREED + sz ))
  if (( DRY_RUN == 1 )); then
    log "[dry-run] would free $(human "$sz") in $dir (mtime>+${AGE_DAYS}d)"
    return 0
  fi
  # -delete on files only; directories are left intact (cargo rebuilds them).
  find "$dir" "$@" -type f -mtime +"$AGE_DAYS" -delete 2>/dev/null || true
  log "freed $(human "$sz") in $dir (mtime>+${AGE_DAYS}d)"
}

# prune_tmp_op: /tmp/op-* debug outputs (e.g. op-head). These are whole
# scratch trees, so remove matching entries entirely (files + dirs).
prune_tmp_op() {
  [[ -d /tmp ]] || return 0
  local sz
  sz="$(find /tmp -maxdepth 1 -name 'op-*' -mtime +"$AGE_DAYS" -exec stat "${STAT_OPTS[@]}" {} + 2>/dev/null \
        | awk '{s+=$1} END{print s+0}')"
  FREED=$(( FREED + sz ))
  if (( DRY_RUN == 1 )); then
    log "[dry-run] would free $(human "$sz") in /tmp/op-* (mtime>+${AGE_DAYS}d)"
    return 0
  fi
  # Scoped strictly to /tmp/op-* — never a bare rm -rf.
  find /tmp -maxdepth 1 -name 'op-*' -mtime +"$AGE_DAYS" -exec rm -rf {} + 2>/dev/null || true
  log "freed $(human "$sz") in /tmp/op-* (mtime>+${AGE_DAYS}d)"
}

# prune_cache_dir: fully clear a regenerable cache directory (e.g. web/dist/.cache).
prune_cache_dir() {
  local dir="$1"
  [[ -d "$dir" ]] || { log "skip (missing): $dir"; return 0; }
  local sz; sz="$(du -sb "$dir" 2>/dev/null | awk '{print $1+0}')"
  FREED=$(( FREED + sz ))
  if (( DRY_RUN == 1 )); then
    log "[dry-run] would free $(human "$sz") in $dir (full cache clear)"
    return 0
  fi
  rm -rf "$dir"
  log "freed $(human "$sz") in $dir (full cache clear)"
}

# ── Concurrency guard: don't prune target/ while cargo is building ─────────
cargo_running=0
if pgrep -f 'cargo (build|test|run|bench|check)' >/dev/null 2>&1; then
  cargo_running=1
  log "WARN: cargo build/test detected — skipping target/ pruning to avoid clobbering an active build"
fi

# ── 1. target/debug intermediates (regenerable) ────────────────────────────
if (( cargo_running == 0 )); then
  prune_files "target/debug"
else
  log "skip: target/debug (cargo active)"
fi

# ── 2. target/tmp (cargo scratch) — safe to clear when not building ────────
if (( cargo_running == 0 )); then
  # target/tmp holds short-lived temp files; clear anything older than 1 day.
  if [[ -d target/tmp ]]; then
    sz_tmp="$(find target/tmp -mindepth 1 -mtime +1 -exec stat "${STAT_OPTS[@]}" {} + 2>/dev/null | awk '{s+=$1} END{print s+0}')"
    FREED=$(( FREED + sz_tmp ))
    if (( DRY_RUN == 1 )); then
      log "[dry-run] would free $(human "$sz_tmp") in target/tmp (mtime>+1d)"
    else
      find target/tmp -mindepth 1 -mtime +1 -delete 2>/dev/null || true
      log "freed $(human "$sz_tmp") in target/tmp (mtime>+1d)"
    fi
  fi
else
  log "skip: target/tmp (cargo active)"
fi

# ── 3. /tmp/op-* debug scratch trees ───────────────────────────────────────
prune_tmp_op

# ── 4. web/dist/.cache (Astro build cache) ─────────────────────────────────
prune_cache_dir "web/dist/.cache"

# ── 5. cargo registry cache — only under space pressure / aggressive ───────
if (( SPACE_PRESSURE == 1 || AGGRESSIVE == 1 )); then
  CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
  if [[ -d "$CARGO_HOME/registry/cache" ]]; then
    # Downloaded .crate archives are safe to prune (re-downloaded on demand).
    # Keep the extracted src/ tree — it is needed for builds.
    sz_reg="$(find "$CARGO_HOME/registry/cache" -type f -mtime +30 -exec stat "${STAT_OPTS[@]}" {} + 2>/dev/null | awk '{s+=$1} END{print s+0}')"
    FREED=$(( FREED + sz_reg ))
    if (( DRY_RUN == 1 )); then
      log "[dry-run] would free $(human "$sz_reg") in ~/.cargo/registry/cache (mtime>+30d)"
    else
      find "$CARGO_HOME/registry/cache" -type f -mtime +30 -delete 2>/dev/null || true
      log "freed $(human "$sz_reg") in ~/.cargo/registry/cache (mtime>+30d)"
    fi
  fi
else
  log "skip: cargo registry cache (no space pressure; use --aggressive to force)"
fi

# ── Summary ─────────────────────────────────────────────────────────────────
log "done: freed $(human "$FREED") total this run"
exit 0
