# Wonderblob installer/updater for Windows.
#
#   irm https://raw.githubusercontent.com/jackmusick/wonderblob/main/Install-Wonderblob.ps1 | iex
#
# Downloads the newest wonderblob NSIS setup from GitHub Releases and runs
# it silently (per-user, no admin). Re-running updates in place (same NSIS
# app id). The installer is not code-signed yet; this script downloads over
# HTTPS straight from this repo's Releases.

$ErrorActionPreference = "Stop"
$repo = "jackmusick/wonderblob"

function Say($msg) { Write-Host "[wonderblob] $msg" }

Say "looking up the latest release..."
try {
    $release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
} catch {
    Write-Error ("No published release found for $repo yet. " +
        "Check https://github.com/$repo/releases")
    return
}

# Tauri's NSIS bundle is named like wonderblob_<version>_x64-setup.exe
$asset = $release.assets | Where-Object { $_.name -like "*-setup.exe" } | Select-Object -First 1
if (-not $asset) {
    Write-Error "Release $($release.tag_name) has no NSIS *-setup.exe asset."
    return
}

$tmp = Join-Path $env:TEMP $asset.name
Say "downloading $($asset.name) ($([math]::Round($asset.size / 1MB)) MB)..."
Invoke-WebRequest $asset.browser_download_url -OutFile $tmp

Say "installing (silent, per-user)..."
# Tauri's NSIS installer is per-user (currentUser) and accepts /S for silent.
$p = Start-Process $tmp -ArgumentList "/S" -Wait -PassThru
if ($p.ExitCode -ne 0) {
    Write-Error "installer exited with code $($p.ExitCode)"
    return
}
Remove-Item $tmp -ErrorAction SilentlyContinue

Say "done — Wonderblob $($release.tag_name) is in your Start menu."
Say "update later by re-running this same command."
