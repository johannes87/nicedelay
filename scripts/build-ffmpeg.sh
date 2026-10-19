#!/usr/bin/env bash
# Builds the LGPL FFmpeg libraries nicedelay links against, with vcpkg, from vcpkg.json.
# Works on macOS, Linux and Windows (Git Bash). Prints the FFMPEG_DIR to build with.
#
#   scripts/build-ffmpeg.sh            # build for this machine
#   eval "$(scripts/build-ffmpeg.sh --env)"   # ...and export FFMPEG_DIR for cargo
#
# vcpkg itself is cloned to $VCPKG_ROOT (default: rust/.vcpkg) at the baseline pinned in
# vcpkg.json, so every machine builds the same FFmpeg.
set -euo pipefail

HERE=$(cd "$(dirname "$0")/.." && pwd)
VCPKG_ROOT=${VCPKG_ROOT:-$HERE/.vcpkg}
INSTALL_ROOT=${FFMPEG_INSTALL_ROOT:-$HERE/.ffmpeg}
BASELINE=$(sed -n 's/.*"builtin-baseline": *"\([0-9a-f]*\)".*/\1/p' "$HERE/vcpkg.json")

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TRIPLET=arm64-osx-nicedelay ;;
  Darwin-x86_64) TRIPLET=x64-osx-nicedelay ;;
  Linux-x86_64) TRIPLET=x64-linux-nicedelay ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) TRIPLET=x64-windows-nicedelay ;;
  *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
TRIPLET=${VCPKG_TRIPLET:-$TRIPLET}

if [ "${1:-}" = "--env" ]; then
  echo "export FFMPEG_DIR='$INSTALL_ROOT/$TRIPLET'"
  exit 0
fi

if [ ! -d "$VCPKG_ROOT/.git" ]; then
  git clone --filter=blob:none https://github.com/microsoft/vcpkg.git "$VCPKG_ROOT" >&2
fi
if ! git -C "$VCPKG_ROOT" cat-file -e "$BASELINE^{commit}" 2>/dev/null; then
  git -C "$VCPKG_ROOT" fetch --filter=blob:none origin >&2
fi
git -C "$VCPKG_ROOT" -c advice.detachedHead=false checkout --quiet "$BASELINE"

if [ -f "$VCPKG_ROOT/vcpkg.exe" ] || [ -x "$VCPKG_ROOT/vcpkg" ]; then :
elif [ -f "$VCPKG_ROOT/bootstrap-vcpkg.bat" ] && [ "${TRIPLET#*windows}" != "$TRIPLET" ]; then
  cmd //c "$(cygpath -w "$VCPKG_ROOT/bootstrap-vcpkg.bat")" -disableMetrics >&2
else
  "$VCPKG_ROOT/bootstrap-vcpkg.sh" -disableMetrics >&2
fi
VCPKG=$VCPKG_ROOT/vcpkg
[ -f "$VCPKG.exe" ] && VCPKG=$VCPKG.exe

"$VCPKG" install \
  --x-manifest-root="$HERE" \
  --x-install-root="$INSTALL_ROOT" \
  --overlay-triplets="$HERE/vcpkg-triplets" \
  --overlay-ports="$HERE/vcpkg-ports" \
  --triplet="$TRIPLET" \
  --host-triplet="$TRIPLET" \
  --clean-after-build >&2

# Refuse anything that isn't LGPL: vcpkg records each installed feature as
# "Package: ffmpeg" followed by "Feature: <name>".
if awk '/^Package: /{p=$2} /^Feature: /{if (p=="ffmpeg" && ($2=="gpl" || $2=="nonfree")) bad=1} END{exit !bad}' \
  "$INSTALL_ROOT/vcpkg/status"; then
  echo "error: this FFmpeg build is GPL or nonfree, which nicedelay can't ship" >&2
  exit 1
fi
[ -f "$INSTALL_ROOT/$TRIPLET/include/libavcodec/avcodec.h" ] || {
  echo "error: FFmpeg headers missing after the build" >&2
  exit 1
}

echo "$INSTALL_ROOT/$TRIPLET"
