# Setting up Kiln on Windows

Kiln compiles to a native binary. It does not interpret your program and it
does not ship its own code generator, so it needs a C toolchain on the machine
to produce the executable — the same way a C or Rust install does.

This bundle contains everything else: the compiler (`bin\kiln.exe`), Studio
(`bin\kiln-studio.exe`), the runtime, the support libraries and the
examples. What it cannot contain is a linker.

If you see this in Studio's PROBLEMS pane or in the console:

```
kiln: clang is not on PATH — install LLVM (which provides clang) and put
its bin directory on PATH
```

then this page is the fix.

## What to install

**LLVM**, which provides `clang`. Download the Windows installer from
<https://releases.llvm.org/> or the
[LLVM releases on GitHub](https://github.com/llvm/llvm-project/releases) —
pick `LLVM-<version>-win64.exe`.

**During installation, choose "Add LLVM to the system PATH".** This is the
step that matters, and it is not the default. If you miss it, the install
succeeds and Kiln still reports clang missing.

That is enough to build console programs, windowed programs and libraries.

## Checking it worked

Open a **new** Command Prompt — an already-open one keeps the old PATH — and
run:

```
clang --version
```

A version banner means you are done. `'clang' is not recognized` means the
PATH entry did not take: see "If clang is still not found" below.

Then build something from the bundle:

```
bin\kiln.exe run examples\hello.kiln
```

## If clang is still not found

Add it by hand. LLVM installs to `C:\Program Files\LLVM\bin` by default.

1. Press Start, type `environment variables`, and open **Edit the system
   environment variables**.
2. Click **Environment Variables…**.
3. Under **User variables**, select `Path` and click **Edit…**.
4. Click **New** and add `C:\Program Files\LLVM\bin`.
5. Click OK on all three dialogs, then open a **new** Command Prompt.

If you installed LLVM somewhere else, use that folder's `bin` directory
instead.

## Building for Windows from Linux

The other direction needs **mingw-w64** rather than LLVM, because the Windows
link is done by the mingw driver:

```sh
sudo dnf install mingw64-gcc mingw64-gcc-c++      # Fedora
sudo apt install gcc-mingw-w64 g++-mingw-w64      # Debian / Ubuntu
```

Then:

```sh
kiln build myprogram.kiln --os windows
```

A missing mingw reports itself the same way clang does, naming what to
install.

## What Studio can and cannot do here

Studio runs on Windows: it draws its canvas, edits code, and reads and writes
projects. It builds and runs them once clang is on PATH.

Studio's own build is not produced *on* Windows — the toolchain that makes it
runs on Linux and cross-builds — so it is the shipped `.exe` that is tested,
not a Windows-native build of it. See [Limitations](https://axdsan.github.io/kiln/docs/limitations.html)
for the current state of the Windows port, including what has and has not been
looked at on a real display.
