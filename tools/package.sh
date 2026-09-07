#!/bin/bash
# Build a relocatable Kiln release bundle.
#
#   tools/package.sh            -> dist/kiln-<version>-linux-x86_64{,.tar.gz}
#   tools/package.sh --no-tar   -> leave the tree, skip the archive
#
# The bundle mirrors the repository layout on purpose: `kiln` finds its
# runtime by walking up from its own executable, so `bin/kiln` resolving to
# `<root>/runtime` is what makes the tree relocatable with no extra code and no
# environment variables.
#
# NOTE ON THE WORD "RELEASE": this packages the toolchain and the IDE. The
# hardened profile for programs *users* build is a different thing entirely, and
# it is theirs to ask for: `kiln build --release`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# The workspace version is the product version. A git description would put
# a commit hash in the artefact name, which reads like a mistake on a download
# page; the commit is recorded in VERSION.commit instead.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
TRIPLE="linux-x86_64"
NAME="kiln-${VERSION}-${TRIPLE}"
OUT="$ROOT/dist/$NAME"

echo "==> Kiln $VERSION -> dist/$NAME"

# --- prerequisites --------------------------------------------------------
# Fail early and by name. A half-built bundle that fails at the link step is
# far more confusing than a missing-dependency message here.
for dep in cargo clang ar pkg-config; do
    command -v "$dep" >/dev/null || { echo "missing required tool: $dep" >&2; exit 1; }
done
if [ ! -f vendor/RmlUi/build/librmlui.a ]; then
    echo "RmlUi is not vendored — run tools/fetch-rmlui.sh" >&2
    exit 1
fi

# --- build ----------------------------------------------------------------
echo "==> compiler (release)"
cargo build --release --quiet

echo "==> studio"
./designer/build.sh >/dev/null

# --- assemble -------------------------------------------------------------
# A clean dist: every earlier version's tree and archives for THIS platform
# go, so dist/ holds only what this build produced — no stale bundle a
# release upload or a `ls dist` could pick up by mistake.
rm -rf "$ROOT"/dist/kiln-*-linux-x86_64 "$ROOT"/dist/kiln-*-linux-x86_64.tar.gz \
       "$ROOT"/dist/kiln-*-linux-x86_64.zip "$ROOT"/dist/kiln-*-linux-x86_64.*.sha256 \
       "$ROOT"/dist/kiln-*-linux-x86_64.tar.gz.sha256 "$ROOT"/dist/kiln-*-linux-x86_64.zip.sha256
mkdir -p "$OUT"/{bin,licenses}

install -m755 target/release/kiln "$OUT/bin/kiln"
install -m755 designer/kiln-designer "$OUT/bin/kiln-studio"
strip "$OUT/bin/kiln" "$OUT/bin/kiln-studio" 2>/dev/null || true

# The binary reports the version it was compiled with; the archive is named
# after Cargo.toml. A stale target/ makes them disagree, and a bundle whose
# `kiln version` contradicts its own file name is not one to ship.
REPORTED="$("$OUT/bin/kiln" version | sed -n 's/^kiln //p')"
if [ "$REPORTED" != "$VERSION" ]; then
    echo "bin/kiln reports $REPORTED but Cargo.toml says $VERSION" >&2
    exit 1
fi

# The runtime, ABI and support libraries ship as SOURCE: `kiln build`
# compiles and links them into each program, which is what makes dead-stripping
# per-command possible.
# `kits/` travels too, so the kits this repository ships are present in an
# install. They are not on the bundled search path — that is `libs/` — so a
# user adopts one with `kiln kit add <install>/kits/<name>`, the same way
# they would adopt anyone else's.
for d in runtime abi libs kits templates examples editors assets; do
    cp -r "$d" "$OUT/$d"
done

# Only user-facing documentation ships.
mkdir -p "$OUT/docs"
cp docs/editors.md "$OUT/docs/editors.md"

# The handbook travels with the bundle, twice over, because it has two readers.
#
#   docs/book/  the mdBook, for a browser. Its search index is loaded with a
#               <script> tag rather than fetch(), so it works from a file://
#               URL with no server — which is the whole point of shipping it.
#   docs/src/   the same pages as Markdown, for Studio's own help viewer. It
#               renders them itself; RmlUi is not a browser and cannot open the
#               HTML. One source, two renderings, no third copy to drift.
#
# Building here rather than committing book/ keeps the generated reference
# honest: it is whatever `tools/gen-docs.sh` last produced from the toolchain.
if ! command -v mdbook >/dev/null 2>&1; then
    echo "mdbook is not installed; the bundle would ship without its handbook" >&2
    echo "install it with: cargo install mdbook" >&2
    exit 1
fi
mdbook build docs-site >/dev/null
cp -r docs-site/book "$OUT/docs/book"
mkdir -p "$OUT/docs/src"
cp docs-site/src/*.md "$OUT/docs/src/"
cp -r docs-site/src/assets "$OUT/docs/src/assets"

# editors.md is an mdBook include of the copy in docs/, and Studio's viewer
# expands includes by reading relative to the page. Flattening it here means
# the viewer does not need to know where the bundle put docs/editors.md.
cp docs/editors.md "$OUT/docs/src/editors.md"

# GUI programs link the vendored UI stack, so a user building outside this
# repository needs its headers and static libraries too.
mkdir -p "$OUT/vendor/RmlUi/build" "$OUT/vendor/accesskit-c"
cp -r vendor/RmlUi/Include "$OUT/vendor/RmlUi/Include"
cp -r vendor/RmlUi/Backends "$OUT/vendor/RmlUi/Backends"
cp vendor/RmlUi/build/librmlui.a "$OUT/vendor/RmlUi/build/"
if [ -d vendor/accesskit-c/include ]; then
    cp -r vendor/accesskit-c/include "$OUT/vendor/accesskit-c/include"
    # Only the archive this platform actually links. accesskit-c vendors every
    # target it supports — Windows, Android, arm64 — which is 274 MB of code
    # this bundle can never use, and would quadruple the download.
    AK="lib/linux/x86_64/static"
    mkdir -p "$OUT/vendor/accesskit-c/$AK"
    cp vendor/accesskit-c/$AK/* "$OUT/vendor/accesskit-c/$AK/"
fi

# Licences: RmlUi and AccessKit are statically linked into what we ship, so
# their notices travel with it.
cp LICENSE "$OUT/LICENSE"
cp THIRD-PARTY.md "$OUT/THIRD-PARTY.md"
cp vendor/RmlUi/LICENSE.txt "$OUT/licenses/RmlUi-LICENSE.txt"
for f in vendor/accesskit-c/LICENSE-MIT vendor/accesskit-c/LICENSE-APACHE; do
    [ -f "$f" ] && cp "$f" "$OUT/licenses/accesskit-$(basename "$f").txt"
done

# Drop build artefacts that must not ship.
find "$OUT/examples" "$OUT/libs" "$OUT/kits" -name '*.o' -o -name '*.so' -o -name '*.ll' 2>/dev/null \
    | xargs -r rm -f

sed -e "s/__VERSION__/$VERSION/g" tools/bundle-README.md > "$OUT/README.md"
printf '%s\ncommit %s\n' "$VERSION" "$COMMIT" > "$OUT/VERSION"

# --- archive --------------------------------------------------------------
if [ "${1:-}" != "--no-tar" ]; then
    echo "==> archive"
    ( cd "$ROOT/dist" && tar czf "$NAME.tar.gz" "$NAME" )
    ( cd "$ROOT/dist" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256" )
fi

echo
echo "bundle:  dist/$NAME"
[ -f "$ROOT/dist/$NAME.tar.gz" ] && \
    echo "archive: dist/$NAME.tar.gz ($(du -h "$ROOT/dist/$NAME.tar.gz" | cut -f1))"
echo
echo "Verify it with: tools/verify-bundle.sh dist/$NAME"
