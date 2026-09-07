#!/bin/bash
# Fetch + build the pinned RmlUi into vendor/ (gitignored). Run once.
# Pinned: this version is the one the UI layer is built and tested against.
# This is the Linux build; tools/build-rmlui-windows.sh builds the same
# checkout a second time with mingw-w64, for `kiln build --os windows`.
set -eu
VER=6.3
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/RmlUi"

if [ -f "$DEST/build/librmlui.a" ]; then echo "RmlUi $VER already built at $DEST"; exit 0; fi
mkdir -p "$ROOT/vendor"
[ -d "$DEST" ] || git clone --depth 1 --branch "$VER" https://github.com/mikke89/RmlUi.git "$DEST"
cmake -S "$DEST" -B "$DEST/build" -G Ninja -DCMAKE_BUILD_TYPE=Release \
      -DRMLUI_BACKEND=SDL_GL3 -DRMLUI_SAMPLES=OFF -DBUILD_SHARED_LIBS=OFF
cmake --build "$DEST/build"
echo "RmlUi $VER built at $DEST"
