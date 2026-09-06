# OpenEPL __VERSION__

An open implementation of **Easy Programming Language** (易语言, EPL): a
cross-platform RAD environment where you draw a form, wire an event, and get a
clean native binary. English-first, and a fresh implementation rather than a
clone — it does not run existing EPL programs.

    bin/openepl-studio          the IDE — run it with no arguments
    bin/openepl                 the command-line toolchain

Nothing here needs installing. The bundle is relocatable: move it anywhere and
run the binaries from `bin/`. They locate the runtime and templates relative to
themselves, not through environment variables.

## Requirements

| For | You need |
| --- | --- |
| Any build | `clang`, `ar` |
| GUI programs and the IDE | `pkg-config`, SDL2, SDL2_image, FreeType, OpenGL |

The runtime and support libraries ship as **source** and are compiled into each
program you build. That is what lets the linker drop every command your program
does not call, so a hello-world stays small and contains no unpacking stub.

Fedora: `sudo dnf install clang binutils pkgconf-pkg-config SDL2-devel SDL2_image-devel freetype-devel`
Debian/Ubuntu: `sudo apt install clang binutils pkg-config libsdl2-dev libsdl2-image-dev libfreetype-dev`

## Start here

    bin/openepl-studio

Studio opens a welcome screen: pick a project kind, and it is created and opened
for you. Drag components onto the form, set properties in the inspector, wire a
button's click to a subroutine, and press **Run**.

From the command line:

    bin/openepl templates                    # what you can create
    bin/openepl new gui-app my-app           # create a project
    bin/openepl run my-app/main.oir          # build and run it

## Build targets

One source builds as any of these — a choice, not a rewrite:

| `target` | Produces |
| --- | --- |
| `console` | a terminal program |
| `gui` | a windowed program |
| `sharedlib` | `.so` (`.dll` / `.dylib` on other platforms) |
| `staticlib` | `.a` archive |

Declare it in the module (`target sharedlib`) or override it per build
(`--target sharedlib`). Omitted, a module with a form is a GUI program and
anything else is a console one.

## Editing

`bin/openepl lsp` is a language server: diagnostics as you type, completion,
signature help, hover, go-to-definition and find-references. Point any
LSP-capable editor at it — `docs/editors.md` has ready-made configuration for
Neovim, VS Code, Helix and Zed, and `editors/vscode/` is a working extension
with syntax highlighting.

## On Windows

The Windows bundle (`openepl-__VERSION__-windows-x86_64.zip`) is cross-built
from Linux and has the same layout: `bin\openepl-studio.exe` with the DLLs it
needs beside it, `bin\openepl.exe` when the compiler cross-built (this file
ends with a line saying so when it did not), and the same `templates\`,
`runtime\`, `libs\` and `docs\`. Unzip it anywhere and run
`bin\openepl-studio.exe`.

Building a program there needs a toolchain on the machine:

| For | You need on the Windows machine |
| --- | --- |
| Any build | LLVM's `clang` on `PATH` — https://releases.llvm.org/ |
| GUI programs | SDL2, SDL2_image and freetype, built for the mingw ABI (MSYS2: `mingw-w64-x86_64-SDL2`, `-SDL2_image`, `-freetype`) |

Those MSYS2 package names begin with `mingw-w64-` because they are the mingw
ABI *builds of those libraries*, which is what a program built here links
against. The mingw-w64 **cross-compiler** is a different thing, and it is
needed on the *Linux* machine running `openepl build --os windows` — not on
the Windows one.

**The Windows bundle carries `SETUP-WINDOWS.md` at its root, and it walks
through installing them** — including the "Add LLVM to the system PATH" box,
which is not ticked by default and is what most first runs are missing.

Without `clang`, Studio opens and shows the templates, but the toolbox is
empty — it is filled from `openepl commands`, which compiles each library's
metadata with clang — and Build says so, naming what to install. This is a first Windows build:
it has been run under wine, not on Windows, and the IDE's window has not yet
been seen drawn there. Accessibility is off on Windows, and `https://` is
off in a Windows build.

## What this is not, yet

* **x86-64 only, and Windows is new.** The Linux bundle is what has been
  used; the Windows one is cross-built and run under wine only (see above).
  macOS and arm64 are not supported yet.
* **A release build is hardened, not hidden.** `openepl build --release`
  optimises, hardens and strips what you build. A native binary can still be
  disassembled — the flag buys a smaller, harder-to-attack program, not secrecy.
* **The debugger is Linux-only.** Set a breakpoint in Studio's gutter, press
  Debug, and the program stops on the line with its call stack and variables
  beside it. `openepl dap` is the same debugger for any editor that speaks the
  Debug Adapter Protocol, and the bundled VS Code extension is wired to it.
  The engine is `ptrace`, so a Windows build cannot be debugged yet.
* **No TLS in this bundle.** It ships no TLS stack, so `net_http_get` on an
  `https://` URL fails with a message rather than downgrading to plaintext,
  and the `httpserver` component is plaintext regardless. Building the
  toolchain from source with `tools/fetch-mbedtls.sh` run is what adds
  https.
* **Memory is reclaimed at exit**, not before. A program that runs for days
  grows with the work it has done.

The full documentation — a tour of the language, the component model, the
IDE, and the generated reference for every command and component — is at
https://axdsan.github.io/openepl/. The project's design notes, specifications
and decision log live in the source repository, not in this download.

Licensed MIT OR BSD-3-Clause. See `LICENSE` and `THIRD-PARTY.md`.
