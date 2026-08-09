#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

./script/check_feature_gap_audit

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp docs/quality/ASHIDE_AGENTTY_FEATURE_GAP_AUDIT.yaml "$tmp/audit.yaml"

sed '/^  - keyboard_and_accessibility$/d' "$tmp/audit.yaml" > "$tmp/missing-dimension.yaml"
if AGENTTY_FEATURE_GAP_AUDIT="$tmp/missing-dimension.yaml" ./script/check_feature_gap_audit >"$tmp/out" 2>&1; then
  echo '[feature-gap-audit-test] missing dimension was accepted' >&2
  exit 1
fi
rg -q 'missing audit dimension keyboard_and_accessibility' "$tmp/out" || {
  cat "$tmp/out" >&2
  echo '[feature-gap-audit-test] wrong missing-dimension failure' >&2
  exit 1
}

sed '/^  - id: GAP-SESSION-LIVE-ALIAS$/,/^  - id: /{ /^  - id: GAP-SESSION-LIVE-ALIAS$/d; /^  - id: /!d; }' \
  "$tmp/audit.yaml" > "$tmp/missing-known-gap.yaml"
if AGENTTY_FEATURE_GAP_AUDIT="$tmp/missing-known-gap.yaml" ./script/check_feature_gap_audit >"$tmp/out" 2>&1; then
  echo '[feature-gap-audit-test] missing known gap was accepted' >&2
  exit 1
fi
rg -q 'missing known gap GAP-SESSION-LIVE-ALIAS' "$tmp/out" || {
  cat "$tmp/out" >&2
  echo '[feature-gap-audit-test] wrong known-gap failure' >&2
  exit 1
}

echo 'feature_gap_audit_contract passed'
