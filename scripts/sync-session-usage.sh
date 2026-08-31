#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
snapshot_root="$repo_root/3rdparty/cc-switch"
pin_file="$snapshot_root/.telemetry-upstream-commit"
manifest="$snapshot_root/.telemetry-importer-files"
snapshot_doc="$snapshot_root/PARSER_SNAPSHOT.md"
license_file="$snapshot_root/LICENSE"
importer_source="$repo_root/crates/session-usage-core/src/lib.rs"
expected_pin="3217f72596f2d1c0f879f0a05f83803825d9809f"

expected_files=(
  "src-tauri/src/services/session_usage.rs"
  "src-tauri/src/services/session_usage_codex.rs"
  "src-tauri/src/services/session_usage_gemini.rs"
  "src-tauri/src/services/session_usage_grokbuild.rs"
  "src-tauri/src/services/session_usage_opencode.rs"
  "src-tauri/src/services/session_usage_pi.rs"
)

fail() {
  echo "session parser snapshot verification failed: $*" >&2
  exit 1
}

expected_hash_for() {
  case "$1" in
    src-tauri/src/services/session_usage.rs)
      echo "7ae348e46f259d19195d8de4f42bea48dd6963ad77441aff94d05d52c44621f9"
      ;;
    src-tauri/src/services/session_usage_codex.rs)
      echo "eadb325ca4b408c9a330784d3e6e6bca86b235cf0e5ce9a4568c82596e055d03"
      ;;
    src-tauri/src/services/session_usage_gemini.rs)
      echo "a5b158271a984325d29a6b3fbba99429d96a9729482c99d64cf73cbd82dbf727"
      ;;
    src-tauri/src/services/session_usage_grokbuild.rs)
      echo "269f0b3250a89b5d562fa1bab41ea1c4aedb7f61f50af961712697b8a8adba55"
      ;;
    src-tauri/src/services/session_usage_opencode.rs)
      echo "5b423f4deffab6f330e1dfb68852d3a72e26f6c0095417935a1db5545f4bb516"
      ;;
    src-tauri/src/services/session_usage_pi.rs)
      echo "8e8065b8dcca900ebb70ea0e9d47401176a88e2145a3fe105730061e6ef019f8"
      ;;
    *)
      return 1
      ;;
  esac
}

command -v sha256sum >/dev/null || fail "sha256sum is required"

for required_file in "$pin_file" "$manifest" "$snapshot_doc" "$license_file" "$importer_source"; do
  [[ -f "$required_file" && ! -L "$required_file" ]] || fail "missing or symbolic-link file: $required_file"
done

for snapshot_dir in \
  "$snapshot_root" \
  "$snapshot_root/src-tauri" \
  "$snapshot_root/src-tauri/src" \
  "$snapshot_root/src-tauri/src/services"; do
  [[ -d "$snapshot_dir" && ! -L "$snapshot_dir" ]] || fail "missing or symbolic-link directory: $snapshot_dir"
done

pin_lines=()
mapfile -t pin_lines < "$pin_file"
(( ${#pin_lines[@]} == 1 )) || fail "pin file must contain exactly one line"
pin="${pin_lines[0]}"
[[ "$pin" =~ ^[0-9a-f]{40}$ ]] || fail "pin must be 40 lowercase hexadecimal characters"
[[ "$pin" == "$expected_pin" ]] || fail "unexpected source pin: $pin"

source_commits=()
mapfile -t source_commits < <(
  sed -nE 's/^pub const IMPORTER_SOURCE_COMMIT: &str = "([0-9a-f]{40})";$/\1/p' "$importer_source"
)
(( ${#source_commits[@]} == 1 )) || fail "IMPORTER_SOURCE_COMMIT must appear exactly once"
[[ "${source_commits[0]}" == "$pin" ]] || fail "Rust importer source pin does not match snapshot pin"

declare -A manifest_hashes=()
manifest_count=0
while read -r digest repo_file extra; do
  [[ -n "$digest" && -n "$repo_file" && -z "${extra:-}" ]] || fail "malformed manifest line"
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || fail "invalid SHA-256 for $repo_file"
  expected_digest=$(expected_hash_for "$repo_file") || fail "unexpected manifest path: $repo_file"
  [[ -z "${manifest_hashes[$repo_file]+present}" ]] || fail "duplicate manifest path: $repo_file"
  [[ "$digest" == "$expected_digest" ]] || fail "unexpected declared SHA-256 for $repo_file"
  manifest_hashes["$repo_file"]="$digest"
  manifest_count=$((manifest_count + 1))
done < "$manifest"
(( manifest_count == ${#expected_files[@]} )) || fail "manifest must contain exactly six entries"

doc_pin_count=$( (grep -Fo -- "$pin" "$snapshot_doc" || true) | wc -l | tr -d '[:space:]')
[[ "$doc_pin_count" == "1" ]] || fail "snapshot documentation must contain the pin exactly once"
doc_row_count=$(grep -Ec '^\| `src-tauri/src/services/session_usage(_[a-z]+)?\.rs` \| `[0-9a-f]{64}` \|$' "$snapshot_doc" || true)
[[ "$doc_row_count" == "${#expected_files[@]}" ]] || fail "snapshot documentation must contain exactly six parser rows"

for repo_file in "${expected_files[@]}"; do
  [[ -n "${manifest_hashes[$repo_file]+present}" ]] || fail "manifest is missing $repo_file"
  expected_digest=$(expected_hash_for "$repo_file")
  snapshot_file="$snapshot_root/$repo_file"
  [[ -f "$snapshot_file" && ! -L "$snapshot_file" ]] || fail "missing or symbolic-link parser: $repo_file"
  actual_digest=$(sha256sum "$snapshot_file" | awk '{print $1}')
  [[ "$actual_digest" == "$expected_digest" ]] || fail "snapshot content drifted: $repo_file"
  doc_line="| \`$repo_file\` | \`$expected_digest\` |"
  doc_line_count=$(grep -Fxc -- "$doc_line" "$snapshot_doc" || true)
  [[ "$doc_line_count" == "1" ]] || fail "snapshot documentation mismatch: $repo_file"
done

echo "cc-switch parser snapshot verified at $pin"
echo "six parser files, manifest, documentation, Rust pin, and MIT license are consistent"
