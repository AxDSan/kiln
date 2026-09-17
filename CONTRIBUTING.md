# Contributing to Kiln

Thanks for looking. This page is what you need to build Kiln, change it, and get
the change accepted. [`CLAUDE.md`](CLAUDE.md) carries the same conventions in
more detail.

## Layout

```
k2/         Kiln 2: lexer, parser, lowering                (Rust)
kir/        Kiln 2's intermediate form and its LLVM emitter (Rust)
ir/         Kiln 1.x: parser, type checker, validator      (Rust)
backend/    Kiln 1.x lowering to LLVM IR                   (Rust)
cli/        the `kiln` binary, language server, debug adapter (Rust)
debug/      the debugger engine                            (Rust)
runtime/    the core runtime and its commands              (C)
libs/       support libraries                              (C / C++)
abi/        the C ABI shared by all of the above
designer/   Kiln Studio, the IDE                           (C++ / RmlUi)
kits/       a project-tier kit, the worked example of one
editors/    the VS Code extension
docs-site/  the documentation site (mdBook + landing page)
tools/      fetch, package and check scripts
```

## Building

You need a Rust toolchain, `clang`, and — for GUI programs and Studio —
`pkg-config`, SDL2, SDL2_image and FreeType.

```sh
tools/fetch-rmlui.sh && tools/fetch-accesskit.sh   # the vendored UI stack
cargo build --release                             # the compiler
designer/build.sh                                 # Studio
```

Optional: `tools/fetch-mbedtls.sh` for `https`, the SQLite and MariaDB client
development packages for `db`, `mingw-w64` and `wine` for the Windows
cross-builds and their tests.

## Before you open a pull request

```sh
cargo test --release              # the whole suite
designer/build.sh test            # Studio's own tests, if you touched designer/
tools/check-docs.sh               # every documented sample compiles
tools/check-release.sh            # the README and generated pages match the toolchain
```

CI runs the first, third and fourth on every push and pull request. A test that
needs something a machine does not have — the vendored UI stack, Wine, a MariaDB
server — skips itself and says so; it does not fail.

## Conventions that matter

- **The CLI is the only reader of a project file.** Studio and the documentation
  ask `kiln inspect`, `kiln commands` and `kiln templates` rather than parsing
  `.kiln` themselves.
- **Reference documentation is generated.** `tools/gen-docs.sh` writes the
  reference pages and `cli.md`; never edit them by hand. A solved limitation is
  removed from `docs-site/src/limitations.md`, not left there.
- **Verify UI work by rendering it.** Studio bugs pass tests and fail on screen:
  `KILN_DESIGNER_DUMP=x.ppm KILN_DESIGNER_SCRIPT='view:code' …` writes a frame you
  can look at. Headless runs use `SDL_VIDEODRIVER=offscreen`.
- **Never open a tracked example in Studio.** It saves on exit, and the change
  lands in your next commit. Open a copy.
- **Clean up after tests.** Large scratch — Wine prefixes, database directories,
  build trees — goes under `target/`, not `/tmp`, and a test reuses one fixed
  directory rather than making a new one per run.
- **A support library is plain C**: `libs/<name>/<name>_libinfo.c` for the
  metadata and `<name>_cmds.c` for the implementations. `use <name>` finds it;
  there is no registration list. A command's `doc` and `example` fields become
  its reference entry, its hover text and its F1 page, and the example is
  compiled by `check-docs`.

## Commits

One change per commit. The subject says what now happens, from the point of
view of someone using Kiln, prefixed by the area it touches:

```
k2: a T? stored where a T is wanted is its value or a stop
studio: File has Open Project and Open File
```

No attribution trailers.

## Versioning

A MINOR release adds surface and keeps every program that compiled; a PATCH
release fixes without adding; a MAJOR release is allowed to invalidate source.
[`RELEASING.md`](RELEASING.md) is the release checklist.

## Licence

By contributing you agree that your contribution is licensed as Kiln is:
MIT OR BSD-3-Clause, at the user's option.
