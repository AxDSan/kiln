#!/bin/bash
# Fetch + build the pinned mbedTLS into vendor/ (gitignored). Run once, and
# only if you want https.
#
# This one is OPTIONAL, unlike tools/fetch-rmlui.sh. Without it every build
# still works and `net` still speaks http; what is missing is https, which then
# fails at run time with KN_ERR_UNSUPPORTED and never downgrades itself to a
# plaintext request. `libs/net/lib.json` names the archives below under
# `optional_requires`, so the presence of this directory is the whole switch.
#
# Pinned to the 3.6 long-term-support line: security fixes for years, and no
# API churn under a program that was compiled last winter.
set -eu
VER=mbedtls-3.6.7
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/mbedtls"
LIB="$DEST/build/library"

if [ -f "$LIB/libmbedtls.a" ]; then echo "mbedTLS $VER already built at $DEST"; exit 0; fi
mkdir -p "$ROOT/vendor"
# --recurse-submodules: since 3.6.1 the build scripts live in a `framework`
# submodule, and a plain clone configures into an error about a missing file.
[ -d "$DEST" ] || git clone --depth 1 --branch "$VER" --recurse-submodules \
    https://github.com/Mbed-TLS/mbedtls.git "$DEST"

# Static and position-independent: Kiln links a program statically, and the
# same objects have to go into a shared library when the target is one.
cmake -S "$DEST" -B "$DEST/build" -DCMAKE_BUILD_TYPE=Release \
      -DENABLE_PROGRAMS=OFF -DENABLE_TESTING=OFF \
      -DUSE_STATIC_MBEDTLS_LIBRARY=ON -DUSE_SHARED_MBEDTLS_LIBRARY=OFF \
      -DCMAKE_POSITION_INDEPENDENT_CODE=ON
cmake --build "$DEST/build" -j "$(nproc 2>/dev/null || echo 4)"

echo "mbedTLS $VER built at $DEST"
echo "https is now available; rebuild any program that needs it."
