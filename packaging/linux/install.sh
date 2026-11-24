#!/bin/sh
# Installs Scroder from the release tarball.
#   ./install.sh            install for the current user (~/.local)
#   sudo ./install.sh --system   install for all users (/usr/local)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
if [ "${1:-}" = "--system" ]; then
    prefix=/usr/local
else
    prefix="${HOME}/.local"
fi

install -Dm755 "$here/scroder" "$prefix/bin/scroder"
install -Dm644 "$here/scroder.desktop" "$prefix/share/applications/scroder.desktop"
install -Dm644 "$here/scroder.png" "$prefix/share/icons/hicolor/256x256/apps/scroder.png"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$prefix/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q "$prefix/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Scroder installed to $prefix/bin/scroder"
if ! command -v ffmpeg >/dev/null 2>&1; then
    echo
    echo "Scroder needs FFmpeg. Install it with your package manager, e.g.:"
    echo "  sudo apt install ffmpeg        (Debian, Ubuntu, Mint)"
    echo "  sudo dnf install ffmpeg-free   (Fedora)"
    echo "  sudo pacman -S ffmpeg          (Arch)"
fi
case "${XDG_SESSION_TYPE:-}" in
    wayland)
        if ! command -v gst-launch-1.0 >/dev/null 2>&1; then
            echo
            echo "On Wayland Scroder also needs GStreamer with PipeWire support, e.g.:"
            echo "  sudo apt install gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-good"
        fi
        ;;
esac
