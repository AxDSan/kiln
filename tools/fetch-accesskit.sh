#!/bin/bash
# Fetch the prebuilt accesskit-c into vendor/ (gitignored). Run once.
# Pinned. Prebuilt avoids a cargo + cbindgen toolchain requirement.
set -eu
VER=0.22.3
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/vendor/accesskit-c"

# The archives ship with full Rust debug information — 43MB for linux-x86_64
# alone — and it is linked into every windowed program a user builds. A debug
# build keeps the *program's* debug info, which is what stepping needs; nobody
# is stepping into accesskit's internals, and 20MB of DWARF for a Hello World
# window is not a trade anyone asked for. Stripping the archives once here is
# the only place that fixes it for every build.
#
# Run on every invocation rather than only after a download, because a tree
# fetched before this existed is still carrying them. objcopy is idempotent.
strip_archives() {
    local n=0
    while IFS= read -r a; do
        case "$a" in
            *"/windows/"*) tool=x86_64-w64-mingw32-objcopy ;;
            *)             tool=objcopy ;;
        esac
        command -v "$tool" >/dev/null || tool=objcopy
        command -v "$tool" >/dev/null || return 0
        "$tool" --strip-debug "$a" 2>/dev/null && n=$((n+1))
    done < <(find "$DEST" -name '*.a' 2>/dev/null)
    [ "$n" -gt 0 ] && echo "stripped debug info from $n accesskit archive(s)"
    return 0
}

if [ -f "$DEST/include/accesskit.h" ]; then
    echo "accesskit-c $VER already present"
    strip_archives
    exit 0
fi
mkdir -p "$ROOT/vendor"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "downloading accesskit-c $VER ..."
curl -fsSL -o "$TMP/ak.zip" \
  "https://github.com/AccessKit/accesskit-c/releases/download/$VER/accesskit-c-$VER.zip"
unzip -q "$TMP/ak.zip" -d "$TMP"
mv "$TMP/accesskit-c-$VER" "$DEST"
strip_archives
echo "accesskit-c $VER at $DEST"
