# Wonderblob Install Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give wonderblob wondershot's two-command install story — `curl … | sh` (Flatpak) on Linux, `irm … | iex` (release installer) on Windows — plus the Flatpak packaging needed to make the Linux command real.

**Architecture:** A Windows PowerShell installer ports wondershot's verbatim. On Linux, a Flatpak manifest *repackages the `.deb`* that `tauri build` already emits (against `org.gnome.Platform`, which ships `webkit2gtk-4.1`, so no Rust/Node in the sandbox); `release.yml` builds and attaches `wonderblob.flatpak` to the draft release; a thin `install.sh` downloads and `flatpak install`s it.

**Tech Stack:** Tauri (Rust + JS), Flatpak / flatpak-builder, GNOME runtime, GitHub Actions, PowerShell, POSIX sh.

**Reference files (read before starting):**
- Spec: `docs/superpowers/specs/2026-06-08-wonderblob-install-parity-design.md`
- Wondershot installers to mirror: `../wondershot/Install-Wondershot.ps1`, `../wondershot/install.sh`, `../wondershot/packaging/flatpak/io.github.jackmusick.wondershot.yml`
- Current CI: `.github/workflows/release.yml`
- App identity: `src-tauri/tauri.conf.json` (`productName: wonderblob`, `identifier: com.wonderblob.app`)

---

### Task 1: Windows installer — `Install-Wonderblob.ps1`

**Files:**
- Create: `Install-Wonderblob.ps1`

- [ ] **Step 1: Write the script**

Create `Install-Wonderblob.ps1` with exactly this content:

```powershell
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
```

- [ ] **Step 2: Verify it parses**

Run: `pwsh -NoProfile -Command "$null = [System.Management.Automation.Language.Parser]::ParseFile('Install-Wonderblob.ps1', [ref]$null, [ref]$null); Write-Host 'parse OK'"`
Expected: `parse OK` (no parser errors).

- [ ] **Step 3: Commit**

```bash
git add Install-Wonderblob.ps1
git commit -m "feat(install): Windows PowerShell installer (mirrors wondershot)"
```

---

### Task 2: Flatpak manifest — `packaging/flatpak/com.wonderblob.app.yml`

**Files:**
- Create: `packaging/flatpak/com.wonderblob.app.yml`

- [ ] **Step 1: Write the manifest**

Create `packaging/flatpak/com.wonderblob.app.yml` with this content. It repackages
the `.deb` produced by `tauri build` (the CI renames it to `wonderblob.deb`
alongside this manifest before building):

```yaml
# Flatpak manifest — the Linux bundle (one install, real menu integration).
#
# Local build + run (needs the deb next to this file, named wonderblob.deb):
#   cp src-tauri/target/release/bundle/deb/wonderblob_*_amd64.deb \
#     packaging/flatpak/wonderblob.deb
#   flatpak-builder --user --install --force-clean build-dir \
#     packaging/flatpak/com.wonderblob.app.yml
#   flatpak run com.wonderblob.app
#
# Unlike a from-source manifest, this does NOT compile Rust/JS in the sandbox —
# it unpacks the prebuilt .deb and relies on org.gnome.Platform for
# webkit2gtk-4.1. Verify the runtime-version below actually ships webkit2gtk
# (bump if the app fails to launch from the built bundle).
app-id: com.wonderblob.app
runtime: org.gnome.Platform
runtime-version: '47'
sdk: org.gnome.Sdk
command: wonderblob

finish-args:
  - --share=ipc
  - --socket=wayland
  - --socket=fallback-x11
  - --device=dri
  # Every backend talks to the network: S3, Azure Blob, SFTP, OneDrive (Graph).
  - --share=network
  # OneDrive sign-in opens the browser; opening a file in the user's editor
  # (EditSession) also rides OpenURI.
  - --talk-name=org.freedesktop.portal.OpenURI
  # "To Downloads" writes ~/Downloads directly (no save dialog). Regular
  # open/save dialogs go through the file-chooser portal — no broad fs grant.
  - --filesystem=xdg-download

modules:
  - name: wonderblob
    buildsystem: simple
    build-commands:
      # Unpack the Debian package: ar -> data.tar.* -> usr/ tree.
      - ar x wonderblob.deb
      - mkdir -p deb-root
      - tar -xf data.tar.* -C deb-root
      # Binary.
      - install -Dm755 deb-root/usr/bin/wonderblob ${FLATPAK_DEST}/bin/wonderblob
      # Desktop file, renamed to the app-id with Icon= pointed at the app-id.
      - |
        src_desktop=$(ls deb-root/usr/share/applications/*.desktop | head -n1)
        sed 's/^Icon=.*/Icon=com.wonderblob.app/' "$src_desktop" \
          > ${FLATPAK_ID}.desktop
        install -Dm644 ${FLATPAK_ID}.desktop \
          ${FLATPAK_DEST}/share/applications/${FLATPAK_ID}.desktop
      # Icons: copy every size/format the deb shipped, renamed to the app-id.
      - |
        for icon in $(find deb-root/usr/share/icons -type f); do
          dir=$(dirname "$icon" | sed 's|deb-root/usr/share/icons|share/icons|')
          ext="${icon##*.}"
          install -Dm644 "$icon" "${FLATPAK_DEST}/${dir}/${FLATPAK_ID}.${ext}"
        done
    sources:
      - type: file
        path: wonderblob.deb
```

- [ ] **Step 2: Verify it is valid YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('packaging/flatpak/com.wonderblob.app.yml')); print('yaml OK')"`
Expected: `yaml OK`

- [ ] **Step 3 (optional, needs flatpak-builder + a built deb): local bundle smoke build**

Only if `flatpak-builder` is installed (`sudo dnf install flatpak-builder`) and you
have a local deb. This is the real test of the three spec risks (webkit ABI,
EditSession editor spawn, OneDrive deep-link):

```bash
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak install --user -y flathub org.gnome.Platform//47 org.gnome.Sdk//47
cp src-tauri/target/release/bundle/deb/wonderblob_*_amd64.deb packaging/flatpak/wonderblob.deb
flatpak-builder --user --install --force-clean build-dir packaging/flatpak/com.wonderblob.app.yml
flatpak run com.wonderblob.app
```
Expected: the app window launches. If it fails to start with a webkit/.so error,
bump `runtime-version`. Note any EditSession/OneDrive issues for the follow-up.
Clean up: `rm -f packaging/flatpak/wonderblob.deb` (gitignored, see Task 3).

- [ ] **Step 4: Commit**

```bash
git add packaging/flatpak/com.wonderblob.app.yml
git commit -m "feat(install): Flatpak manifest repackaging the Tauri .deb"
```

---

### Task 3: Gitignore the local-build deb artifact

**Files:**
- Modify: `.gitignore`

- [ ] **Step 1: Append the ignore rule**

Add this line to `.gitignore` (the manifest's local-build step drops a copied deb
here; it must never be committed):

```
packaging/flatpak/wonderblob.deb
```

- [ ] **Step 2: Verify it is ignored**

Run: `touch packaging/flatpak/wonderblob.deb && git check-ignore packaging/flatpak/wonderblob.deb && rm packaging/flatpak/wonderblob.deb`
Expected: prints `packaging/flatpak/wonderblob.deb` (confirming it is ignored).

- [ ] **Step 3: Commit**

```bash
git add .gitignore
git commit -m "chore: gitignore local flatpak build deb"
```

---

### Task 4: Linux installer — `install.sh`

**Files:**
- Create: `install.sh`

- [ ] **Step 1: Write the script**

Create `install.sh` with this content:

```sh
#!/bin/sh
# Wonderblob installer/updater for Linux (Flatpak bundle).
#
#   curl -fsSL https://raw.githubusercontent.com/jackmusick/wonderblob/main/install.sh | sh
#
# Downloads the latest wonderblob.flatpak from GitHub Releases and installs it
# per-user via flatpak (real menu integration, sandboxed, no sudo). Re-running
# updates in place. Flatpak itself is CHECKED, not installed — a piped script
# can't sudo safely; you get the exact command to run.
set -eu

REPO="jackmusick/wonderblob"
APP_ID="com.wonderblob.app"

say() { printf '\033[1m[wonderblob]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[wonderblob]\033[0m %s\n' "$*" >&2; exit 1; }

# -- dependency check ----------------------------------------------------------

if ! command -v flatpak >/dev/null 2>&1; then
    say "flatpak is required but not installed."
    if command -v dnf >/dev/null 2>&1; then
        say "install it with:  sudo dnf install flatpak"
    elif command -v apt-get >/dev/null 2>&1; then
        say "install it with:  sudo apt install flatpak"
    else
        say "install flatpak with your distro's package manager."
    fi
    fail "re-run this script once flatpak is installed"
fi

# The bundle's GNOME runtime is pulled from flathub at install time.
say "ensuring the flathub remote exists (user)"
flatpak remote-add --user --if-not-exists flathub \
    https://flathub.org/repo/flathub.flatpakrepo

# -- locate + download the bundle ----------------------------------------------

say "looking up the latest release..."
api="https://api.github.com/repos/$REPO/releases/latest"
url=$(curl -fsSL "$api" \
    | grep -o '"browser_download_url": *"[^"]*wonderblob\.flatpak"' \
    | head -n1 | cut -d'"' -f4)
[ -n "$url" ] || fail "no wonderblob.flatpak asset in the latest release of $REPO"

tmp=$(mktemp --suffix=.flatpak)
trap 'rm -f "$tmp"' EXIT
say "downloading wonderblob.flatpak"
curl -fsSL "$url" -o "$tmp"

# -- install -------------------------------------------------------------------

say "installing (per-user, no sudo)"
flatpak install --user -y "$tmp"

say "done. Launch Wonderblob from your menu, or: flatpak run $APP_ID"
say "  update later: re-run this same command"
```

- [ ] **Step 2: Make it executable**

Run: `chmod +x install.sh`

- [ ] **Step 3: Verify it parses and lints clean**

Run: `sh -n install.sh && shellcheck install.sh && echo "shell OK"`
Expected: `shell OK` (no syntax errors, no shellcheck warnings). If shellcheck
flags `SC2312`/style-only notes, address or `# shellcheck disable=` with a reason.

- [ ] **Step 4: Commit**

```bash
git add install.sh
git commit -m "feat(install): Linux install.sh (downloads + installs the flatpak)"
```

---

### Task 5: Wire the flatpak build into `release.yml`

**Files:**
- Modify: `.github/workflows/release.yml` (append steps after the `tauri-apps/tauri-action@v0` step)

- [ ] **Step 1: Add the flatpak build+upload steps**

In `.github/workflows/release.yml`, immediately after the `- uses: tauri-apps/tauri-action@v0`
step block (the last step in the file), append these steps at the same indentation
(they run on the ubuntu leg only; tauri-action has already left the deb on disk and
created the draft release):

```yaml
      - name: Build & attach Flatpak (Linux only)
        if: matrix.platform == 'ubuntu-22.04'
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        run: |
          set -eux
          sudo apt-get install -y flatpak flatpak-builder
          flatpak remote-add --user --if-not-exists flathub \
            https://flathub.org/repo/flathub.flatpakrepo
          flatpak install --user -y flathub org.gnome.Platform//47 org.gnome.Sdk//47

          # tauri-action left the deb here; the manifest expects it as wonderblob.deb.
          deb=$(ls src-tauri/target/release/bundle/deb/wonderblob_*_amd64.deb | head -n1)
          cp "$deb" packaging/flatpak/wonderblob.deb

          flatpak-builder --user --force-clean --repo=fp-repo \
            fp-build packaging/flatpak/com.wonderblob.app.yml
          flatpak build-bundle fp-repo wonderblob.flatpak com.wonderblob.app

          gh release upload "${{ github.ref_name }}" wonderblob.flatpak --clobber
```

- [ ] **Step 2: Verify the workflow is valid YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml')); print('yaml OK')"`
Expected: `yaml OK`

- [ ] **Step 3: Lint the workflow if actionlint is available (optional)**

Run: `command -v actionlint >/dev/null && actionlint .github/workflows/release.yml && echo "actionlint OK" || echo "actionlint not installed — skipping"`
Expected: `actionlint OK` or the skip message.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): build and attach wonderblob.flatpak on tag"
```

---

### Task 6: README — lead with the two one-liners

**Files:**
- Modify: `README.md` (the `## Install` section, currently around lines 33-41)

- [ ] **Step 1: Replace the Install intro**

Read `README.md` first to confirm current line numbers. Replace the block that
currently reads:

```markdown
## Install

Download the asset for your platform from the
[Releases](https://github.com/jackmusick/wonderblob/releases) page:

- **Linux** — `.AppImage` (`chmod +x Wonderblob_*.AppImage && ./Wonderblob_*.AppImage`),
  or the `.deb` / `.rpm` for your distro.
- **macOS** — the `.dmg` (arm64 or x86_64). Drag Wonderblob to Applications.
- **Windows** — the `.msi` or the NSIS `*-setup.exe`.
```

with:

```markdown
## Install

### Linux (Flatpak)

```sh
curl -fsSL https://raw.githubusercontent.com/jackmusick/wonderblob/main/install.sh | sh
```

Installs the latest release per-user via Flatpak (real menu integration,
sandboxed, no sudo). Re-run the same command to update. Requires `flatpak` —
the script tells you the one-liner to install it if it's missing.

### Windows

```powershell
irm https://raw.githubusercontent.com/jackmusick/wonderblob/main/Install-Wonderblob.ps1 | iex
```

Downloads the latest NSIS installer and runs it silently, per-user, no admin.
Re-run to update. The installer isn't code-signed yet, so SmartScreen will
interject — see the caveats below.

### Manual downloads

Or grab the asset for your platform from the
[Releases](https://github.com/jackmusick/wonderblob/releases) page:

- **Linux** — `wonderblob.flatpak` (`flatpak install --user wonderblob.flatpak`),
  the `.AppImage` (`chmod +x Wonderblob_*.AppImage && ./Wonderblob_*.AppImage`),
  or the `.deb` / `.rpm` for your distro.
- **macOS** — the `.dmg` (arm64 or x86_64). Drag Wonderblob to Applications.
- **Windows** — the `.msi` or the NSIS `*-setup.exe`.
```

- [ ] **Step 2: Verify the section renders (no broken fences)**

Run: `grep -n "## Install" README.md && grep -c '```' README.md`
Expected: the Install heading is present and the count of ``` fences is even
(balanced code blocks).

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(readme): lead Install with Flatpak + PowerShell one-liners"
```

---

### Task 7: Final verification & handoff notes

**Files:**
- Modify: `RESUME.md` (append a note about the new install path + the untested risks)

- [ ] **Step 1: Confirm all artifacts exist and are wired**

Run:
```bash
ls -1 install.sh Install-Wonderblob.ps1 packaging/flatpak/com.wonderblob.app.yml \
  && grep -q "wonderblob.flatpak" .github/workflows/release.yml \
  && grep -q "install.sh | sh" README.md \
  && echo "all wired"
```
Expected: the three filenames list cleanly and `all wired` prints.

- [ ] **Step 2: Append the post-release verification note to RESUME.md**

Add this under a new heading in `RESUME.md` (so the manual-only checks aren't lost):

```markdown
## Install scripts — verify on first real release

The Flatpak/PowerShell install path (spec
`docs/superpowers/specs/2026-06-08-wonderblob-install-parity-design.md`) ships
unverified end-to-end — it needs a published release to test:

1. **`releases/latest` needs a PUBLISHED, non-prerelease release.** The current
   `release.yml` produces a *draft prerelease*; `Install-Wonderblob.ps1` and
   `install.sh` both query `/releases/latest`, which skips drafts/prereleases.
   Publish (and unflag prerelease on) the first release before advertising the
   one-liners.
2. **Flatpak live checks (can't be done headless):** after the tag build,
   `flatpak install --user wonderblob.flatpak` then `flatpak run com.wonderblob.app`
   — confirm (a) the window launches (webkit ABI vs GNOME runtime 47),
   (b) OneDrive sign-in's `wonderblob://auth` deep-link returns into the app,
   (c) EditSession opens a file in the editor and saves back. Fixes if not:
   bump `runtime-version`, add `x-scheme-handler/wonderblob` MimeType to the
   manifest desktop file, route the editor spawn through OpenURI.
3. **Windows:** `irm … | iex` on a real Windows box; confirm per-user silent
   install and that re-running updates in place.
```

- [ ] **Step 3: Commit**

```bash
git add RESUME.md
git commit -m "docs(resume): note install-script verification owed on first release"
```

---

## Self-Review

**Spec coverage:**
- Windows installer → Task 1 ✓
- Flatpak manifest (repackage deb, org.gnome.Platform, finish-args) → Task 2 ✓
- install.sh (flatpak wrapper) → Task 4 ✓
- release.yml flatpak build+upload → Task 5 ✓
- README restructure → Task 6 ✓
- Three live risks flagged for verification → Task 2 Step 3 + Task 7 Step 2 ✓
- Out-of-scope items (no from-source install.sh, no Flathub, no macOS one-liner) → honored (not present) ✓
- Supporting: gitignore for the build deb → Task 3 (implied by manifest local-build step) ✓

**Placeholder scan:** No TBD/TODO; every file has full content; commands have expected output.

**Type/name consistency:** `com.wonderblob.app` (app-id), `wonderblob` (command/binary), `wonderblob.deb` (CI-renamed source), `wonderblob.flatpak` (bundle asset), `*-setup.exe` (NSIS asset), runtime `47` — used consistently across the manifest, install.sh, release.yml, and PowerShell.

**Note carried into execution:** the GNOME `runtime-version: '47'` is a best-guess for webkit2gtk-4.1 availability; Task 2 Step 3 / Task 7 Step 2 explicitly cover bumping it if the app won't launch.
