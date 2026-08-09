#!/usr/bin/env bash
# Contract for nightly CalVer planning when the repo has no stable tags yet.
set -euo pipefail
cd "$(dirname "$0")/../.."

plan=.github/scripts/plan-nightly-version.sh
stamp=.github/scripts/stamp-version.sh
[[ -x "$plan" || -f "$plan" ]] || { echo "[nightly-version] missing $plan" >&2; exit 1; }
[[ -f "$stamp" ]] || { echo "[nightly-version] missing $stamp" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
repo_root=$PWD

# Fixture workspace package version with no git tags → bump patch.
cat >"$tmp/Cargo.toml" <<'EOF'
[workspace.package]
version = "0.0.1"
EOF
git -C "$tmp" init -q
git -C "$tmp" config user.email "test@example.com"
git -C "$tmp" config user.name "test"
git -C "$tmp" add Cargo.toml
git -C "$tmp" commit -qm "fixture"
# Script reads tags from the current repo; run it with GIT_DIR pointing at the
# empty-of-tags fixture so tag lookup is empty, and pass the fixture Cargo.toml.
got=$(
  GIT_DIR="$tmp/.git" GIT_WORK_TREE="$tmp" \
    bash "$plan" "$tmp/Cargo.toml" 20260809
)
[[ "$got" == "0.0.2-nightly.20260809" ]] || {
  echo "[nightly-version] expected 0.0.2-nightly.20260809 without tags, got '$got'" >&2
  exit 1
}

# Stable tag wins over Cargo.toml.
git -C "$tmp" tag v1.2.3
got=$(
  GIT_DIR="$tmp/.git" GIT_WORK_TREE="$tmp" \
    bash "$plan" "$tmp/Cargo.toml" 20260809
)
[[ "$got" == "1.2.4-nightly.20260809" ]] || {
  echo "[nightly-version] expected 1.2.4-nightly.20260809 from tag, got '$got'" >&2
  exit 1
}

# stamp-version must reject the historical leading-dot failure mode without
# touching the real workspace manifest.
cp Cargo.toml "$tmp/Cargo.toml"
if (
  cd "$tmp"
  bash "$repo_root/$stamp" ".1-nightly.20260808"
) >/dev/null 2>&1; then
  echo "[nightly-version] stamp-version must reject '.1-nightly.20260808'" >&2
  exit 1
fi
# Confirm the fixture was not rewritten to the bad stamp.
if grep -q '\.1-nightly' "$tmp/Cargo.toml"; then
  echo "[nightly-version] stamp-version wrote an invalid version before rejecting" >&2
  exit 1
fi

echo "nightly version contract passed"
