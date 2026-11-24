#!/bin/sh
# Removes a tarball installation of Scroder (use --system for /usr/local).
set -eu
if [ "${1:-}" = "--system" ]; then prefix=/usr/local; else prefix="${HOME}/.local"; fi
rm -f "$prefix/bin/scroder" \
      "$prefix/share/applications/scroder.desktop" \
      "$prefix/share/icons/hicolor/256x256/apps/scroder.png"
echo "Scroder removed from $prefix"
