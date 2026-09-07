#!/bin/bash
# Build the Kiln designer. No CMake: this mirrors libs/ui/lib.json's flags.
#   designer/build.sh          -> build designer/kiln-designer
#   designer/build.sh test     -> run the model tests, then the Studio tests (needs a display)
#   designer/build.sh --os windows -> designer/windows/kiln-studio.exe, via build-windows.sh
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ "${1:-}" = "--os" ]; then
  case "${2:-}" in
    windows) exec designer/build-windows.sh ;;
    linux) shift 2 ;;
    *) echo "unknown --os ${2:-}: linux or windows" >&2; exit 1 ;;
  esac
fi

if [ ! -f vendor/RmlUi/build/librmlui.a ]; then
  echo "RmlUi is not vendored — run tools/fetch-rmlui.sh" >&2
  exit 1
fi

FLAGS=(-std=c++17 -O1 -Wall -Wextra -Wformat=2 -DRMLUI_SDL_VERSION_MAJOR=2 -DSDL_VIDEO_RENDER_OGL=1
       -I abi -I libs/ui -I designer -I vendor/RmlUi/Include -I vendor/RmlUi/Backends)
read -r -a PKG_CFLAGS <<< "$(pkg-config --cflags sdl2 SDL2_image freetype2)"
read -r -a PKG_LIBS <<< "$(pkg-config --libs sdl2 SDL2_image freetype2)"
LIBS=(-Lvendor/RmlUi/build -lrmlui -lGL -ldl "${PKG_LIBS[@]}")

if [ "${1:-}" = "test" ]; then
  # The tests open and save scratch files, and Studio remembers what it
  # opened; kept out of the user's real recent-projects list.
  export XDG_CACHE_HOME=/tmp/kiln_test_cache XDG_DATA_HOME=/tmp/kiln_test_data
  clang++ -std=c++17 -O1 -I designer designer/model.cpp designer/test_model.cpp -o /tmp/kiln_designer_test
  /tmp/kiln_designer_test ./target/debug/kiln
  # The platform layer (portable.h): the process and path shims Studio's
  # build, run and language-server code sit on. The same probe runs under
  # wine for the Windows half (cli/tests/studio_windows.rs).
  clang++ -std=c++17 -O1 -I designer designer/test_portable.cpp -o /tmp/kiln_portable_probe
  /tmp/kiln_portable_probe
  # The Studio tests drive the built designer through its scripted verbs, so
  # they need the binary and a display; a headless checkout still gets the
  # model tests above rather than a failure it cannot act on.
  if [ -x designer/kiln-designer ] && [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
    clang++ -std=c++17 -O1 -I abi -I designer designer/test_studio.cpp libs/ui/ui_libinfo.c \
      -o /tmp/kiln_studio_test
    exec /tmp/kiln_studio_test ./target/debug/kiln designer/kiln-designer
  fi
  echo "studio tests skipped: need designer/kiln-designer and a display" >&2
  exit 0
fi

clang++ "${FLAGS[@]}" "${PKG_CFLAGS[@]}" \
  designer/main.cpp designer/model.cpp libs/ui/ui_libinfo.c \
  vendor/RmlUi/Backends/RmlUi_Backend_SDL_GL3.cpp \
  vendor/RmlUi/Backends/RmlUi_Platform_SDL.cpp \
  vendor/RmlUi/Backends/RmlUi_Renderer_GL3.cpp \
  "${LIBS[@]}" -o designer/kiln-designer
echo "built designer/kiln-designer"
