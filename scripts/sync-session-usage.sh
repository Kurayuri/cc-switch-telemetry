#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
upstream="$repo_root/3rdparty/cc-switch"
pin_file="$upstream/.telemetry-upstream-commit"

test -d "$upstream/.git" || { echo "missing 3rdparty/cc-switch checkout" >&2; exit 1; }
expected=$(tr -d '[:space:]' < "$pin_file")
actual=$(git -C "$upstream" rev-parse HEAD)
test "$actual" = "$expected" || {
  echo "cc-switch checkout is $actual, expected pinned $expected" >&2
  exit 1
}

manifest="$repo_root/3rdparty/cc-switch/.telemetry-importer-files"
if test -f "$manifest"; then
  while read -r digest file; do
    test -n "$digest" || continue
    test -f "$upstream/$file" || {
      echo "missing pinned importer source: $file" >&2
      exit 1
    }
    actual_digest=$(sha256sum "$upstream/$file" | awk '{print $1}')
    test "$actual_digest" = "$digest" || {
      echo "upstream importer source drifted: $file" >&2
      echo "run the review/update workflow before changing the adapter" >&2
      exit 1
    }
  done < "$manifest"
fi

echo "cc-switch upstream pinned at $actual"
echo "neutral adapter sources are validated against the pinned upstream manifest"
