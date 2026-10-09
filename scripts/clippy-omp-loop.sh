#!/usr/bin/env bash
# clippy-omp-loop: fix pedantic clippy errors one at a time, each in a FRESH `omp -p` session.
# Why: shoving all errors into one agent session blows up context. This loops:
#   refresh clippy -> take FIRST error -> fresh `omp -p "@AGENTS.md fix this one..."` -> repeat.
# Each omp run loads @AGENTS.md itself, fixes ONE error, tests, commits, exits.
# Re-selects the error every pass (line numbers shift after edits), so no stale list.
#
# Usage:
#   scripts/clippy-omp-loop.sh [--max N] [--mode error|file] [--dry-run]
#                              [--model X] [--max-time 10m] [--no-auto-approve]
#
#   --max N           stop after N fixes (default 0 = until clean)
#   --mode error      one error line per session (default; smallest context)
#   --mode file       whole smallest file per session (fewer sessions, more context)
#   --dry-run         print the omp command for the next item, run nothing
#   --model X         pass --model X to omp
#   --max-time T      pass --max-time T to omp (default 10m)
#   --no-auto-approve don't pass --auto-approve (default passes it; unattended loop hangs without it)
#
# Examples:
#   scripts/clippy-omp-loop.sh --dry-run            # see what the next session would get
#   scripts/clippy-omp-loop.sh --max 1              # fix exactly one error, commit, stop
#   scripts/clippy-omp-loop.sh --max 5              # fix five, one session each
#   scripts/clippy-omp-loop.sh --mode file --max 3  # smallest-file batching instead
set -euo pipefail

MAX=0
MODE="error"
DRY_RUN=0
MODEL=""
MAX_TIME="10m"
AUTO_APPROVE=1

while (($# > 0)); do
  case "$1" in
    --max) MAX="${2:?}"; shift 2 ;;
    --max=*) MAX="${1#--max=}"; shift ;;
    --mode) MODE="${2:?}"; shift 2 ;;
    --mode=*) MODE="${1#--mode=}"; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --model) MODEL="${2:?}"; shift 2 ;;
    --model=*) MODEL="${1#--model=}"; shift ;;
    --max-time) MAX_TIME="${2:?}"; shift 2 ;;
    --max-time=*) MAX_TIME="${1#--max-time=}"; shift ;;
    --no-auto-approve) AUTO_APPROVE=0; shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1 (see --help)" >&2; exit 1 ;;
  esac
done

[[ "$MODE" == "error" || "$MODE" == "file" ]] || { echo "--mode must be error|file" >&2; exit 1; }
command -v omp >/dev/null || { echo "omp not on PATH" >&2; exit 1; }
command -v cargo >/dev/null || { echo "cargo not on PATH" >&2; exit 1; }

# Always fresh: never resume a prior session, each iteration is a new context.
current_errors() {
  cargo clippy --workspace --all-targets --message-format=short -- -Dclippy::pedantic 2>&1 \
    | grep -E ': error(:| )' || true
}

count_errors() { current_errors | wc -l | tr -d ' '; }

build_prompt_error() {
  local err="$1"
  local file="${err%%:*}"
  cat <<EOF
Follow @AGENTS.md exactly (serena for reads/writes, skills for new Rust code, ≤500 LOCs/module, commit when done).

Fix EXACTLY ONE clippy pedantic error and nothing else:

\`\`\`
$err
\`\`\`

Steps:
1. Read only the code around $file needed for this error.
2. Apply the minimal idiomatic Rust fix for the reported lint. Do not touch other errors, other files, or refactor unrelated code.
3. Verify with: cargo clippy --workspace --all-targets -- -Dclippy::pedantic 2>&1 | grep -F "$file" (expect no rows for this lint; other files' errors are out of scope, ignore them).
4. Run the tests covering the touched crate (e.g. cargo test -p <crate> \$(basename $file .rs) or the nearest test target). If no targeted test exists, run cargo check -p <crate>.
5. Commit ONLY the touched file(s): git add <files> && git commit -m "clippy: <lint-short> in $file". Never use git add -A. Verify with git log --oneline -1 and git status --short.

If the error is a false positive needing suppression, prefer a narrow #[allow(...)] with a comment over restructuring. Report what you changed, the verify commands' output, and the commit hash. Stop after this one error.
EOF
}

build_prompt_file() {
  local file="$1" errs="$2" n="$3"
  cat <<EOF
Follow @AGENTS.md exactly (serena for reads/writes, skills for new Rust code, ≤500 LOCs/module, commit when done).

Fix ONLY these $n clippy pedantic errors in $file and nothing else:

\`\`\`
$errs
\`\`\`

Steps:
1. Read only $file (and minimal imports as needed).
2. Apply minimal idiomatic Rust fixes, one per reported lint. Do not touch other files.
3. Verify with: cargo clippy --workspace --all-targets -- -Dclippy::pedantic 2>&1 | grep -F "$file:" (expect empty).
4. Run the tests covering the touched crate. If no targeted test exists, run cargo check for that crate.
5. Commit ONLY $file (plus split-out modules if you had to break it up): git add $file && git commit -m "clippy: clean $file ($n errors)". Never use git add -A. Verify with git log --oneline -1 and git status --short.

Prefer narrow #[allow(...)] with a comment for false positives. Report changes, verify output, and commit hash. Stop after this file.
EOF
}

run_omp() {
  local prompt="$1"
  local -a cmd=(omp -p)
  ((AUTO_APPROVE == 1)) && cmd+=(--auto-approve)
  [[ -n "$MAX_TIME" ]] && cmd+=(--max-time "$MAX_TIME")
  [[ -n "$MODEL" ]] && cmd+=(--model "$MODEL")
  # @AGENTS.md as its own message arg: documented file-inclusion form
  # (embedded "@AGENTS.md" inside a longer -p string may not expand).
  cmd+=("@AGENTS.md" "$prompt")
  if ((DRY_RUN == 1)); then
    printf '%q ' "${cmd[@]}"
    printf '\n'
    return 0
  fi
  "${cmd[@]}"
}

before=$(count_errors)
echo "clippy-omp-loop: start $before errors (mode=$MODE, max=${MAX:-until-clean})"
if ((before == 0)); then echo "clean: no errors"; exit 0; fi

done_n=0
while true; do
  if ((MAX > 0 && done_n >= MAX)); then break; fi
  errs=$(current_errors)
  [[ -z "$errs" ]] && { echo "clean: no errors after $done_n session(s)"; break; }

  if [[ "$MODE" == "error" ]]; then
    err=$(printf '%s\n' "$errs" | head -n 1)
    file="${err%%:*}"
    echo "--- session $((done_n + 1)): $err"
    prompt=$(build_prompt_error "$err")
  else
    file=$(printf '%s\n' "$errs" | cut -d: -f1 | sort | uniq -c | sort -n | head -n 1 | awk '{print $2}')
    ferrs=$(printf '%s\n' "$errs" | grep -F "$file:")
    n=$(printf '%s\n' "$ferrs" | wc -l | tr -d ' ')
    echo "--- session $((done_n + 1)): $file ($n errors)"
    prompt=$(build_prompt_file "$file" "$ferrs" "$n")
  fi

  if ((DRY_RUN == 1)); then
    run_omp "$prompt"
    break
  fi

  before_one=$(printf '%s\n' "$errs" | wc -l | tr -d ' ')
  if ! run_omp "$prompt"; then
    echo "omp session failed (exit $?) — stopping. Re-run to resume; fixed errors stay fixed." >&2
    exit 1
  fi
  after_one=$(count_errors)
  done_n=$((done_n + 1))
  echo "session $done_n done: $before_one -> $after_one errors remaining"
  git log --oneline -1
  if ((after_one == 0)); then echo "clean: no errors"; break; fi
  if ((after_one >= before_one)); then
    echo "no progress (still $after_one). Stopping — inspect the last commit/session before re-running." >&2
    exit 2
  fi
done

echo "clippy-omp-loop: finished $done_n session(s), $(count_errors) errors remaining"
