#!/bin/bash
# Prove a release bundle works away from the repository that built it.
#
#   tools/verify-bundle.sh dist/kiln-<version>-linux-x86_64
#   tools/verify-bundle.sh dist/kiln-<version>-linux-x86_64.tar.gz
#
# Everything runs from a scratch directory with KILN_RUNTIME_DIR unset. That
# variable is set by every test in the repo; leaving it set here would mask the
# exact relocation bugs this script exists to find.
set -uo pipefail

BUNDLE="${1:-}"
[ -n "$BUNDLE" ] || { echo "usage: verify-bundle.sh <bundle-dir|tarball>" >&2; exit 2; }
BUNDLE="$(cd "$(dirname "$BUNDLE")" && pwd)/$(basename "$BUNDLE")"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A relocated bundle, not the one in place: unpack or copy it somewhere else
# entirely, so nothing can resolve back to the source tree by accident.
if [ -d "$BUNDLE" ]; then
    cp -r "$BUNDLE" "$WORK/kiln"
else
    mkdir -p "$WORK/x" && tar xzf "$BUNDLE" -C "$WORK/x"
    mv "$WORK/x"/* "$WORK/kiln"
fi
BIN="$WORK/kiln/bin"

unset KILN_RUNTIME_DIR
mkdir -p "$WORK/project"
cd "$WORK/project"

pass=0
fail=0
check() {
    local name="$1"; shift
    if "$@" >/tmp/kiln_verify.log 2>&1; then
        printf '  %-52s PASS\n' "$name"
        pass=$((pass + 1))
    else
        printf '  %-52s FAIL\n' "$name"
        sed 's/^/      /' /tmp/kiln_verify.log | head -12
        fail=$((fail + 1))
    fi
}

echo "verifying $(basename "$BUNDLE") from $WORK/project"
echo

check "binaries are present and executable" test -x "$BIN/kiln" -a -x "$BIN/kiln-studio"
check "brand assets ship with the bundle" test -f "$WORK/kiln/assets/kiln-wordmark.png" \
    -a -f "$WORK/kiln/assets/kiln-icon.png"
check "licences ship with the bundle" test -f "$WORK/kiln/LICENSE" \
    -a -f "$WORK/kiln/licenses/RmlUi-LICENSE.txt"

# Offline documentation, in both of its forms. The HTML book is what a browser
# opens; the Markdown is what Studio's own viewer renders. A bundle missing
# either one is a bundle whose Help menu is a dead end.
check "the handbook ships as HTML" test -f "$WORK/kiln/docs/book/index.html" \
    -a -f "$WORK/kiln/docs/book/reference-commands.html"
check "the handbook ships as Markdown for Studio" \
    test -f "$WORK/kiln/docs/src/SUMMARY.md" \
    -a -f "$WORK/kiln/docs/src/language.md" \
    -a -f "$WORK/kiln/docs/src/reference-commands.md"
# mdBook's search index is a plain script, which is what lets it work from a
# file:// URL with no server. If a future mdBook switches it to fetch(), search
# silently dies in an offline bundle and this is where we find out.
check "the handbook's search index is offline-safe" bash -c \
    'ls "$1"/docs/book/searchindex*.js >/dev/null 2>&1 &&
     grep -q "createElement..script" "$1"/docs/book/searcher*.js' _ "$WORK/kiln"
# The generated reference is generated, so an empty one means gen-docs.sh did
# not run before packaging and the bundle documents nothing.
check "the shipped reference lists real commands" bash -c \
    'grep -q "print_text" "$1"/docs/src/reference-commands.md' _ "$WORK/kiln"

# Finds its own runtime with no environment variable — the whole point of the
# layout.
check "kiln locates its runtime unaided" bash -c \
    "'$BIN/kiln' templates | grep -q '^template: console-app'"

check "creates a console project" "$BIN/kiln" new console-app hello
check "builds it" "$BIN/kiln" build hello/main.kiln -o hello/app
check "the built program runs and prints" bash -c \
    "cd '$WORK/project' && ./hello/app | grep -q 'Hello from Kiln'"

check "creates a GUI project" "$BIN/kiln" new gui-app winapp
check "builds a GUI program (links the vendored UI stack)" \
    "$BIN/kiln" build winapp/main.kiln -o winapp/app

check "creates a shared library" "$BIN/kiln" new shared-library slib
check "builds a .so" "$BIN/kiln" build slib/main.kiln -o slib/libslib.so
check "the .so exports its subroutines" bash -c \
    "nm -D --defined-only '$WORK/project/slib/libslib.so' | grep -q ' T greet'"

check "creates a static library" "$BIN/kiln" new static-library alib
check "builds a .a" "$BIN/kiln" build alib/main.kiln -o alib/libalib.a

# The IDE, headless: it must find `kiln` beside itself and open a project.
check "studio opens a project" env KILN_DESIGNER_SCRIPT='view:code' \
    "$BIN/kiln-studio" "$WORK/project/hello/main.kiln"
check "studio's welcome screen creates and opens a project" env \
    KILN_DESIGNER_WELCOME_PICK=console-app KILN_DESIGNER_SCRIPT='view:code' \
    "$BIN/kiln-studio"

# The logo must be found from the unpacked location, not from a build tree.
check "studio finds its assets after relocation" bash -c \
    "KILN_DESIGNER_SCRIPT='view:code' '$BIN/kiln-studio' '$WORK/project/hello/main.kiln' 2>&1 \
     | grep -qi 'could not load texture' && exit 1 || exit 0"

# Studio must not litter the directory it was launched from.
check "studio leaves no scratch files behind" bash -c \
    "! test -f '$WORK/project/kiln_dotgrid.tga'"

# The frame length is computed, not written by hand: a wrong Content-Length
# makes the server hang rather than answer, and the test would be reporting its
# own arithmetic error as a product failure.
frame() { printf 'Content-Length: %d\r\n\r\n%s' "${#1}" "$1"; }

lsp_probe() {
    # A full lifecycle, not just initialize: closing stdin without a shutdown
    # makes the server exit non-zero, which `pipefail` would report as a
    # failure of the thing being tested rather than of how it was asked.
    {
        frame '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}'
        frame '{"jsonrpc":"2.0","method":"initialized","params":{}}'
        frame '{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}'
        frame '{"jsonrpc":"2.0","method":"exit"}'
    } | timeout 20 "$BIN/kiln" lsp 2>/dev/null > "$WORK/lsp.out"
    grep -q capabilities "$WORK/lsp.out"
}
check "the language server answers" lsp_probe

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
