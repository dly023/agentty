#!/bin/bash
# Usage: bundle-macos.sh <target-triple> <arch-label>
# Package the release binary into dist/Agentty.app and wrap it in a
# drag-to-Applications DMG: dist/agentty-<version>-macos-<arch>.dmg.
#
# Signing posture is chosen from the environment:
#   * Developer ID secrets present (APPLE_SIGNING_IDENTITY + APPLE_CERTIFICATE)
#     -> hardened-runtime signature, then notarize + staple. Passes Gatekeeper.
#   * Otherwise -> adhoc signature, same as before. Fine for local dev, but the
#     OS will quarantine it on other machines.
set -euo pipefail

TARGET="$1"
ARCH="$2"
# Anchored on `= "` because the root manifest's `[package]` section leads with
# `version.workspace = true` — a bare `^version` match grabs that line, finds no
# quotes to substitute, and passes it through as the "version", which then lands
# in CFBundleVersion and the .dmg filename. Guard against a silent recurrence.
VERSION="$(grep -m1 '^version = "' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
  echo "bundle-macos: could not read a version from Cargo.toml (got '$VERSION')" >&2
  exit 1
fi
# Keep the user-facing SemVer stable while carrying the exact source identity
# emitted by the staged native server. Git revision alone is insufficient for
# local packages: two dirty builds can share HEAD while linking different
# server source and speaking different provider vocabularies.
SERVER_PROTOCOL="$("target/${TARGET}/release/agentty-server" --protocol | awk 'NF { line = $0 } END { print line }')"
BUNDLE_VERSION="$(printf '%s\n' "$SERVER_PROTOCOL" | sed -nE 's/^\{"control":[0-9]+,"protocol":[0-9]+,"build":"([^"]+)"\}$/\1/p')"
if [[ ! "$BUNDLE_VERSION" =~ ^${VERSION}\+[[:alnum:]]+\.[0-9a-f]{16}$ ]]; then
  echo "bundle-macos: staged server returned an incomplete exact build identity: $SERVER_PROTOCOL" >&2
  exit 1
fi
APP="dist/Agentty.app"

# Always remove this target's old stage and package before touching new inputs.
# A failed package run must never leave an older DMG looking current.
rm -rf "$APP" "dist/dmg-stage" "dist/notarize.zip" "dist/agentty-${VERSION}-macos-${ARCH}.dmg"
bash script/check_bundled_remote_helpers \
  "target/${TARGET}/release/agentty-server" bundled-server
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/${TARGET}/release/agentty-app" "$APP/Contents/MacOS/agentty-app"
chmod +x "$APP/Contents/MacOS/agentty-app"
# Durable local panes are owned by the headless sibling, never by a GUI process
# relaunched under a hidden flag.
cp "target/${TARGET}/release/agentty-server" "$APP/Contents/MacOS/agentty-server"
chmod +x "$APP/Contents/MacOS/agentty-server"
# The CLI rides inside the bundle rather than beside it: a DMG is drag-to-
# Applications, so anything not in the .app never reaches the user's disk. The
# GUI symlinks it onto PATH at launch (see core::cli_install), which is why it
# sits next to agentty-app under MacOS/ — that is the directory the GUI resolves
# relative to its own executable.
cp "target/${TARGET}/release/agentty" "$APP/Contents/MacOS/agentty"
chmod +x "$APP/Contents/MacOS/agentty"
cp assets/agentty.icns "$APP/Contents/Resources/agentty.icns"
# Completion signatures are loaded at runtime (not embedded), resolved relative
# to the executable as ../Resources/completions — see terminal::signature.
mkdir -p "$APP/Contents/Resources/completions"
cp assets/completions/*.json "$APP/Contents/Resources/completions/"
# Both supported static Linux helpers travel inside the app. Managed SSH picks
# the target architecture after `uname -sm`; no published release is required
# for a freshly installed desktop build to enter a remote Environment.
SERVER_DIR="$APP/Contents/Resources/server"
mkdir -p "$SERVER_DIR"
for asset in agentty-server-linux-x86_64-musl agentty-server-linux-aarch64-musl; do
  src="bundled-server/$asset"
  if [[ ! -f "$src" ]]; then
    echo "bundle-macos: missing required remote helper $src" >&2
    exit 1
  fi
  cp "$src" "$SERVER_DIR/$asset"
  chmod +x "$SERVER_DIR/$asset"
done
printf 'APPL????' > "$APP/Contents/PkgInfo"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Agentty</string>
    <key>CFBundleDisplayName</key><string>Agentty</string>
    <key>CFBundleIdentifier</key><string>com.dly023.agentty</string>
    <key>CFBundleVersion</key><string>${BUNDLE_VERSION}</string>
    <key>CFBundleShortVersionString</key><string>${VERSION}</string>
    <key>CFBundleExecutable</key><string>agentty-app</string>
    <key>CFBundleIconFile</key><string>agentty</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

SIGN_ID="${APPLE_SIGNING_IDENTITY:-}"

if [[ -n "$SIGN_ID" && -n "${APPLE_CERTIFICATE:-}" ]]; then
    # ---- Developer ID signing ------------------------------------------------
    # Import the cert into a throwaway keychain so we never touch the login one.
    KEYCHAIN="${RUNNER_TEMP:-/tmp}/agentty-sign.keychain-db"
    CERT_PATH="${RUNNER_TEMP:-/tmp}/agentty-cert.p12"
    KEYCHAIN_PASSWORD="${KEYCHAIN_PASSWORD:-agentty-ci}"
    # Scrub the decoded cert + temp keychain on any exit path.
    cleanup() {
        security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
        rm -f "$CERT_PATH"
    }
    trap cleanup EXIT

    security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
    security set-keychain-settings -lut 21600 "$KEYCHAIN"
    security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
    echo "$APPLE_CERTIFICATE" | base64 --decode > "$CERT_PATH"
    security import "$CERT_PATH" -P "${APPLE_CERTIFICATE_PASSWORD:-}" \
        -A -t cert -f pkcs12 -k "$KEYCHAIN"
    security set-key-partition-list -S apple-tool:,apple:,codesign: \
        -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null
    security list-keychains -d user -s "$KEYCHAIN" login.keychain

    # Hardened runtime forbids JIT / unsigned executable memory by default; the
    # GPU/Metal path gpui uses needs them, so grant them explicitly or the
    # notarized build crashes on launch.
    ENTITLEMENTS="dist/entitlements.plist"
    cat > "$ENTITLEMENTS" <<'ENT'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.cs.allow-jit</key><true/>
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/>
    <key>com.apple.security.cs.disable-library-validation</key><true/>
</dict>
</plist>
ENT

    # Sign inner-out: the executables first, then the bundle. The CLI must be
    # signed explicitly — notarization rejects a bundle carrying an unsigned
    # Mach-O, and the outer `codesign "$APP"` does not descend into MacOS/ for
    # anything but CFBundleExecutable.
    #
    # It gets hardened runtime (notarization requires it) but none of the GUI's
    # entitlements: the JIT and library-validation exemptions exist for gpui's
    # Metal path, and a CLI that never renders anything has no business holding
    # them.
    codesign --force --options runtime --timestamp \
        --sign "$SIGN_ID" "$APP/Contents/MacOS/agentty"
    codesign --force --options runtime --timestamp \
        --sign "$SIGN_ID" "$APP/Contents/MacOS/agentty-server"
    codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_ID" "$APP/Contents/MacOS/agentty-app"
    codesign --force --options runtime --timestamp --entitlements "$ENTITLEMENTS" \
        --sign "$SIGN_ID" "$APP"
    codesign --verify --strict --verbose=2 "$APP"

    # ---- Notarization --------------------------------------------------------
    if [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
        # Submit a zip of the .app; on success staple the ticket onto the bundle
        # so it validates offline (the distributed zip below then carries it).
        ditto -c -k --keepParent "$APP" "dist/notarize.zip"
        xcrun notarytool submit "dist/notarize.zip" \
            --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
            --team-id "$APPLE_TEAM_ID" --wait
        xcrun stapler staple "$APP"
        rm -f "dist/notarize.zip"
        echo "✅ signed + notarized + stapled"
    else
        echo "⚠️  signed with Developer ID but notarization secrets missing — skipping notarize"
    fi
else
    echo "⚠️  no Developer ID secrets — adhoc signing (won't pass Gatekeeper on other machines)"
    codesign --force --deep --sign - "$APP"
fi

# Package the (now stapled) bundle as a drag-to-Applications DMG.
DMG="dist/agentty-${VERSION}-macos-${ARCH}.dmg"
STAGE="dist/dmg-stage"
rm -rf "$STAGE"
mkdir "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Agentty" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"
if [[ -n "$SIGN_ID" && -n "${APPLE_CERTIFICATE:-}" ]]; then
    codesign --force --timestamp --sign "$SIGN_ID" "$DMG"
fi
# The .app is an intermediate signing/staging input, not a second installable
# desktop copy. Leave the DMG as the handoff artifact so Launch Services cannot
# register a stale dist/Agentty.app beside /Applications/Agentty.app.
rm -rf "$APP"
echo "✅ $DMG"
