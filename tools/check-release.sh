#!/bin/bash
# Every documentation surface a release can leave stale, checked in one command.
#
#   tools/check-release.sh
#
# A release went out with four of these wrong at once — the README counted 13
# kits when there were 16, the databases page described the synchronous behaviour
# the same release had just fixed, a solved limitation was still listed as one,
# and a port record still said "open, deliberately" about finished work. Three of
# those are prose that nothing can check. The four below are not, so they are
# checked here rather than trusted, and `RELEASING.md` carries the rest as a list
# a human has to walk.
#
# It does not run the test suite or build anything: it answers "is the paperwork
# consistent with the toolchain", in seconds, so it can be run before every
# commit as well as before every release.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
KILN="${KILN:-$ROOT/target/release/kiln}"
[ -x "$KILN" ] || KILN="$ROOT/target/debug/kiln"
[ -x "$KILN" ] || { echo "build kiln first (cargo build)" >&2; exit 1; }

pass=0
fail=0

ok()   { printf '  %-52s PASS\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  %-52s FAIL\n' "$1"; sed 's/^/      /' <<<"$2"; fail=$((fail + 1)); }

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[ -n "$VERSION" ] || { echo "no version in Cargo.toml" >&2; exit 1; }

# --- 1. the README's counts, which nothing else checks -----------------------
# The landing page's counts are held to the toolchain by check-docs.sh. The
# README's are held to nothing, which is exactly how they drifted by three kits
# and sixty commands.
kits=$("$KILN" kits | awk '/^kit: / && $4 == "bundled"' | wc -l)
cmds=$({ "$KILN" commands | sed -n 's/^command: //p'
         for k in $("$KILN" kits | awk '/^kit: / && $4 == "bundled" { print $2 }'); do
             "$KILN" commands --use "$k" | sed -n 's/^command: //p'
         done; } | sort -u | wc -l)
comps=$({ "$KILN" commands | sed -n 's/^component: //p'
          for k in $("$KILN" kits | awk '/^kit: / && $4 == "bundled" { print $2 }'); do
              "$KILN" commands --use "$k" | sed -n 's/^component: //p'
          done; } | sort -u | wc -l)

readme_counts="$(grep -o '[0-9]\+ bundled kits' README.md | head -1 | grep -o '[0-9]\+')"
if [ "$readme_counts" = "$kits" ]; then
    ok "README kit count ($kits)"
else
    bad "README kit count" "README says ${readme_counts:-missing}; the toolchain says $kits"
fi

readme_line="$(grep -o '[0-9]\+ commands and [0-9]\+ components' README.md | head -1)"
if [ "$readme_line" = "$cmds commands and $comps components" ]; then
    ok "README command and component counts ($cmds / $comps)"
else
    bad "README command and component counts" \
        "README says '${readme_line:-missing}'; the toolchain says '$cmds commands and $comps components'"
fi

# --- 2. the version, wherever a filename carries it --------------------------
# Only filenames: prose legitimately names older versions when it is recording
# history, and a port record that mentioned 1.0.1 was not stale for saying so.
stale_files="$(grep -rn --include='*.md' --include='*.html' --include='*.sh' \
                   -o 'kiln-[0-9]\+\.[0-9]\+\.[0-9]\+-[a-z0-9_-]*' . 2>/dev/null |
               grep -v '^\./target/\|^\./dist/\|^\./vendor/\|^\./\.git/' |
               grep -v "kiln-${VERSION}-" || true)"
if [ -z "$stale_files" ]; then
    ok "no file names a version other than $VERSION"
else
    bad "a doc names an older release" "$stale_files"
fi

readme_unpack="$(grep -o 'tar xzf kiln-[0-9]\+\.[0-9]\+\.[0-9]\+-linux-x86_64.tar.gz' README.md | head -1)"
if [ "$readme_unpack" = "tar xzf kiln-${VERSION}-linux-x86_64.tar.gz" ]; then
    ok "README's unpack line names $VERSION"
else
    bad "README's unpack line" "README says '${readme_unpack:-missing}'; Cargo.toml says $VERSION"
fi

# --- 3. every page is in the book -------------------------------------------
# A page written but not listed in SUMMARY.md is a page mdBook never renders and
# no reader ever finds — the failure is silent, which is why it is checked.
missing=""
for page in docs-site/src/*.md; do
    name="$(basename "$page")"
    case "$name" in
        SUMMARY.md|reference-*.md|cli.md) continue ;;
    esac
    # SUMMARY writes its links as `(./page.md)`, with the ./ — matching the bare
    # name passes nothing and fails every page, which is how this was written first.
    grep -q "(\./$name)" docs-site/src/SUMMARY.md || missing="$missing$name "
done
if [ -z "$missing" ]; then
    ok "every guide page is in SUMMARY.md"
else
    bad "a page is not in the book" "$missing"
fi

# --- 4. the generated pages are current --------------------------------------
# The reference is generated from the toolchain, so regenerating it must be a
# no-op. A diff here means a command, a component or a kit changed and the
# published reference does not say so yet.
before="$(git diff --stat -- docs-site/src/reference-commands.md docs-site/src/reference-components.md \
           docs-site/src/reference-templates.md docs-site/src/cli.md)"
tools/gen-docs.sh >/dev/null 2>&1
after="$(git diff --stat -- docs-site/src/reference-commands.md docs-site/src/reference-components.md \
          docs-site/src/reference-templates.md docs-site/src/cli.md)"
if [ "$before" = "$after" ]; then
    ok "the generated reference matches the toolchain"
else
    bad "the generated reference is stale" "run tools/gen-docs.sh and commit the result"
fi

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
