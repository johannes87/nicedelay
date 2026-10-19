#!/usr/bin/env bash
# Writes the licences of everything compiled into nicedelay besides FFmpeg (which ships as
# separate libraries with its own licence file):
#   - the Rust crates, from Cargo.lock (cargo-about; build-only tools are left out)
#   - Skia and the libraries it bundles, fetched at the exact commits pinned for the Skia
#     version skia-bindings builds
#
#   scripts/third-party-licenses.sh THIRD-PARTY-LICENSES.txt
#
# Needs cargo-about (cargo install cargo-about --locked --features cli) and the network.
set -euo pipefail

OUT=${1:?usage: third-party-licenses.sh <output file>}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cargo about generate --fail about.hbs -o "$TMP/crates.txt"

# The Skia tag skia-bindings builds, from its Cargo.toml ([package.metadata] skia = "...").
SB_VERSION=$(awk '/^name = "skia-bindings"$/{getline; print $3}' Cargo.lock | tr -d '"')
SB_TOML=$(ls "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/"skia-bindings-$SB_VERSION/Cargo.toml" | head -1)
SKIA_TAG=$(sed -n 's/^skia = "\(.*\)"$/\1/p' "$SB_TOML")
curl -fsSL "https://raw.githubusercontent.com/rust-skia/skia/$SKIA_TAG/DEPS" -o "$TMP/DEPS"

# A file from a googlesource repository at a commit.
gs() { curl -fsSL "$1/+/$2/$3?format=TEXT" | base64 -d; }

# The repository and commit DEPS pins for a third_party/externals entry.
pinned() { sed -n "s|.*\"third_party/externals/$1\" *: *\"\([^@\"]*\)@\([0-9a-f]*\)\".*|\1 \2|p" "$TMP/DEPS"; }

section() { # title, then the licence text on stdin
  printf '\n================================================================================\n%s\n--------------------------------------------------------------------------------\n\n' "$1"
  cat
}

{
  echo "Third-party software in nicedelay"
  echo
  echo "FFmpeg (LGPL) and OpenSSL ship as separate libraries; their licences are in their own files."
  echo "The on-stream text uses the Inter font (SIL Open Font License, LICENSE-Inter-font.txt)."
  echo
  echo "Skia ($SKIA_TAG, via skia-bindings $SB_VERSION) and the libraries it includes:"
  curl -fsSL "https://raw.githubusercontent.com/rust-skia/skia/$SKIA_TAG/LICENSE" | section "Skia — BSD 3-Clause"
  # name in DEPS, licence file(s), title
  while read -r dep files title; do
    read -r repo commit <<<"$(pinned "$dep")"
    [ -n "$commit" ] || { echo "error: $dep isn't pinned in Skia's DEPS" >&2; exit 1; }
    for f in ${files//,/ }; do
      gs "$repo" "$commit" "$f" | section "$title ($f, $commit)"
    done
  done <<'EOF'
harfbuzz COPYING HarfBuzz
icu LICENSE ICU
libjpeg-turbo LICENSE.md,README.ijg libjpeg-turbo
libpng LICENSE libpng
zlib LICENSE zlib
expat expat/COPYING Expat
wuffs LICENSE Wuffs
EOF
  echo
  cat "$TMP/crates.txt"
} > "$OUT"
echo "$OUT"
