#!/bin/bash
# Cross-build Kiln Studio for Windows x86-64 with mingw-w64.
#   designer/build-windows.sh          -> designer/kiln-studio.exe, and the
#                                         DLLs it imports beside it (designer/windows/)
#
# The same recipe as designer/build.sh — RmlUi statically, SDL2, SDL2_image
# and freetype from the sysroot's packages — with the Windows toolchain the
# ui library uses for `kiln build --os windows`: mingw's own g++ for C++,
# because it built the Windows RmlUi archive and clang's C++ objects will not
# link against it (libs/ui/lib.json, cli/src/main.rs say the same).
#
# What it needs (Fedora names): mingw64-gcc-c++ mingw64-sdl2-compat
# mingw64-SDL2_image mingw64-freetype, and tools/build-rmlui-windows.sh run.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

TRIPLE=x86_64-w64-mingw32
SYSROOT="${SYSROOT:-/usr/$TRIPLE/sys-root/mingw}"
OUT_DIR="${OUT_DIR:-designer/windows}"
EXE="$OUT_DIR/kiln-studio.exe"

command -v "$TRIPLE-g++" >/dev/null || { echo "missing $TRIPLE-g++ (mingw64-gcc-c++ on Fedora)" >&2; exit 1; }
command -v "$TRIPLE-pkg-config" >/dev/null || { echo "missing $TRIPLE-pkg-config (mingw64-filesystem on Fedora)" >&2; exit 1; }
if [ ! -f vendor/RmlUi/build-windows/librmlui.a ]; then
  echo "RmlUi is not built for Windows — run tools/build-rmlui-windows.sh (after tools/fetch-rmlui.sh)" >&2
  exit 1
fi
"$TRIPLE-pkg-config" --exists sdl2 SDL2_image freetype2 || {
  echo "the mingw SDL2, SDL2_image and freetype packages are missing (mingw64-sdl2-compat mingw64-SDL2_image mingw64-freetype on Fedora)" >&2
  exit 1
}

# The ui library's Windows flags (libs/ui/lib.json): RMLUI_STATIC_LIB so the
# headers do not ask for dllimport, and the same backend defines as build.sh.
# `_Static_assert` is C11; g++ has only `static_assert`, and abi/kiln_abi.h
# uses the C spelling — the same shim libs/ui/ui_rmlui.cpp carries for the
# ui library's own cross build.
FLAGS=(-std=gnu++17 -O1 -Wall -Wextra -Wformat=2 -DRMLUI_SDL_VERSION_MAJOR=2 -DSDL_VIDEO_RENDER_OGL=1
       -DRMLUI_STATIC_LIB -D_Static_assert=static_assert -I abi -I libs/ui -I designer -I vendor/RmlUi/Include -I vendor/RmlUi/Backends)
read -r -a PKG_CFLAGS <<< "$("$TRIPLE-pkg-config" --cflags sdl2 SDL2_image freetype2)"
read -r -a PKG_LIBS <<< "$("$TRIPLE-pkg-config" --libs sdl2 SDL2_image freetype2)"
# pkg-config's line already carries -lmingw32 -lSDL2main -mwindows: Studio's
# `main` sees SDL.h, which renames it SDL_main on Windows, so SDL2main's
# WinMain is the entry and -mwindows keeps a console from opening behind the
# IDE. The C++ runtime is linked in — two DLLs fewer to ship.
# comdlg32 is GetOpenFileName, which is how Studio asks the platform for a
# file rather than listing one directory itself (sys::pick_open_file).
LIBS=(-Lvendor/RmlUi/build-windows -lrmlui -lopengl32 -lgdi32 -lcomdlg32 "${PKG_LIBS[@]}"
      -static-libgcc -static-libstdc++)

mkdir -p "$OUT_DIR"
OBJ="$(mktemp -d)"
trap 'rm -rf "$OBJ"' EXIT
"$TRIPLE-gcc" -O1 -I abi -I libs/ui -c libs/ui/ui_libinfo.c -o "$OBJ/ui_libinfo.o"
"$TRIPLE-g++" "${FLAGS[@]}" "${PKG_CFLAGS[@]}" \
  designer/main.cpp designer/model.cpp "$OBJ/ui_libinfo.o" \
  vendor/RmlUi/Backends/RmlUi_Backend_SDL_GL3.cpp \
  vendor/RmlUi/Backends/RmlUi_Platform_SDL.cpp \
  vendor/RmlUi/Backends/RmlUi_Renderer_GL3.cpp \
  "${LIBS[@]}" -o "$EXE"

# The DLLs it imports, transitively, from the sysroot — read from the
# images' import tables, as `kiln build --os windows` does, never from a
# list kept by hand. sdl2-compat's SDL2.dll loads SDL3.dll by hand rather
# than importing it, so that one is named here (libs/ui/lib.json,
# windows_extra_dlls).
declare -A have
for f in "$SYSROOT"/bin/*.dll; do have["$(basename "$f" | tr 'A-Z' 'a-z')"]="$f"; done
declare -A copied
queue=("$EXE")
wanted=("SDL3.dll")
while [ "${#queue[@]}" -gt 0 ]; do
  img="${queue[0]}"; queue=("${queue[@]:1}")
  while read -r name; do wanted+=("$name"); done < <("$TRIPLE-objdump" -p "$img" | sed -n 's/^\s*DLL Name: \(.*\)$/\1/p')
  while [ "${#wanted[@]}" -gt 0 ]; do
    name="${wanted[0]}"; wanted=("${wanted[@]:1}")
    key="$(echo "$name" | tr 'A-Z' 'a-z')"
    [ -n "${copied[$key]:-}" ] && continue
    src="${have[$key]:-}"
    [ -z "$src" ] && continue          # a system DLL: Windows has it
    cp "$src" "$OUT_DIR/$(basename "$src")"
    copied[$key]=1
    queue+=("$OUT_DIR/$(basename "$src")")
  done
done
echo "built $EXE"
echo "beside it: $(ls "$OUT_DIR" | grep -i '\.dll$' | tr '\n' ' ')"
