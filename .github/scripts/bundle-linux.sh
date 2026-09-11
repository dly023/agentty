#!/bin/bash
# Usage: bundle-linux.sh <target-triple> <arch-label>
# Package the release binary into a tarball:
#   dist/agentty-<version>-linux-<arch>.tar.gz
#
# Fonts and the app icon are embedded via include_bytes!, so the archive is the
# stripped executable plus a sibling completions/ dir (loaded at runtime — see
# terminal::signature) and the license/readme. gpui's x11/wayland backends still
# dynamic-link the usual system libs at runtime — see the README's Linux
# build-dependency list — so this is an unsigned build, not a
# portable AppImage.
set -euo pipefail

TARGET="$1"
ARCH="$2"
bash script/check_package_target "$TARGET" "$ARCH" linux
# Anchored on `= "` — see the note in bundle-macos.sh: the root manifest leads
# with `version.workspace = true`, which a bare `^version` match would return
# verbatim as the "version" and bake into every asset filename.
VERSION="$(grep -m1 '^version = "' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?([0-9A-Za-z.-]+)?$ ]]; then
  echo "bundle-linux: could not read a version from Cargo.toml (got '$VERSION')" >&2
  exit 1
fi
NAME="agentty-${VERSION}-linux-${ARCH}"
STAGE="dist/${NAME}"

# Delete this format's old stage and archive first. The sibling AppImage belongs
# to the next packaging step and must survive.
rm -rf dist/agentty-*-linux-"${ARCH}" dist/agentty-*-linux-"${ARCH}".tar.gz
bash script/check_bundled_remote_helpers \
  "target/${TARGET}/release/tty7-server" bundled-server
mkdir -p "$STAGE"

cp "target/${TARGET}/release/tty7-app" "$STAGE/tty7-app"
chmod +x "$STAGE/tty7-app"
cp "target/${TARGET}/release/tty7-server" "$STAGE/tty7-server"
chmod +x "$STAGE/tty7-server"
# The CLI ships beside the GUI, which symlinks it onto PATH at launch (see
# core::cli_install) by resolving it relative to its own executable.
cp "target/${TARGET}/release/tty7" "$STAGE/tty7"
chmod +x "$STAGE/tty7"
# Release builds keep symbols (thin LTO, no profile strip); drop them here so
# the archive isn't ~100 MB of debug info.
strip "$STAGE/tty7-app" || echo "⚠️  strip unavailable — shipping unstripped binary"
strip "$STAGE/tty7-server" || echo "⚠️  strip unavailable — shipping unstripped server"
strip "$STAGE/tty7" || echo "⚠️  strip unavailable — shipping unstripped CLI"
mkdir -p "$STAGE/completions"
cp assets/completions/*.json "$STAGE/completions/"
# Managed SSH may target either Linux architecture regardless of the desktop
# host architecture, so every distribution carries both static helpers.
mkdir -p "$STAGE/server"
for asset in tty7-server-linux-x86_64-musl tty7-server-linux-aarch64-musl; do
  src="bundled-server/$asset"
  if [[ ! -f "$src" ]]; then
    echo "bundle-linux: missing required remote helper $src" >&2
    exit 1
  fi
  cp "$src" "$STAGE/server/$asset"
  chmod +x "$STAGE/server/$asset"
done
cp LICENSE "$STAGE/LICENSE"
cp README.md "$STAGE/README.md"

tar -C dist -czf "dist/${NAME}.tar.gz" "$NAME"
rm -rf "$STAGE"
echo "✅ dist/${NAME}.tar.gz"
