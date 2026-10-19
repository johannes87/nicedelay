#!/usr/bin/env bash
# Renders the app icons from assets/icon/icon.svg (the logo in the window's corner) and
# icon-macos.svg (the logo on the rounded square macOS icons have):
#   icon.png         the window icon (Windows, Linux)
#   icon-macos.png   the Dock icon
#   icon.icns        the macOS app bundle's icon
#   icon.ico         the icon built into the Windows program
#
#   scripts/icons.sh
#
# Runs on macOS (iconutil) and needs rsvg-convert and ImageMagick (brew install librsvg imagemagick).
set -euo pipefail

DIR=$(cd "$(dirname "$0")/../assets/icon" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

rsvg-convert -w 1024 -h 1024 "$DIR/icon.svg" -o "$TMP/full.png"
rsvg-convert -w 1024 -h 1024 "$DIR/icon-macos.svg" -o "$TMP/macos.png"

magick "$TMP/full.png" -resize 256x256 "$DIR/icon.png"
magick "$TMP/macos.png" -resize 512x512 "$DIR/icon-macos.png"

mkdir "$TMP/icon.iconset"
for size in 16 32 128 256 512; do
  magick "$TMP/macos.png" -resize ${size}x$size "$TMP/icon.iconset/icon_${size}x$size.png"
  magick "$TMP/macos.png" -resize $((size * 2))x$((size * 2)) "$TMP/icon.iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$TMP/icon.iconset" -o "$DIR/icon.icns"

magick "$TMP/full.png" -define icon:auto-resize=256,64,48,32,24,16 "$DIR/icon.ico"
