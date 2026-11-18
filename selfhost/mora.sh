#!/usr/bin/env bash
# Run the Mora-in-Mora functional core on the Rust mora host.
# Copyright (c) 2026 Microbiosis. BSD-3-Clause.
# Usage, from the repository root or from anywhere:
#   selfhost/mora.sh <file.mora>
#   selfhost/mora.sh --check <file.mora>
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

printf '%s\n%s\n' "$mode" "$path" > selfhost/.argv
rm -f selfhost/.code selfhost/.diag
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
