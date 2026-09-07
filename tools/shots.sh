#!/bin/bash
# Regenerate the screenshots the README, the handbook and the landing page use.
#
#   tools/shots.sh
#
# They go stale silently: nothing checks that a picture of the IDE still looks
# like the IDE, so a toolbar that changed six releases ago goes on being shown
# the way it was. This makes retaking them one command rather than a session of
# remembering which verbs produced which frame.
#
# Every run is headless — SDL's offscreen driver, so no window opens and
# nothing steals focus.
#
# The size is whatever the offscreen driver gives the window, deliberately.
# `winsize` can ask for a different one, but a size taller or wider than the
# offscreen display leaves the strip outside it unpainted, and the dump comes
# back with a black band where the title bar should be — which is exactly how
# the first run of this script produced four unusable shots. Set SHOT_SIZE only
# if you know it fits, and look at the result.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

STUDIO="$ROOT/designer/kiln-designer"
KILN="$ROOT/target/release/kiln"
[ -x "$STUDIO" ] || { echo "build the designer first — designer/build.sh" >&2; exit 1; }
[ -x "$KILN" ]   || { echo "build the compiler first — cargo build --release" >&2; exit 1; }

SIZE="${SHOT_SIZE:-}"
RESIZE=""
[ -n "$SIZE" ] && RESIZE="winsize:$SIZE;frame;"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The project is a COPY. Studio saves on exit, so opening a tracked example
# would land its own reformatting in the next commit — this has bitten before.
mkdir -p "$WORK/counter"
cp examples/counter.kiln "$WORK/counter/main.kiln"
cp templates/gui-app/app.png "$WORK/counter/app.png" 2>/dev/null || true

# A settings file of its own, so whatever the developer running this has set —
# a dark theme, a remembered window size — cannot leak into a published shot.
export XDG_CONFIG_HOME="$WORK/config"
mkdir -p "$XDG_CONFIG_HOME"

shot() {   # shot <name> <script>
    local name="$1" script="$2"
    local ppm="$WORK/$name.ppm"
    ( cd "$WORK/counter" && KILN_DESIGNER_DUMP="$ppm" KILN_DESIGNER_SCRIPT="$script" \
        "$STUDIO" "$KILN" main.kiln ) >"$WORK/$name.log" 2>&1 || true
    if [ ! -f "$ppm" ]; then
        echo "  $name  FAILED — see $WORK/$name.log" >&2
        cat "$WORK/$name.log" >&2
        return 1
    fi
    python3 - "$ppm" "assets/screenshot-$name.png" <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])
PY
    echo "  assets/screenshot-$name.png"
}

echo "==> screenshots${SIZE:+ at $SIZE}"
shot designer "${RESIZE}frame"
shot editor   "${RESIZE}view:code;frame"
shot settings "${RESIZE}settings:appearance;frame"

# The welcome screen is not a view the scripted session can switch to: it runs
# before a project is open, so it has a dump variable of its own and is taken
# by starting with no project at all.
( cd "$WORK" && KILN_DESIGNER_WELCOME_DUMP="$WORK/welcome.ppm" \
    "$STUDIO" "$KILN" ) >"$WORK/welcome.log" 2>&1 || true
if [ -f "$WORK/welcome.ppm" ]; then
    python3 - "$WORK/welcome.ppm" assets/screenshot-welcome.png <<'PY'
import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])
PY
    echo "  assets/screenshot-welcome.png"
else
    echo "  welcome  FAILED — see $WORK/welcome.log" >&2
    cat "$WORK/welcome.log" >&2
fi

# The handbook keeps its own copies: mdBook only serves what is under its src/.
for n in designer editor welcome; do
    [ -f "docs-site/src/assets/screenshot-$n.png" ] && \
        cp "assets/screenshot-$n.png" "docs-site/src/assets/screenshot-$n.png"
done
echo "==> done"
