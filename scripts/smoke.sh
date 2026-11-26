#!/usr/bin/env bash
set -euo pipefail

binary=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"

"$binary" --version
"$binary" --store "$work/store" init --git
export PLAINDB_STORE="$work/store"
"$binary" db create demo
"$binary" collection create demo notes --format md
"$binary" insert demo notes n1 --data '{"title":"Hello","body":"First note"}'

found=$("$binary" --json find demo notes --filter '{"title":"Hello"}')
case "$found" in
  *'"_id":"n1"'*) ;;
  *) echo "find did not return n1: $found" >&2; exit 1 ;;
esac

history=$("$binary" --json history demo notes n1)
entries=$(printf '%s' "$history" | grep -o '"commit":' | wc -l | tr -d ' ')
if [ "$entries" != "1" ]; then
  echo "expected one history entry, got $entries: $history" >&2
  exit 1
fi

echo "smoke test passed"
