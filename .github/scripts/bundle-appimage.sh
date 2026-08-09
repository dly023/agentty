#!/bin/bash
# Usage: bundle-appimage.sh <target-triple> <arch-label>
# Package the release binary into a self-contained AppImage:
#   dist/agentty-<version>-linux-<arch>.AppImage
#
# Unlike the bare tarball (bundle-linux.sh), this bundles the x11/wayland/xkb/
# fontconfig/freetype runtime libraries alongside the binary, so it launches on
# distros that don't ship the exact same set Ubuntu does — Fedora, Arch, etc.
# (glibc itself is NOT bundled — an AppImage still needs the host glibc to be
# >= the build machine's, so the release runner's Ubuntu sets the floor.)
#
# Completion signatures are loaded at runtime relative to the executable
# (<exe-dir>/completions — see terminal::signature), so they go beside the
# binary at usr/bin/completions inside the AppDir.
set -euo pipefail

TARGET="$1"
ARCH="$2"
# Anchored on `= "` — see the note in bundle-macos.sh: the root manifest leads
# with `version.workspace = true`, which a bare `^version` match would return
# verbatim as the "version" and bake into every asset filename.
VERSION="$(grep -m1 '^version = "' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
  echo "bundle-appimage: could not read a version from Cargo.toml (got '$VERSION')" >&2
  exit 1
fi
NAME="agentty-${VERSION}-linux-${ARCH}"

# Delete this format's old stage and package before downloading tools or copying
# inputs. The tarball step runs first, so cleanup must not wipe dist/ wholesale.
APPDIR="dist/AppDir"
rm -rf "$APPDIR" "dist/${NAME}.AppImage"

# AppImage tools need FUSE to self-mount; CI runners usually lack it, so extract
# and run instead. Harmless on machines that do have FUSE.
export APPIMAGE_EXTRACT_AND_RUN=1

# linuxdeploy is published per-arch; map Rust's arch label to its naming.
case "$ARCH" in
  x86_64) LD_ARCH=x86_64 ;;
  arm64 | aarch64) LD_ARCH=aarch64 ;;
  *) echo "unsupported arch for AppImage: $ARCH" >&2; exit 1 ;;
esac

TOOLS="$(mktemp -d)"
LINUXDEPLOY="$TOOLS/linuxdeploy-${LD_ARCH}.AppImage"
APPIMAGETOOL="$TOOLS/appimagetool-${LD_ARCH}.AppImage"
curl -fsSL -o "$LINUXDEPLOY" \
  "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-${LD_ARCH}.AppImage"
curl -fsSL -o "$APPIMAGETOOL" \
  "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${LD_ARCH}.AppImage"
chmod +x "$LINUXDEPLOY" "$APPIMAGETOOL"

mkdir -p "$APPDIR/usr/bin"

cp "target/${TARGET}/release/agentty-app" "$APPDIR/usr/bin/agentty-app"
chmod +x "$APPDIR/usr/bin/agentty-app"
cp "target/${TARGET}/release/agentty-server" "$APPDIR/usr/bin/agentty-server"
chmod +x "$APPDIR/usr/bin/agentty-server"

# The CLI, beside the GUI as everywhere else. Unlike the tarball, an AppImage is
# mounted at a fresh /tmp/.mount_XXXX per run, so `core::cli_install` must copy
# this onto PATH rather than symlink it — a link into the mount dies the moment
# the app exits. That branch keys off $APPIMAGE, which the runtime sets.
cp "target/${TARGET}/release/agentty" "$APPDIR/usr/bin/agentty"
chmod +x "$APPDIR/usr/bin/agentty"

# A desktop entry + icon are mandatory AppImage metadata; linuxdeploy places
# them and generates AppRun. Icon basename must match the desktop's Icon= key.
cat > "$TOOLS/agentty.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=agentty
Comment=A fast, native terminal
Exec=agentty-app
Icon=agentty
Categories=System;TerminalEmulator;
Terminal=false
StartupWMClass=agentty
DESKTOP
# linuxdeploy only accepts fixed icon resolutions (…256, 384, 512 — NOT the
# source's 1024), so downscale to 256×256.
convert assets/app-icon.png -resize 256x256 "$TOOLS/agentty.png"

# Phase 1 — populate the AppDir: copy in dependent libs (ldd + patchelf) and
# install the desktop/icon into their standard locations.
"$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --executable "$APPDIR/usr/bin/agentty-app" \
  --executable "$APPDIR/usr/bin/agentty-server" \
  --desktop-file "$TOOLS/agentty.desktop" \
  --icon-file "$TOOLS/agentty.png"

# Runtime-loaded completion specs live beside the binary (not bundled by
# linuxdeploy, which only tracks ELF deps), so drop them in after populate.
mkdir -p "$APPDIR/usr/bin/completions"
cp assets/completions/*.json "$APPDIR/usr/bin/completions/"
# `BundledServerBinary::discover` searches beside the running executable. Ship
# both remote target architectures because the local AppImage architecture does
# not constrain the managed SSH destination.
mkdir -p "$APPDIR/usr/bin/server"
for asset in agentty-server-linux-x86_64-musl agentty-server-linux-aarch64-musl; do
  src="bundled-server/$asset"
  if [[ ! -f "$src" ]]; then
    echo "bundle-appimage: missing required remote helper $src" >&2
    exit 1
  fi
  cp "$src" "$APPDIR/usr/bin/server/$asset"
  chmod +x "$APPDIR/usr/bin/server/$asset"
done

# Phase 2 — pack the finished AppDir. Done separately from linuxdeploy so the
# completions added above are included.
# Bundled musl helpers for both remote Linux arches live under usr/bin/server/,
# so appimagetool sees multiple ELF architectures and requires an explicit ARCH
# for the *desktop* host binary — not the helpers.
ARCH="$LD_ARCH" "$APPIMAGETOOL" "$APPDIR" "dist/${NAME}.AppImage"
chmod +x "dist/${NAME}.AppImage"
rm -rf "$APPDIR" "$TOOLS"
echo "✅ dist/${NAME}.AppImage"
