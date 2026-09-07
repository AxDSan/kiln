#!/bin/bash
# Remove what building and running leaves behind.
#
#   tools/clean.sh            what would go, and nothing else
#   tools/clean.sh --yes      actually remove it
#   tools/clean.sh --yes --all   ...including target/ and dist/
#
# This exists because the root of a working tree quietly fills with programs.
# `kiln build x.kiln` with no `-o` writes the module name into the current
# directory, so running the examples by hand leaves `hello`, `mathlib`,
# `sugar_tour` and two dozen others at the root. They are gitignored — which is
# why nobody notices until there are forty of them.
#
# Nothing tracked by git is ever removed: every candidate is checked against
# the index first, so a mistake in a pattern here cannot delete your work.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

YES=0
ALL=0
for a in "$@"; do
    case "$a" in
        --yes|-y) YES=1 ;;
        --all)    ALL=1 ;;
        *) echo "usage: tools/clean.sh [--yes] [--all]" >&2; exit 2 ;;
    esac
done

removed=0
kept=0

# A file at the root with no extension is a compiled program: every real one
# either has a suffix or lives in a subdirectory. That is the same reasoning
# .gitignore's `/*` + `!/*.*` rules are built on.
drop() {
    local path="$1"
    [ -e "$path" ] || return 0
    # Tracked files are counted and left alone without comment: LICENSE has no
    # extension and lives at the root, so it matches every run and saying so
    # every run is noise.
    if git ls-files --error-unmatch "$path" >/dev/null 2>&1; then
        kept=$((kept + 1))
        return 0
    fi
    if [ "$YES" = 1 ]; then
        rm -rf -- "$path"
        printf '  rm     %s\n' "$path"
    else
        printf '  would  %s\n' "$path"
    fi
    removed=$((removed + 1))
}

echo "stray programs at the root"
while IFS= read -r f; do
    case "$(basename "$f")" in *.*) continue ;; esac
    drop "$f"
done < <(find . -maxdepth 1 -mindepth 1 -type f)

echo "toolchain and session scratch"
for f in ./*.ll ./*.o ./*.s ./.wf-*.js ./.directory; do drop "$f"; done

if [ "$ALL" = 1 ]; then
    echo "build output"
    for d in target dist docs-site/book _site designer/kiln-designer; do drop "./$d"; done
fi

echo
if [ "$YES" = 1 ]; then
    echo "  removed $removed, kept $kept tracked"
else
    echo "  $removed would be removed, $kept tracked and kept — pass --yes to do it"
fi
