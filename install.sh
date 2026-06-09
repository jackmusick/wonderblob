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
