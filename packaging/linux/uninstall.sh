#!/bin/sh
# Removes a tarball installation of ScreenRec (use --system for /usr/local).
set -eu
if [ "${1:-}" = "--system" ]; then prefix=/usr/local; else prefix="${HOME}/.local"; fi
rm -f "$prefix/bin/screenrec" \
      "$prefix/share/applications/screenrec.desktop" \
      "$prefix/share/icons/hicolor/256x256/apps/screenrec.png"
echo "ScreenRec removed from $prefix"
