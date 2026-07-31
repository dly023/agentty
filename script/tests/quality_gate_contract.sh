#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
./script/check_agent_harness
./script/check_spec_matrix
./script/check_remote_helper
