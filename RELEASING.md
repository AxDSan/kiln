# Releasing Kiln

A release touches more surfaces than the compiler. This page is the list of them,
in the order they have to be updated, and what verifies each one. It exists
because a release went out with four of them stale and nobody noticed: the README
counted 13 kits when there were 16, the databases page still described the one
thing that release had just fixed, a solved limitation was still listed as one,
and a port record still said "open, deliberately" about finished work.

The last section is a script rather than advice, for that reason.

## 1. What the version number means

It is a promise about *your* programs, not about the toolchain's internals —
the same promise the README's Status section makes:

| | |
|---|---|
| **MINOR** | new commands, new kits, new targets. A program that compiled still compiles and still behaves. |
| **PATCH** | fixes, no new surface. |
| **MAJOR** | something that invalidates source that compiles today: a language change, or an ABI change. |

**Adding values at the end of a frozen enum is not an ABI change.** Handle kinds
are assigned in `abi/kiln_abi.h`, and appending to that enum moves no structure
and changes no existing value, so `KILN_ABI_VERSION` stays where it is and a
library built against the old number keeps loading. Bump it only when a
`Kiln_*` structure's *layout* changes — the ABI section of the README says the
same thing, and structures grow at the end only.

## 2. Every surface, and who checks it

**Generated — never hand-edit. A stale generated page is a `git diff` away.**

| surface | written by | verified by |
|---|---|---|
| `docs-site/src/reference-commands.md` | `tools/gen-docs.sh` | `tools/check-docs.sh` compiles every `example` in every `_libinfo.c` row |
| `docs-site/src/reference-components.md` | `tools/gen-docs.sh` | same |
| `docs-site/src/reference-templates.md` | `tools/gen-docs.sh` | same |
| `docs-site/src/cli.md` | `tools/gen-docs.sh` | same |

**Hand-written — each one is verified differently, and one of them is not
verified at all.**

| surface | what must change | verified by |
|---|---|---|
| `README.md` | the unpack lines (`kiln-X.Y.Z-linux-x86_64.tar.gz`, `cd kiln-X.Y.Z-...`), the kit/command/component counts, and a sentence in the 1.x promise | **nothing.** This is where drift hides; `tools/check-release.sh` now guards it |
| `docs-site/landing/index.html` | `id="command-count"`, `id="kit-count"`, `id="component-count"` | `tools/check-docs.sh`, against `kiln kits` / `kiln commands` — it fails until they agree |
| `docs-site/src/limitations.md` | a solved limitation is **removed**, a newly-discovered gap is added | by hand, against the toolchain |
| the guide pages (`language`, `components`, `networking`, `databases`, `data-files`, `memory`, `build-targets`, `kits`, `interop`, `win-kit`, `studio`, `editors`, `forms-and-events`, `installation`, `quick-start`, `first-gui-app`, `introduction`) | whichever the release touches | `tools/check-docs.sh` compiles every whole-program sample; `mdbook build` and `tools/check-site.sh` check the links |
| `docs-site/src/SUMMARY.md` | a line for every new page | `tools/check-release.sh`: a page that is not listed is a page nobody reads |
| `libs/README.md` | the command-prefix table, when a kit lands | nothing |
| `docs/*.md` | the port records, when they name a state that changed | nothing — they say "open, deliberately" and stay true only if somebody looks |
| `Cargo.toml` | the workspace `version` | `tools/package.sh` refuses a binary whose reported version differs from it |
| `Cargo.lock` | refreshed by any build | — |
| `tools/bundle-README.md` | nothing: it carries `__VERSION__`, substituted at package time | `tools/package.sh` |
| `THIRD-PARTY.md` | when a dependency is added, removed or re-licensed | — |

## 3. The order

```sh
# 1. green, at the commit being released
cargo test
tools/gen-docs.sh && git diff --exit-code docs-site/src/reference-*.md docs-site/src/cli.md
tools/check-docs.sh

# 2. the version, in the two places a person reads
#    Cargo.toml: version.  README: the unpack lines and the 1.x promise.
cargo build --release           # refreshes Cargo.lock
tools/check-release.sh          # nothing left stale

# 3. commit and tag. Annotated, and the message says what a user gets.
git add Cargo.toml Cargo.lock README.md
git commit -F - <<'MSG'
Kiln X.Y.Z

<what changed, and what a program from the previous version sees>
MSG
git tag -a vX.Y.Z -m "Kiln X.Y.Z — <the one line>"
git push --follow-tags origin main

# 4. both platforms, and verify the one that can be verified
tools/package.sh
tools/verify-bundle.sh dist/kiln-X.Y.Z-linux-x86_64
tools/package-windows.sh        # needs vendor/RmlUi/build-windows

# 5. publish, both platforms, checksums beside them
gh release create vX.Y.Z --title "Kiln X.Y.Z" --notes-file notes.md \
   dist/kiln-X.Y.Z-linux-x86_64.tar.gz dist/kiln-X.Y.Z-linux-x86_64.tar.gz.sha256 \
   dist/kiln-X.Y.Z-windows-x86_64.zip dist/kiln-X.Y.Z-windows-x86_64.zip.sha256
```

Everything under step 1 is cheap and everything under step 4 is not, which is why
the order matters: a version bump that fails `check-docs` should fail before an
IDE has been built for two platforms.

## 4. Packaging an older tag

A release that was tagged but never packaged is packaged from a worktree. `vendor/`
and `target/` are untracked, so they are symlinked and shared rather than
rebuilt:

```sh
git worktree add /tmp/kiln-old vX.Y.Z
ln -s "$PWD/vendor" /tmp/kiln-old/vendor
cd /tmp/kiln-old
CARGO_TARGET_DIR="$OLDPWD/target" tools/package.sh
CARGO_TARGET_DIR="$OLDPWD/target" tools/package-windows.sh
```

`vendor/RmlUi/build-windows/librmlui.a` comes from
`tools/build-rmlui-windows.sh`, and it survives a `clean`; check it exists before
blaming the packaging.

## 5. What `tools/check-release.sh` catches

The four things that actually went stale:

1. the README's kit, command or component counts against the toolchain;
2. the README's unpack lines and any `kiln-X.Y.Z-<platform>` filename anywhere
   in the docs, against `Cargo.toml`'s version;
3. a documentation page that exists but is not in `SUMMARY.md`, so the book never
   shows it;
4. the generated reference pages, which must be what `tools/gen-docs.sh` produces
   right now — it regenerates and diffs them.

It does not check prose. Nothing can, which is why the port records and
`limitations.md` are on the list above rather than in the script.
