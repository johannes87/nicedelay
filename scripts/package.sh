#!/usr/bin/env bash
# Packages a release build with the LGPL FFmpeg libraries it was linked against, as
# separate files next to the program (so users can replace them, as the LGPL requires).
#
#   scripts/build-ffmpeg.sh && eval "$(scripts/build-ffmpeg.sh --env)"
#   cargo build --release
#   scripts/package.sh          # → dist/
#
# macOS: nicedelay.app in a .dmg · Windows: a .zip · Linux: a .tar.gz
# Every package carries the licences (needs cargo-about, see third-party-licenses.sh), and dist/ also gets the FFmpeg source with vcpkg's
# build recipe and patches (attach it to the release).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
FFMPEG_DIR=${FFMPEG_DIR:-$(scripts/build-ffmpeg.sh --env | sed "s/^export FFMPEG_DIR='\(.*\)'$/\1/")}
VCPKG_ROOT=${VCPKG_ROOT:-$ROOT/.vcpkg}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=$ROOT/dist
STAGE=$OUT/stage
rm -rf "$STAGE"
mkdir -p "$STAGE" "$OUT"
# The Rust crates, Skia and what Skia bundles (all compiled into the program).
scripts/third-party-licenses.sh "$STAGE/THIRD-PARTY-LICENSES.txt" >/dev/null

licenses() { # dir
  mkdir -p "$1"
  cp LICENSE "$1/LICENSE-nicedelay.txt"
  cp assets/fonts/OFL.txt "$1/LICENSE-Inter-font.txt"
  cp "$STAGE/THIRD-PARTY-LICENSES.txt" "$1/"
  cp "$FFMPEG_DIR/share/ffmpeg/copyright" "$1/LICENSE-FFmpeg.txt"
  [ -f "$FFMPEG_DIR/share/openssl/copyright" ] && cp "$FFMPEG_DIR/share/openssl/copyright" "$1/LICENSE-OpenSSL.txt"
  cat > "$1/README-FFmpeg.txt" <<EOF
nicedelay uses FFmpeg (https://ffmpeg.org) under the GNU Lesser General Public License
(LGPL) version 3 or later; see LICENSE-FFmpeg.txt. FFmpeg's libraries are the separate
avcodec, avformat, avutil, swresample and swscale files shipped with nicedelay, and you
may replace them with your own build of the same FFmpeg version.

The FFmpeg source code, with the configuration and patches used for this build, is
published alongside this release as ffmpeg-source.tar.gz:
https://github.com/johannes87/nicedelay/releases
EOF
}

case "$(uname -s)" in
Darwin)
  APP=$STAGE/nicedelay.app
  mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
  cp target/release/nicedelay "$APP/Contents/MacOS/"
  # The libraries the program needs, and the ones they need, by their @rpath names.
  todo=("$APP/Contents/MacOS/nicedelay")
  while [ ${#todo[@]} -gt 0 ]; do
    f=${todo[0]}
    todo=("${todo[@]:1}")
    for dep in $(otool -L "$f" | awk '/@rpath\//{print $1}'); do
      name=${dep#@rpath/}
      if [ ! -f "$APP/Contents/Frameworks/$name" ]; then
        cp "$FFMPEG_DIR/lib/$name" "$APP/Contents/Frameworks/"
        chmod u+w "$APP/Contents/Frameworks/$name"
        todo+=("$APP/Contents/Frameworks/$name")
      fi
    done
  done
  # Only the bundle's own Frameworks folder, not the build machine's paths.
  for rp in $(otool -l "$APP/Contents/MacOS/nicedelay" | awk '/LC_RPATH/{getline; getline; print $2}'); do
    [ "$rp" = "@executable_path/../Frameworks" ] || install_name_tool -delete_rpath "$rp" "$APP/Contents/MacOS/nicedelay"
  done
  cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>nicedelay</string>
  <key>CFBundleDisplayName</key><string>nicedelay</string>
  <key>CFBundleIdentifier</key><string>io.github.johannes87.nicedelay</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>nicedelay</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
  licenses "$APP/Contents/Resources/licenses"
  # Ad-hoc signature (Apple Silicon refuses unsigned code). Releases for others need a
  # Developer ID signature and notarization on top.
  codesign --force --deep --sign - "$APP"
  ARCH=$(uname -m)
  rm -f "$OUT/nicedelay-$VERSION-macos-$ARCH.dmg"
  hdiutil create -quiet -volname nicedelay -srcfolder "$APP" -ov -format UDZO "$OUT/nicedelay-$VERSION-macos-$ARCH.dmg"
  echo "$OUT/nicedelay-$VERSION-macos-$ARCH.dmg"
  ;;
Linux)
  DIR=$STAGE/nicedelay
  mkdir -p "$DIR/lib"
  cp target/release/nicedelay "$DIR/"
  # FFmpeg and the libraries vcpkg built with it; libva and the GPU drivers stay the system's.
  for f in "$FFMPEG_DIR"/lib/lib{avcodec,avformat,avutil,swresample,swscale,ssl,crypto,vpl}.so*; do
    [ -e "$f" ] && cp -P "$f" "$DIR/lib/"
  done
  licenses "$DIR/licenses"
  tar -C "$STAGE" -czf "$OUT/nicedelay-$VERSION-linux-x86_64.tar.gz" nicedelay
  echo "$OUT/nicedelay-$VERSION-linux-x86_64.tar.gz"
  ;;
MINGW* | MSYS* | CYGWIN*)
  DIR=$STAGE/nicedelay
  mkdir -p "$DIR"
  cp target/release/nicedelay.exe "$DIR/"
  cp "$FFMPEG_DIR"/bin/*.dll "$DIR/"
  licenses "$DIR/licenses"
  (cd "$STAGE" && powershell -NoProfile -Command "Compress-Archive -Force -Path nicedelay -DestinationPath '$(cygpath -w "$OUT/nicedelay-$VERSION-windows-x86_64.zip")'")
  echo "$OUT/nicedelay-$VERSION-windows-x86_64.zip"
  ;;
esac

# FFmpeg's source as built: the release vcpkg builds (checked against its SHA-512) plus
# vcpkg's port (the build recipe and patches).
if [ ! -f "$OUT/ffmpeg-source.tar.gz" ]; then
  PORT=$VCPKG_ROOT/ports/ffmpeg
  FFV=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$PORT/vcpkg.json" | head -1)
  SHA=$(sed -n 's/^ *SHA512 \([0-9a-f]*\)$/\1/p' "$PORT/portfile.cmake" | head -1)
  mkdir -p "$STAGE/ffmpeg-source"
  TAR=$STAGE/ffmpeg-source/FFmpeg-n$FFV.tar.gz
  curl -fsSL -o "$TAR" "https://github.com/FFmpeg/FFmpeg/archive/refs/tags/n$FFV.tar.gz"
  GOT=$( (sha512sum "$TAR" 2>/dev/null || shasum -a 512 "$TAR") | cut -d' ' -f1)
  if [ "$GOT" != "$SHA" ]; then
    echo "error: FFmpeg n$FFV source doesn't match vcpkg's checksum" >&2
    exit 1
  fi
  cp -R "$PORT" "$STAGE/ffmpeg-source/vcpkg-port"
  cp -R vcpkg.json vcpkg-triplets "$STAGE/ffmpeg-source/"
  tar -C "$STAGE" -czf "$OUT/ffmpeg-source.tar.gz" ffmpeg-source
  echo "$OUT/ffmpeg-source.tar.gz"
fi
rm -rf "$STAGE"
