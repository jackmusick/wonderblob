# Wonderblob — Install Parity with Wondershot

_Design spec. 2026-06-08._

## Goal

Give wonderblob the same two-command install story wondershot has:

- **Linux:** `curl -fsSL …/install.sh | sh` — a thin bash wrapper that installs a
  published Flatpak bundle (`flatpak install --user`). Real menu integration,
  sandboxed, no sudo. Re-run updates in place.
- **Windows:** `irm …/Install-Wonderblob.ps1 | iex` — downloads the latest
  release NSIS installer and runs it silently, per-user, no admin. Re-run
  updates in place.

"Follow the same principles as wondershot" is the whole brief: consistent install
UX across the apps. No Flathub submission, no from-source Linux path (wondershot
only has one because it's interpreted Python; wonderblob is compiled).

## Background

- Wonderblob is a Tauri app: Rust backend + JS frontend, `webkit2gtk-4.1`,
  app-id `com.wonderblob.app`, productName/binary `wonderblob`.
- `release.yml` is tag-gated (`v*`), uses `tauri-action` on
  macos/ubuntu-22.04/windows, uploads installers (deb/rpm/appimage/dmg/msi/nsis)
  to a **draft prerelease**. Builds are unsigned.
- Wondershot's model: a single-file `wondershot.flatpak` on the Releases page
  (built from `packaging/flatpak/io.github.jackmusick.wondershot.yml`) +
  `Install-Wondershot.ps1` that downloads the latest Inno Setup `.exe` and runs
  it silently per-user. Wonderblob publishes **no flatpak today** — so Linux
  parity is more than a script.

## Components

### 1. `Install-Wonderblob.ps1` (Windows)

Near line-for-line port of `Install-Wondershot.ps1`:

- GET `https://api.github.com/repos/jackmusick/wonderblob/releases/latest`.
- Select the NSIS asset matching `*-setup.exe` (already produced by tauri-action).
- Download to `$env:TEMP`, run silently per-user with NSIS's `/S` flag
  (wondershot used Inno's `/VERYSILENT /NORESTART /SUPPRESSMSGBOXES`; Tauri's NSIS
  installer is currentUser and accepts `/S`), `-Wait -PassThru`, check exit code,
  clean up the temp file.
- Same `Say` logging and the same guards: "no published release found" and
  "release has no setup asset".

Note: `releases/latest` excludes drafts/prereleases. The first wonderblob release
must be **published** (and ideally not flagged prerelease) for this to resolve —
documented as a release-process note, not a code change.

### 2. `packaging/flatpak/com.wonderblob.app.yml`

Repackages the `.deb` that `tauri build` already produces (no Rust/Node in the
sandbox):

- `app-id: com.wonderblob.app`
- `runtime: org.gnome.Platform` / `sdk: org.gnome.Sdk`, version pinned to a recent
  stable that ships `webkit2gtk-4.1` (verify at build time; e.g. `47`).
- `command: wonderblob`
- A single `simple`-buildsystem module whose source is the `.deb` (renamed to a
  stable filename in CI). Build commands: `ar x` the deb, unpack `data.tar.*`,
  install `usr/bin/wonderblob` → `/app/bin/`, the `.desktop` →
  `/app/share/applications/${FLATPAK_ID}.desktop` (rewriting `Icon=` to the
  app-id), and the icon → `/app/share/icons/hicolor/.../${FLATPAK_ID}.png`.
- `finish-args` (least-privilege):
  - `--share=network` — S3 / Azure / SFTP / OneDrive
  - `--share=ipc`, `--socket=wayland`, `--socket=fallback-x11`, `--device=dri` — GUI
  - `--talk-name=org.freedesktop.portal.OpenURI` — OneDrive browser sign-in and
    opening files in the user's editor (EditSession)
  - `--filesystem=xdg-download` — the "To Downloads" button writes `~/Downloads`
    with no dialog (open/save dialogs use the file-chooser portal, no broad grant)

**Risks to verify with a live flatpak build (untestable headless):**

1. **EditSession editor spawn.** If wonderblob launches the editor directly
   instead of via the OpenURI portal, it won't cross the sandbox. Mitigation:
   route through the portal, or grant a temp dir.
2. **OneDrive `wonderblob://auth` deep-link** returning into the sandboxed app —
   depends on the `.desktop` carrying `x-scheme-handler/wonderblob`. Mitigation:
   add the MimeType handler in the manifest if Tauri's `.desktop` lacks it.
3. **webkit ABI.** The deb links the ubuntu-22.04 `libwebkit2gtk-4.1`; the GNOME
   runtime must provide an ABI-compatible `.so`. Verify the app launches from the
   built bundle.

### 3. `install.sh` (Linux)

Thin `curl … | sh` wrapper; flatpak does the real work:

- POSIX `sh`, `set -eu`, `say`/`fail` helpers (match wondershot's style).
- Check `flatpak` is present; if not, print the dnf/apt one-liner and exit
  (check-don't-install — a piped script can't sudo safely).
- `flatpak remote-add --user --if-not-exists flathub
  https://flathub.org/repo/flathub.flatpakrepo` so the GNOME runtime resolves.
- Fetch latest release JSON, find the `wonderblob.flatpak` asset URL, download to
  a temp file, `flatpak install --user -y <file>` (re-run upgrades).
- Closing message: it's in the menu, or `flatpak run com.wonderblob.app`.

### 4. `release.yml`

Append steps to the existing **ubuntu-22.04** matrix leg, after `tauri-action`
(which leaves the `.deb` on disk and has already created the draft release):

- Install `flatpak` + `flatpak-builder`; add the flathub remote; install the
  pinned `org.gnome.Platform`/`Sdk`.
- Rename the produced `.deb` to a stable filename the manifest references.
- `flatpak-builder --repo=… build-dir packaging/flatpak/com.wonderblob.app.yml`,
  then `flatpak build-bundle … wonderblob.flatpak com.wonderblob.app`.
- `gh release upload ${{ github.ref_name }} wonderblob.flatpak --clobber`
  (`GH_TOKEN` from `secrets.GITHUB_TOKEN`).

Guarded `if: matrix.platform == 'ubuntu-22.04'`. Adds runtime-download minutes to
the Linux leg only.

### 5. `README.md`

Restructure **Install** to lead with the two commands (Linux Flatpak / Windows
PowerShell), mirroring wondershot. Keep the raw deb/rpm/dmg/AppImage links below
as the manual fallback. Keep the unsigned-build caveats.

## Out of scope (YAGNI)

- From-source `install.sh` for Linux.
- Flathub submission (wondershot hasn't either — noted "planned").
- Code-signing / notarization (already deferred post-v1).
- macOS one-liner (wondershot has none; `.dmg` drag-install stands).

## Success criteria

- `Install-Wonderblob.ps1` installs the app per-user from a published release and
  re-running updates it.
- A `v*` tag produces `wonderblob.flatpak` attached to the draft release.
- `install.sh` installs that bundle via flatpak (`flatpak run com.wonderblob.app`
  launches it) and re-running updates it.
- README leads with both one-liners.
- The three live risks above are checked against a real bundle before the install
  story is advertised as working.
