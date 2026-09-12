# Build targets

The same source builds as any of these. It is a build option, not a rewrite:
the language, the type checking and the component model are identical across
all of them. Only the entry contract changes.

| `target` | Produces | Entry |
| --- | --- | --- |
| `console` | a terminal program | `main` |
| `gui` | a windowed program | the form, then `main`, then the event loop |
| `sharedlib` | `.so` (`.dll` + `.lib` on Windows), and a C header | none — subroutines are exported |
| `staticlib` | `.a` archive, and a C header | none — subroutines are exported |

Declare it in the module:

```
module greet
target sharedlib

sub greet
  call print_text("Hello from a shared library.")
end
```

…or override it for one build, without touching the source:

```sh
kiln build greet.kiln --target sharedlib -o libgreet.so
```

Left out entirely, the target is inferred: a module with a form is `gui`,
anything else is `console`.

## Programs

Console and windowed programs are ordinary executables. The linker drops every
command the program never calls, so a small program stays small, and there is
nothing to unpack at start-up.

## Release builds

Every build is a debug build unless you ask otherwise. `--release` is the other
profile, on `build` and on `run`:

```sh
kiln build hello.kiln --release -o hello
```

It compiles at `-O2` and adds the hardening a shipped program wants: `_FORTIFY_SOURCE`,
`-fstack-protector-strong`, position-independent code, read-only relocations
with every symbol bound at load time, and no symbol table — the names of your
subroutines, module variables and the runtime commands you linked are gone from
the file. Dead-stripping still applies, so the program is smaller than the debug
one as well as faster.

Each flag is offered to the local `clang` before it is used, and one the
compiler will not take is dropped with a line saying so. A flag accepted in
silence and ignored would leave you believing in hardening that is not there.

The default stays a debug build because most builds are the one you are about to
run and delete, and hardening costs compile time. Nothing else changes: the same
source, the same output, the same behaviour.

A windowed program is the one exception to position independence — the vendored
UI stack is not built `-fPIC` — and the build tells you when it links without it.

## Libraries

A library has no entry point. Every subroutine is exported under its own name,
so a C host — or anything that can call C — links against it directly, and
the build writes the header that declares them beside the artifact.

```
module greet
target sharedlib

var greetings: int = 0

sub greet
  call print_text("Hello from a shared library.")
end

sub add(a: int, b: int): int
  return a + b
end

sub greeting(name: text): text
  greetings += 1
  return "Hello, {name}!"
end
```

```sh
kiln build greet.kiln -o libgreet.so     # writes libgreet.so and greet.h
```

### The header

`greet.h` is generated from the same code the library was lowered from, so
what it declares is what was linked. It is named after the module, not the
output — `-o libgreet.so` still gives `greet.h` — and `--header <path>` puts
it elsewhere. It compiles as C and as C++, and it looks like the header a
Windows DLL author would have written by hand:

```c
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(GREET_STATIC)
#  define GREET_API
#elif defined(_WIN32)
#  ifdef GREET_EXPORTS /* defined when building the DLL itself */
#    define GREET_API __declspec(dllexport)
#  else
#    define GREET_API __declspec(dllimport)
#  endif
#else
#  define GREET_API __attribute__((visibility("default")))
#endif

GREET_API void greet_init(void); /* initialises module variables; call once, first */
GREET_API void greet(void);
GREET_API int32_t add(int32_t a, int32_t b);
GREET_API const char *greeting(const char *name);

#ifdef __cplusplus
}
#endif
```

The macro prefix is the module name in capitals: `GREET_API` marks each
export, `GREET_EXPORTS` is what the DLL's own build would define (nothing
you write needs it — the library's objects come from the compiler, not from
this header — so a consumer always sees `dllimport`), and `GREET_STATIC`
switches the imports off. A static library's header defines `GREET_STATIC`
itself, because its symbols are linked in, not imported, and a `dllimport`
on them would send a Windows link looking for an import library that does
not exist.

The types are the ones the exported wrappers actually take:

| Kiln | C |
| --- | --- |
| `int` | `int32_t` |
| `int64` | `int64_t` |
| `double` | `double` |
| `bool` | `int32_t` — 0 or 1; never a C `bool`, which is one byte |
| `text` | `const char *` — NUL-terminated, `NULL` means empty |

Text a subroutine returns belongs to the library: copy it if you keep it,
and never free it. A subroutine that takes or returns a byte-set, an array,
a record or a dictionary is a pointer to a runtime-owned object C cannot
build or read, so it gets no prototype; the header lists it in a comment
instead, so the omission is a fact you can see rather than a name that is
simply missing.

`<module>_init` initialises the module's variables. It is exported rather
than run automatically, because a library should not run your code before
the host is ready for it.

### A loader hook

A shared library can also run *without* a host asking — the moment the OS maps
it into a process — by defining a specially-named subroutine:

```
sub dll_attach   # runs when the library is mapped into a process
sub dll_detach   # runs when it is unmapped
```

Each takes no parameters and returns nothing. Define one in a `sharedlib` and
the build wires it to the platform's loader entry: a real `DllMain`
(`DLL_PROCESS_ATTACH` / `DLL_PROCESS_DETACH`) on Windows, an ELF
constructor/destructor on Linux. `<module>_init` runs first, before
`dll_attach`, so module variables are ready. A library that defines neither is
unchanged — it gets no loader entry, and the host calls `<module>_init` itself.

This is what a library loaded for effect needs — one injected into a process, or
brought in with `LoadLibrary`/`dlopen` for what it does rather than what it
exports. The hook runs under the OS loader (on Windows, holding the loader
lock), so it should keep its own work short and hand anything heavy to a thread
it spawns. [Interop](./interop.md) walks through a worked example: a library
that installs a function-pointer hook the instant it loads.

### A consumer

`kiln new shared-library` writes a `consumer.cpp` beside the source; this
is it, for a module named `greet`:

```cpp
#include <stdio.h>
#include "greet.h"

#if defined(_WIN32) && defined(_MSC_VER)
#  pragma comment(lib, "greet.lib")
#endif

int main(void) {
    greet_init();                          /* module variables, once, first */
    greet();
    printf("%d\n", (int)add(2, 3));
    printf("%s\n", greeting("world"));      /* the text belongs to the library */
    return 0;
}
```

On Linux, with clang:

```sh
kiln build greet.kiln -o libgreet.so
clang++ consumer.cpp -I. -L. -lgreet -Wl,-rpath,. -o consumer && ./consumer
```

The same file compiles as C — `clang -x c consumer.cpp ...` — because the
header only wraps its prototypes in `extern "C"` when a C++ compiler reads
it.

A static library is the same, built as an archive and linked in:

```sh
kiln build greet.kiln --target staticlib -o libgreet.a    # and greet.h
clang++ consumer.cpp -I. libgreet.a -lm -o consumer
```

## Building for Windows

A program — windowed or console — or a library cross-builds for Windows
x86-64 from Linux:

```sh
kiln build hello.kiln --os windows          # hello.exe
kiln build form.kiln --os windows           # form.exe, and the DLLs it needs beside it
kiln build greet.kiln --os windows --target sharedlib   # greet.dll
kiln build greet.kiln --os windows --target staticlib   # libgreet.a
```

`--arch x86` builds the same console programs and libraries as a PE32 i386
image, which is what a 2000s Win32 client is. It needs the 32-bit mingw
toolchain (`mingw32-gcc` on Fedora, `gcc-mingw-w64-i686` on Debian):

```sh
kiln build hello.kiln --os windows --arch x86              # hello.exe, i386
kiln build hook.kiln  --os windows --arch x86 --target sharedlib   # hook.dll
```

The language is unchanged. A c-record's pointer-sized fields are four bytes,
and a `system` declaration is `stdcall` — which is what a Win32 call is on
32-bit, and what keeps the stack balanced. `--target gui` is refused on x86
(the UI stack is x86-64 only).

A shared library built for Windows comes as three files: `greet.dll`,
`greet.h`, and the import library `greet.lib` that a Windows link goes
through — the one the consumer's `#pragma comment(lib, "greet.lib")` names.
The consumer above builds against them with either Windows toolchain:

```sh
kiln build greet.kiln --os windows -o greet.dll     # greet.dll, greet.lib, greet.h
cl /EHsc consumer.cpp greet.lib                        # MSVC, x64
x86_64-w64-mingw32-g++ consumer.cpp -L. -lgreet -o consumer.exe   # MinGW
```

The import library is written by mingw's linker from the DLL's export table.
It is an ordinary COFF import archive, the format `link.exe` accepts for
plain C names — the exports carry no decoration to disagree about. It is
produced and checked on Linux, not under MSVC; should a Windows link reject
it, `x86_64-w64-mingw32-dlltool -l greet.lib -d greet.def` writes the
short-import form from the same export list. A static
library cross-built for Windows is a mingw archive, and links with mingw —
`x86_64-w64-mingw32-g++ consumer.cpp libgreet.a -lws2_32` — rather than with
MSVC, whose linker wants an archive its own toolchain produced.

`--os linux` is the default and means the machine you are on. `--os windows`
needs the mingw-w64 cross compiler on the build machine — `mingw64-gcc` on
Fedora, `gcc-mingw-w64-x86-64` on Debian and Ubuntu — and the build says so in
one line when it is missing. The IR still goes through `clang`, retargeted;
mingw's `gcc` does the link.

What comes out is the same program: the same source, the same commands, the
same dead-stripping. `--release` applies too, with the hardening PE has in
place of what ELF has — ASLR (`--dynamicbase`, `--high-entropy-va`) and DEP
(`--nxcompat`) stand in for PIE and RELRO, and the symbol table is stripped
the same way. The IR and the C go through `clang` retargeted; C++ — the ui
library and RmlUi's backend — goes through mingw's own `g++`, the compiler
the vendored RmlUi archive was built with, because the two disagree about
the layout of C++ type information and mingw's linker will not merge them.

### A windowed program

A module with a form builds to a `.exe` for the Windows GUI subsystem — no
console window opens behind it — and its `print_text` output goes nowhere,
as it does for any Windows GUI program. The UI stack is linked in: RmlUi
statically, and SDL2, SDL2_image and freetype as the DLLs the distribution's
mingw packages provide. Those DLLs and everything they in turn import are
copied beside the program, and the build lists them:

```
kiln: copied beside it, because the program imports them: libwinpthread-1.dll
  libfreetype-6.dll SDL2_image.dll SDL2.dll SDL3.dll libwebpdemux-2.dll libwebp-7.dll
  libtiff-5.dll zlib1.dll libjpeg-62.dll libgcc_s_seh-1.dll libsharpyuv-0.dll
  libpng16-16.dll libbz2-1.dll
kiln: wrote form.exe
```

Ship the directory, not the file: the program loads those by name from
beside itself, and a Windows machine has none of them. The exact list is
whatever the mingw packages on the build machine import — read from the
images, not from a list kept by hand — so it is right for the sysroot it was
built from. On Fedora, SDL2 is `sdl2-compat` over SDL3, which is why
`SDL3.dll` is there; it is loaded by hand rather than imported, and the ui
library's manifest names it so it ships. The C++ runtime is linked in, so
`libstdc++-6.dll` is not on the list.

What it needs on the build machine, beyond the cross compiler:

```sh
sudo dnf install mingw64-gcc-c++ mingw64-sdl2-compat mingw64-SDL2_image mingw64-freetype
tools/fetch-rmlui.sh              # the vendored RmlUi, if not already there
tools/build-rmlui-windows.sh      # the same checkout, built a second time with mingw-w64
```

`tools/build-rmlui-windows.sh` is the Windows counterpart of
`tools/fetch-rmlui.sh`: it builds the pinned checkout into
`vendor/RmlUi/build-windows` with a CMake toolchain file it writes itself,
against the sysroot's SDL2 and freetype (`SYSROOT=` points it elsewhere).
Debian and Ubuntu ship the compiler but not those cross packages; a sysroot
with them is needed there. A build without the Windows RmlUi says so in one
line and names the script.

Two things are different on Windows, and both are said here rather than
discovered:

- **Accessibility is off.** The a11y bridge is AccessKit's Unix adapter
  (AT-SPI over D-Bus), and AccessKit has no Windows build vendored here yet.
  Under `_WIN32` the bridge compiles to stubs: the program runs, the
  accessibility tree the component model carries is built as always, and
  nothing on Windows can read it. UI Automation through AccessKit's Windows
  adapter is the piece that is missing.
- **Headless rendering is a Linux thing.** On Linux,
  `KILN_UI_EXIT_AFTER_FRAMES` and `KILN_UI_DUMP` default to SDL's
  offscreen driver, which draws through EGL; the Windows build of SDL has no
  EGL, so a Windows program does not switch drivers. The frame count and the
  dump are the same code on both platforms and should work through an
  ordinary window there — but that has not been observed: no Windows run
  made here has got past opening a window.

To run the result under wine, put it beside its DLLs and run it there:

```sh
cd build/ && wine form.exe
```

With a display, wine's driver would open the window on your desktop and
draw through its OpenGL; that has not been tried here. What has: with no
display — `WINEDLLOVERRIDES="winex11.drv,winewayland.drv=d"` turns wine's
drivers off on purpose, which is how the test suite runs so that it never
puts a window on a developer's screen — the program loads with every DLL
resolved, runs the runtime's entry, and stops where SDL asks for a window,
with `SDL error on create window` on stderr and exit status 1. That is
exactly what the suite checks, and no further: a console program that says
`use ui` runs to completion under wine the same way, which is what proves
the DLL list complete. The drawn window itself has not been seen under wine
or on Windows.

The limits, stated plainly:

- **Nothing is built natively on Windows yet.** This is a cross build from
  Linux: the toolchain and Studio are both cross-built for Windows by
  `tools/package-windows.sh`, and neither has been run on a Windows machine.
- **A windowed program for Windows has been run under wine, not on Windows.**
  See above for what that proves.
- **`https://` is off in a Windows build.** The vendored mbedTLS was built
  for Linux, so a cross build leaves it out and `net_http_get` says so at run
  time; `http://` works.
- `kiln run --os windows` refuses: the machine you are on cannot run the
  result. Run it under `wine`, or on Windows.

## Naming

Without `-o`, an output is named after the source: a program takes the file's
stem, and libraries follow the platform convention (`libgreet.so`,
`libgreet.a`) so a linker finds them by the name it expects. Built for
Windows, a program is `hello.exe`, a shared library `greet.dll`, and a static
library keeps mingw's `libgreet.a`. A Windows program given `-o hello` gets
its `.exe` added — Windows will not run a file without one. A library's
header is `<module>.h` beside it whatever the artifact was called, and a
Windows DLL's import library takes the DLL's name with `.lib` in place of
`.dll`.

## What a library may not do

A library cannot declare a form — a window belongs to a program. Building one
that does is an error that says so, as is a library that exports nothing.
