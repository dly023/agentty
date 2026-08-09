#!/bin/bash
# Usage: plan-nightly-version.sh [Cargo.toml] [YYYYMMDD]
#
# Print the nightly CalVer stamp: <next-patch>-nightly.<date>.
# Base version prefers the latest stable git tag `vX.Y.Z`; when no such tag
# exists (fresh fork / pre-first-release), fall back to [workspace.package]
# version in Cargo.toml. Never emit a leading-dot version like `.1-nightly…`.
set -euo pipefail

CARGO_TOML="${1:-Cargo.toml}"
DATE="${2:-$(date -u +%Y%m%d)}"

if [[ ! "$DATE" =~ ^[0-9]{8}$ ]]; then
  echo "plan-nightly-version: date must be YYYYMMDD, got '$DATE'" >&2
  exit 1
fi

LAST=$(git tag -l 'v*' --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1 || true)
if [[ -n "$LAST" ]]; then
  BASE=${LAST#v}
else
  if [[ ! -f "$CARGO_TOML" ]]; then
    echo "plan-nightly-version: missing $CARGO_TOML and no vX.Y.Z tags" >&2
    exit 1
  fi
  BASE=$(awk '
    /^\[workspace\.package\]/ { inside = 1; next }
    inside && /^\[/ { exit }
    inside && /^version = "/ {
      line = $0
      sub(/^version = "/, "", line)
      sub(/".*/, "", line)
      print line
      exit
    }
  ' "$CARGO_TOML")
fi

if [[ ! "$BASE" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "plan-nightly-version: invalid base version '$BASE' (need X.Y.Z from tags or Cargo.toml)" >&2
  exit 1
fi

NEXT="${BASE%.*}.$(( ${BASE##*.} + 1 ))"
VERSION="${NEXT}-nightly.${DATE}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}$ ]]; then
  echo "plan-nightly-version: refusing invalid nightly version '$VERSION'" >&2
  exit 1
fi

printf '%s\n' "$VERSION"
