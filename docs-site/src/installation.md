# Installation

## Download a release

Releases are self-contained: unpack one anywhere and run it. There is no
installer, nothing to add to `PATH`, and no configuration.

```sh
tar xzf kiln-<version>-linux-x86_64.tar.gz
cd kiln-<version>-linux-x86_64
bin/kiln-studio
```

The binaries find everything they need relative to themselves, so you can move
the folder wherever you like.

## What you need installed

| For | Requirement |
| --- | --- |
| Building any program | `clang`, `ar` |
| Windowed programs and the IDE | `pkg-config`, SDL2, SDL2_image, FreeType, OpenGL |
| Building for Windows (`--os windows`) | the mingw-w64 cross compiler — `mingw64-gcc` on Fedora, `gcc-mingw-w64-x86-64` on Debian and Ubuntu |
| Building for 32-bit Windows (`--arch x86`) | its 32-bit twin — `mingw32-gcc` on Fedora, `gcc-mingw-w64-i686` on Debian and Ubuntu |

The runtime ships as source and is compiled into each program you build. That
is what lets the linker drop every command your program never calls.

**Fedora**

```sh
sudo dnf install clang binutils pkgconf-pkg-config \
                 SDL2-devel SDL2_image-devel freetype-devel
```

**Debian / Ubuntu**

```sh
sudo apt install clang binutils pkg-config \
                 libsdl2-dev libsdl2-image-dev libfreetype-dev
```

## Building from source

You will also need a Rust toolchain.

```sh
git clone https://github.com/AxDSan/kiln
cd kiln

tools/fetch-rmlui.sh          # vendor the UI library
tools/fetch-accesskit.sh      # vendor the accessibility bridge

cargo build --release         # the compiler
designer/build.sh             # the IDE
cargo test                    # the test suite
```

The compiler is then `target/release/kiln` and the IDE is
`designer/kiln-designer`.

`https://` is opt-in: it needs mbedTLS vendored, and everything else builds
without it.

```sh
tools/fetch-mbedtls.sh        # once; then net_http_get speaks https
```

See [Networking](./networking.md) for what that buys and what it never does.

To produce a release bundle of your own:

```sh
tools/package.sh                                # -> dist/
tools/verify-bundle.sh dist/kiln-*.tar.gz    # prove it works unpacked elsewhere
```

## Platform support

Linux on x86-64, and Windows x86-64 as a cross build from it: `kiln build
--os windows` produces a `.exe`, a DLL or a static library, and needs the mingw-w64 cross
compiler above. Studio cross-builds for Windows too, and nothing is built
natively *on* Windows; macOS and arm64 are not supported yet. See [Build
targets](./build-targets.md) and [Limitations](./limitations.md).
