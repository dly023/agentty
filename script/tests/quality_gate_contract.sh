#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
./script/check_agent_harness
./script/check_spec_matrix
./script/check_feature_gap_audit
bash script/tests/quality_infrastructure_contract.sh

./script/check_remote_helper
./script/check_package_cleanup
bash script/tests/nightly_version_contract.sh
bash script/tests/i18n_exhaustive_contract.sh
