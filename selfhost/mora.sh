#!/usr/bin/env bash
# Run the Mora-in-Mora functional core on the Rust mora host.
# Copyright (c) 2026 Microbiosis. BSD-3-Clause.
# Usage, from the repository root or from anywhere:
#   selfhost/mora.sh <file.mora>
#   selfhost/mora.sh --check <file.mora>
#   selfhost/mora.sh --boot <file.mora>
#   selfhost/mora.sh --version

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
HOST="${MORA_HOST:-$ROOT/target/release/mora}"
if [[ ! -x "$HOST" ]]; then
  echo "selfhost: Rust mora host not found at $HOST" >&2
  echo "build it with: rustup run stable cargo build --release -p mora --bin mora" >&2
  exit 1
fi

mode="run"
path=""
boot=0
if [[ "${1:-}" == "--boot" ]]; then
  boot=1
  shift
fi
case "${1:-}" in
  --check)
    mode="check"
    path="${2:-}"
    ;;
  --version|-v)
    mode="version"
    ;;
  --help|-h)
    mode="help"
    ;;
  --repl)
    mode="repl"
    ;;
  --opt|--opt=*)
    echo "selfhost: --opt is the Rust compiler's SSA flag and is not in this trial" >&2
    exit 2
    ;;
  "")
    mode="help"
    ;;
  *)
    mode="run"
    path="$1"
    ;;
esac

if [[ "$mode" == "run" || "$mode" == "check" ]]; then
  if [[ -z "$path" ]]; then
    echo "usage: selfhost/mora.sh [--check] <file.mora>" >&2
    exit 1
  fi
fi

if [[ "$boot" -eq 1 ]]; then
  # Outer frame runs this interpreter; the inner frame is what that copy runs.
  # The outer pops its frame before evaluating, so the inner read sees only the rest.
  printf '%s\n%s\n---\n%s\n%s\n' "run" "selfhost/mora.mora" "$mode" "$path" > selfhost/.argv
else
  printf '%s\n%s\n' "$mode" "$path" > selfhost/.argv
fi
printf '' > selfhost/.code
printf '' > selfhost/.diag
set +e
"$HOST" selfhost/mora.mora
host_status=$?
set -e
if [[ -s selfhost/.diag ]]; then
  cat selfhost/.diag >&2
fi
if [[ -f selfhost/.code ]]; then
  exit "$(tr -d '[:space:]' < selfhost/.code)"
fi
exit "$host_status"
