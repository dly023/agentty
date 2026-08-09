#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

fail=0
if ! ./script/check_quality_gates; then
  fail=1
fi
if [[ $fail -ne 0 ]]; then
  echo '[quality-infrastructure-contract] canonical quality gate invariants failed' >&2
  exit 1
fi

echo 'quality_infrastructure_contract passed'
