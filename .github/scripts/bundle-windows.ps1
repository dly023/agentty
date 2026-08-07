# Usage: bundle-windows.ps1 <target-triple> <arch-label>
# Package the release binary twice from one staged payload:
#   dist/agentty-<version>-windows-<arch>.zip        portable (unzip anywhere)
#   dist/agentty-<version>-windows-<arch>-setup.exe  Inno Setup installer
#     (Program Files or per-user, Start Menu shortcut, "Apps" uninstall entry)
#
# Fonts are embedded via include_bytes! and the app icon is compiled into the
# executable as a resource (see build.rs). So the payload is agentty-app.exe plus a
# sibling completions\ dir (loaded at runtime — see terminal::signature) and the
# license/readme. Both artifacts are unsigned builds — SmartScreen will
# warn on first launch.
$ErrorActionPreference = 'Stop'

$Target = $args[0]
$Arch   = $args[1]
$Version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$Name  = "agentty-$Version-windows-$Arch"
$Stage = "dist/$Name"

# Delete this target's old stage and both final packages before copying new
# inputs. A failed package run must not leave older artifacts looking current.
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
Remove-Item -Force "dist/$Name.zip", "dist/$Name-setup.exe" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null

Copy-Item "target/$Target/release/agentty-app.exe" "$Stage/agentty-app.exe"
Copy-Item "target/$Target/release/agentty-server.exe" "$Stage/agentty-server.exe"
# The CLI, staged beside the GUI so both the zip and the installer carry it.
# `core::cli_install` resolves it relative to agentty-app.exe and puts that
# directory on the user's PATH.
Copy-Item "target/$Target/release/agentty.exe" "$Stage/agentty.exe"
New-Item -ItemType Directory -Force -Path "$Stage/completions" | Out-Null
Copy-Item "assets/completions/*.json" "$Stage/completions/"
Copy-Item LICENSE "$Stage/LICENSE.txt"
Copy-Item README.md "$Stage/README.md"

# Both supported static Linux helpers are staged beside the client. WSL uses
# x86_64 today, while managed SSH can target either architecture; package
# completeness must not depend on a pre-existing GitHub Release.
$ServerAssets = @(
    "agentty-server-linux-x86_64-musl",
    "agentty-server-linux-aarch64-musl"
)
New-Item -ItemType Directory -Force -Path "$Stage/server" | Out-Null
foreach ($ServerAsset in $ServerAssets) {
    $ServerSrc = "bundled-server/$ServerAsset"
    if (-not (Test-Path $ServerSrc)) {
        throw "missing required remote helper $ServerSrc"
    }
    Copy-Item $ServerSrc "$Stage/server/$ServerAsset"
    Write-Host "OK bundled $ServerAsset"
}

Compress-Archive -Path "$Stage/*" -DestinationPath "dist/$Name.zip" -Force

# Installer, built from the same staged payload. ISCC is on PATH on GitHub's
# windows-latest image; fall back to the default install location.
$Iscc = (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
if (-not $Iscc) { $Iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
& $Iscc `
    "/DAppVersion=$Version" `
    "/DStageDir=$((Resolve-Path $Stage).Path)" `
    "/DOutputDir=$((Resolve-Path dist).Path)" `
    "/DOutputName=$Name-setup" `
    .github/scripts/windows-installer.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC exited with $LASTEXITCODE" }

Remove-Item -Recurse -Force $Stage
Write-Host "OK dist/$Name.zip"
Write-Host "OK dist/$Name-setup.exe"
