#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
repo_root=$PWD

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/agentty-structured-docs.XXXXXX")
trap 'rm -rf -- "$fixture_root"' EXIT

make_fixture() {
  local root=$1
  local variant=${2:-valid}
  local spec_rule='    rule: documented behavior'
  local extra_contract=''
  local trailing_spec_document=''
  local extra_feedback_key=''
  local promoted_spec='QUALITY-CANONICAL-PRESUBMIT-CI-01'
  local promoted_trace='QUALITY-CANONICAL-PRESUBMIT-CI-097'
  local promoted_milestone='M0-ARCHITECTURE-RESET/ENV-001'

  case "$variant" in
    valid) ;;
    invalid_yaml)
      spec_rule='    rule: malformed: unquoted plain scalar'
      ;;
    multiple_yaml_documents)
      trailing_spec_document=$'---\nignored: true'
      ;;
    duplicate_mapping_key)
      extra_feedback_key='    decision: this duplicate must be rejected'
      ;;
    duplicate_spec_id)
      extra_contract=$'  - id: QUALITY-CANONICAL-PRESUBMIT-CI-01\n    rule: duplicate stable id'
      ;;
    dangling_spec)
      promoted_spec='QUALITY-MISSING-99'
      ;;
    dangling_traceability)
      promoted_trace='QUALITY-MISSING-999'
      ;;
    dangling_milestone)
      promoted_milestone='M0-ARCHITECTURE-RESET/ENV-MISSING'
      ;;
    *)
      echo "unknown structured-doc fixture: $variant" >&2
      exit 2
      ;;
  esac

  mkdir -p "$root/docs/specs" "$root/docs/quality" "$root/script" "$root/src" "$root/crates"
  cat >"$root/docs/specs/QUALITY_GATE_SPEC.yaml" <<EOF
version: 1
domain: fixture
contracts:
  - id: QUALITY-CANONICAL-PRESUBMIT-CI-01
$spec_rule
$extra_contract
status: verified
$trailing_spec_document
EOF
  cat >"$root/docs/quality/traceability.yaml" <<'EOF'
version: 1
entries:
  - id: QUALITY-CANONICAL-PRESUBMIT-CI-097
    spec: QUALITY-CANONICAL-PRESUBMIT-CI-01
    source: [docs/specs/QUALITY_GATE_SPEC.yaml]
    static_check: script/check_spec_matrix
    tests: [quality_infrastructure_contract]
    verification: script/tests/quality_infrastructure_contract.sh
    evidence: fixture
EOF
  cat >"$root/docs/quality/development-tracker.yaml" <<EOF
version: 1
feedback_policy:
  allowed_statuses: [captured, triaged, promoted, implemented, verified, declined, superseded]
feedback:
  - id: FEEDBACK-20260816-001
    summary: fixture
    status: implemented
    area: quality
    decision: keep structured documents fail closed
$extra_feedback_key
    next_action: verify fixture
    promoted_to:
      spec: $promoted_spec
      traceability: $promoted_trace
      milestone: $promoted_milestone
milestones:
  - id: M0-ARCHITECTURE-RESET
    status: verified
    items:
      - id: ENV-001
        status: verified
  - id: M0.6-SYSTEMATIC-GAP-AUDIT
    status: verified
    items: []
  - id: M1-SESSION-NAVIGATOR
    status: verified
    items: []
  - id: M2-COMPOSER
    status: verified
    items: []
  - id: M3-AGENT-ACTIVITY
    status: verified
    items: []
  - id: M4-COMPLETION
    status: verified
    items: []
  - id: M5-I18N
    status: verified
    items: []
  - id: M6-SSH
    status: verified
    items: []
technical_debt: []
EOF
  printf '%s\n' '# fixture' >"$root/DEVELOPMENT.md"

  cp "$repo_root/script/check_spec_matrix" "$root/script/check_spec_matrix"
  cp "$repo_root/script/check_development_tracker" "$root/script/check_development_tracker"
  if [[ -f "$repo_root/script/check_structured_docs" ]]; then
    cp "$repo_root/script/check_structured_docs" "$root/script/check_structured_docs"
  fi
  chmod +x "$root/script/"check_*
}

expect_accept() {
  local root=$1
  local gate=$2
  local output="$root/${gate}.out"
  if ! "$root/script/$gate" >"$output" 2>&1; then
    cat "$output" >&2
    echo "[quality-infrastructure-contract] valid fixture failed $gate" >&2
    exit 1
  fi
}

expect_reject() {
  local variant=$1
  local gate=$2
  local diagnostic=$3
  local root="$fixture_root/$variant"
  local output="$root/${gate}.out"
  make_fixture "$root" "$variant"
  if "$root/script/$gate" >"$output" 2>&1; then
    echo "[quality-infrastructure-contract] $gate accepted $variant" >&2
    exit 1
  fi
  if ! grep -Fq "$diagnostic" "$output"; then
    cat "$output" >&2
    echo "[quality-infrastructure-contract] $gate rejected $variant without '$diagnostic'" >&2
    exit 1
  fi
}

valid_root="$fixture_root/valid"
make_fixture "$valid_root" valid
expect_accept "$valid_root" check_spec_matrix
expect_accept "$valid_root" check_development_tracker

expect_reject invalid_yaml check_spec_matrix 'YAML syntax error'
expect_reject multiple_yaml_documents check_spec_matrix 'multiple YAML documents'
expect_reject duplicate_mapping_key check_development_tracker 'duplicate mapping key'
expect_reject duplicate_spec_id check_spec_matrix 'duplicate spec contract id'
expect_reject dangling_spec check_development_tracker 'unknown spec contract'
expect_reject dangling_traceability check_development_tracker 'unknown traceability entry'
expect_reject dangling_milestone check_development_tracker 'unknown milestone item'

fail=0
if ! ./script/check_quality_gates; then
  fail=1
fi
if [[ $fail -ne 0 ]]; then
  echo '[quality-infrastructure-contract] canonical quality gate invariants failed' >&2
  exit 1
fi

echo 'quality_infrastructure_contract passed'
