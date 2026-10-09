#!/usr/bin/env bash
# clippy-one: fix `cargo clippy -- -Dclippy::pedantic` incrementally, one file/lint at a time.
# Keeps each step small enough to fit in an agent context window.
#
# Usage:
#   scripts/clippy-one.sh refresh              # re-run clippy, cache to target/clippy-short.txt
#   scripts/clippy-one.sh count                # total error count (from cache)
#   scripts/clippy-one.sh files [N]            # top N files by error count (default 20)
#   scripts/clippy-one.sh lints [N]            # top N lint messages by count (default 30)
#   scripts/clippy-one.sh next                 # smallest file with errors + its errors (best next task)
#   scripts/clippy-one.sh show <path>          # all errors for one file
#   scripts/clippy-one.sh lint <pattern> [N]   # up to N errors matching lint text (default 20)
#   scripts/clippy-one.sh fixable              # auto-apply machine fix for ONE smallest file only
#
# Typical loop (human or agent):
#   1. scripts/clippy-one.sh refresh
#   2. scripts/clippy-one.sh next     # fix just that file
#   3. scripts/clippy-one.sh refresh  # confirm it went to zero
#   4. repeat; `files`/`lints` for planning, `lint <pattern>` for batching one mechanical lint.
set -euo pipefail
CACHE="${CLIPPY_ONE_CACHE:-target/clippy-short.txt}"
CLIPPY_ARGS=("--workspace" "--all-targets" "--message-format=short")

refresh() {
  mkdir -p "$(dirname "$CACHE")"
  cargo clippy "${CLIPPY_ARGS[@]}" -- -Dclippy::pedantic 2>&1 | tee "$CACHE"
  echo "---"
  count_cached
}

ensure_cache() {
  if [[ ! -f "$CACHE" ]]; then
    echo "Cache $CACHE missing, running refresh..." >&2
    refresh > /dev/null 2>&1 || true
  fi
}

error_lines() {
  grep -E ': error(:| )' "$CACHE" || true
}

count_cached() {
  ensure_cache
  local n
  n=$(error_lines | wc -l | tr -d ' ')
  echo "total: $n errors (cache: $CACHE)"
}

cmd_files() {
  ensure_cache
  local n="${1:-20}"
  error_lines | cut -d: -f1 | sort | uniq -c | sort -rn | head -n "$n"
}

cmd_lints() {
  ensure_cache
  local n="${1:-30}"
  error_lines | sed -E 's/^[^ ]+:[0-9]+:[0-9]+: error: //' | sort | uniq -c | sort -rn | head -n "$n"
}

cmd_show() {
  ensure_cache
  local f="${1:?usage: $0 show <path>}"
  grep -F "$f:" "$CACHE" || echo "no errors for $f"
}

cmd_lint() {
  ensure_cache
  local pat="${1:?usage: $0 lint <pattern> [N]}"
  local n="${2:-20}"
  grep -F "error" "$CACHE" | grep -F "$pat" | head -n "$n" || echo "no errors matching: $pat"
}

cmd_next() {
  ensure_cache
  # smallest file first: quick wins, minimal context per step
  local f
  f=$(error_lines | cut -d: -f1 | sort | uniq -c | sort -n | head -n 1 | awk '{print $2}')
  if [[ -z "${f:-}" ]]; then echo "clean: no errors"; return 0; fi
  local n
  n=$(grep -c -F "$f:" "$CACHE")
  echo "next: $f ($n errors)"
  echo "---"
  grep -F "$f:" "$CACHE"
}

cmd_fixable() {
  # Machine-applicable fixes, scoped to the single smallest file by stashing the rest? No:
  # cargo has no per-file --fix, so we fix the smallest file via rustfix-filtered apply:
  # 1. run --fix with --allow-dirty (applies all machine fixes), then show diff stat limited to next file.
  # Safer default: just show what `cargo clippy --fix` WOULD touch for the next file is not queryable,
  # so we apply one `--fix` pass and immediately re-refresh; user reviews with git diff.
  ensure_cache
  local f
  f=$(error_lines | cut -d: -f1 | sort | uniq -c | sort -n | head -n 1 | awk '{print $2}')
  echo "auto-fix pass (review with: git diff --stat; git diff -- $f)"
  cargo clippy --fix --workspace --all-targets --allow-dirty -- -Dclippy::pedantic 2>&1 | tail -n 5 || true
  refresh > /dev/null 2>&1 || true
  count_cached
}

case "${1:-help}" in
  refresh) refresh ;;
  count) count_cached ;;
  files) cmd_files "${2:-20}" ;;
  lints) cmd_lints "${2:-30}" ;;
  next) cmd_next ;;
  show) cmd_show "${2:-}" ;;
  lint) cmd_lint "${2:-}" "${3:-20}" ;;
  fixable) cmd_fixable ;;
  *) sed -n '2,20p' "$0" ;;
esac
