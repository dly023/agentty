#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
development="$root/DEVELOPMENT.md"
tracker="$root/docs/quality/development-tracker.yaml"

grep -Fq 'do not merge it wholesale into Agentty' "$development"
grep -Fq 'Prefer a focused `git cherry-pick`' "$development"
grep -Fq 'implement them natively through the SPEC workflow' "$development"
grep -Fq 'Never use the latest `upstream/main` as an implicit release base' "$development"
grep -Fq 'FEEDBACK-20260814-004' "$tracker"

echo "upstream selective absorption contract passed"
