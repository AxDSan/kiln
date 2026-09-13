# Kiln 2.0 ("K2") — masterplan

The language itself is specified in [`spec.md`](spec.md); this file is the
order of work.

## Context

Kiln 1.x is a VB/EPL-shaped language positioned as "an open implementation of
EPL". Its author is not an EPL/VB6 programmer, does not enjoy writing it, and
does not want to serve an audience they are not part of. A C# server port made the
gap concrete: the same starter kit is a clean declarative table in C# and
string-built SQL in Kiln — partly the port, mostly missing abstraction (lambdas,
generics, records-as-rows, sized ints, visibility, multi-line literals).

K2 is the author's own language, for them and people who think alike:
**C#'s expressiveness, compiled natively — no runtime, no metadata in the
binary, no .NET — and a form you draw is the source.** It breaks every 1.x
program, that port included; that is accepted.

## Decisions taken

| Area | Decision |
|---|---|
| Syntax | C#-shaped: braces, `var`, `string`, `using`, `namespace`, `=>`, `switch` expressions, lambdas, generics, `record`/`class` with methods, `[Attributes]`, top-level `Main` |
| Naming | PascalCase for types, methods, properties, constants, namespaces, enum members; camelCase for locals/parameters. Standard library included (`Db.Open`, `File.ReadText`, `list.Count`) |
| Indexing | **1-based** positions; **0-based offsets** (byte offsets, pointer arithmetic, `[Packed]` layouts) |
| Ranges | `a..b` **inclusive** |
| Operators | `&&` `||` `!` logical; `&` `|` `^` `~` `<<` `>>` bitwise |
| Interpolation | `$"…{x}"` only; plain strings are literal (no brace escaping in JSON/SQL) |
| Errors | **`Result<T>`**, `?` to propagate, `otherwise` for a fallback. No exceptions |
| Types | classes and records with methods, **interfaces, no inheritance** |
| Null | only through `T?`; unchecked use is a compile error |
| Metaprogramming | **compile time only** — `[Table]`, `[Packed]` etc. generate code; no reflection, no type metadata in the binary |
| Forms | `form` stays a keyword; the designer owns its property block, user code in the same file |
| Kept | backend emission, runtime, collector, libraries' C implementations, designer, packaging, toolchain, `.kiln` |

## What the code allows (from exploration)

- **One AST, nothing below it.** `ir::Module` is read directly by the checker,
  desugar and `backend/src/lib.rs` (5,010 lines, emits LLVM text). The parser
  bakes 1.x semantics in (`check` → error-slot `if`, enum folding, dot-call →
  free function). `Ty` is `Copy` with `&'static` names and has no function
  types, generics, methods or unsigned ints; `Elem` forbids nested collections;
  `Expr` carries no spans. Retrofitting generics/closures/methods here costs as
  much as a new front end and would break 1.x while doing it.
- **Backend pieces are reusable**: typed direct calls to user code, the slot-ABI
  command marshalling, dll/stdcall calls incl. 32-bit x86, c-record layout,
  record/array/dict runtime helpers, debug info, GC root emission.
- **`Result<T>` needs no ABI change** — it is lowering over the existing error
  slot, generalising today's `T?`/`check` machinery. A *stored/returned* Result
  needs a two-part value.
- **Closures on events need an ABI change**: handlers are `void(*)(void)` with no
  env pointer; handler tables live in C/C++ memory the collector does not see.
- **Syntax lives outside `ir/` too** (ranked): Studio C++ writes forms/stubs and
  finds `sub`/`end` by text (`designer/model.cpp` `emit_form`/`save_model`,
  `designer/main.cpp` `handler_stub`/`opens_block`/`line_of_sub`); ~389 CLI
  tests with inline source; `cli/src/lsp_index.rs` is a 1.x token scanner;
  ~5.8k lines of hand-written docs; 57 examples, 4 templates, 6 `.kdecl`
  (2.6k lines), 92 libinfo examples; two editor grammars; debugger type-name
  display (`debug/src/value.rs`, `cli/src/dap.rs` `summary`).

## Architecture

```
.kiln (K2) ─▶ k2-syntax ─▶ k2-sema ─▶ k2-lower ─▶ KIR ─▶ backend ─▶ LLVM IR
               lexer        names       monomorphise    typed,      (existing
               parser       types       lambda-lift     monomorphic  emitter,
               AST+spans    generics    methods→fns     no sugar     split)
               printer      checking    Result, attrs
                                        forms/events
```

- **KIR (new crate `kir`)** is the seam the backend consumes instead of
  `ir::Module`: typed functions, locals, blocks, explicit-typed expressions,
  four call kinds (direct, command/slot ABI, indirect typed, dll), record and
  c-layout definitions, globals/GC roots, form/component build tables, spans on
  everything. No syntax, no sugar, no generics.
- **Why a new IR rather than widening `ir::Module`:** `Ty` is `Copy` with
  `&'static` names and `Elem` is a closed enum, so generic instantiations,
  function types and nested collections fight the representation everywhere;
  `Expr` has no spans; and every widening risks 1.x while it ships. KIR is
  built for the lowered shape only, so both front ends target it cleanly.
- **New crates** `k2-syntax`, `k2-sema`, `k2-lower` live beside `ir/` on `main`.
  1.x keeps building and shipping untouched until the flip.
- **The CLI becomes the writer as well as the reader**: a canonical printer
  (`kiln fmt`) and a tree-based `kiln edit` (form property/handler edits) replace
  Studio's C++ text splicing, extending the existing "the CLI is the only reader"
  convention.

## Phases

Each phase ends green and committed; nothing below Phase 10 changes what 1.x users get.

**0. Spec** — `design/k2/` language spec: grammar, type system, the decisions
above, stdlib naming map, 1-based/offset rule. Settle the remaining open items
(below). Reposition `design/brand.md` (drop "open EPL").

**1. KIR and a backend that reads it** *(de-risking step)*
- Split `backend/src/lib.rs` along its seams (module driver, forms/handlers,
  statements, aggregates/c-records, sugar, calls/ABI, declarations).
- Define `kir`; port the backend to consume it.
- Write `ir::Module → KIR` lowering for 1.x.
- **Exit:** (a) the entire 1.x suite (`cargo test`, designer tests,
  check-docs, verify-bundle) passes through KIR — KIR carries today's
  language; (b) hand-written KIR fixtures for what K2 needs and 1.x lacks —
  a closure with a captured env, a monomorphised generic with a mangled name,
  a method call, `uint16`/`uint32` arithmetic — compile and run. (a) alone
  would only prove a refactor; (b) proves KIR can represent K2.
- **1.x keeps shipping from `main` throughout** — KIR is invisible to users;
  1.4 can ship on it.
- **Stop point:** if Phase 1 is not green in **4 weeks**, stop and reassess
  (widen `ir::Module` instead, or reduce KIR scope) before any K2 front-end
  work.

**2. K2 syntax and its first customers** — lexer, error-tolerant parser, AST
with full spans, canonical printer, `kiln fmt`. In the same phase, the
text-level editor support moves onto `k2-syntax`, because Studio will be
editing K2 by Phase 6: the LSP index (`cli/src/lsp_index.rs` → parse tree:
symbols, go-to-definition, outline, completion scaffolding) and Studio's
code-view helpers (`designer/main.cpp` `opens_block`/`handler_stub`/
`line_of_sub`, `highlight.h` keyword list) served by the CLI instead of C++
text scanning. **Exit:** parse → print → parse round-trips a corpus of K2
samples; the LSP outlines and navigates a K2 file; Studio's code view
highlights, indents and jumps to methods in a K2 file (verified by
rendering).

**3. K2 core semantics** — namespaces and `using`, visibility, classes/records
with methods and constructors, interfaces (static dispatch first), enums as real
types, `uint8..uint64`/`int8..int64`/`nint`, `string`, `T?` with flow checking,
`Result<T>` + `?` + `otherwise`, inclusive ranges, `switch` expressions,
interpolation, multi-line literals. Lower to KIR. **Exit:** hello, fizzbuzz,
records, match-style examples compile and run from K2 source.

**4. Standard library surface** — add a `namespace`/member mapping to libinfo
(or derive from prefixes) so `db_open` is `Db.Open`, `length(s)` is `s.Length`;
compiler-known `List<T>`/`Dictionary<K,V>`; `Result`-returning wrappers
generated over the error slot. The toolchain's own inputs migrate here, not
in Phase 9: the 6 `.kdecl` files (2.6k lines) and the 92 libinfo `example`
fields become K2, since the generated reference is built and compiled from
them. **Exit:** reference docs regenerate in PascalCase and `check-docs.sh`
compiles every libinfo example as K2.

**5. Generics and lambdas** — type parameters, monomorphisation with mangled
symbols, `Func<…>` types, closure capture into collector-traced env objects,
lambda lifting, typed indirect calls; `Select`/`Where`/`OrderBy`/`Any`/`First`
on `List<T>` written in K2. Interface dispatch via vtables if needed. **Exit:**
the approved `starter.kiln` sketch's `KitFor`/`WeaponFor` compile and run.

**6. Forms and events** — `form` blocks; `Click += OnAdd`; **ABI v5**: handler
`env` pointer, handler lists, runtime subscribe, a rooted handler registry in
`runtime/kn_gc.c` (update `kn_component.c`, `kn_loop.c`, `libs/ui/ui_rmlui.cpp`,
net components). `kiln edit` for form round-trip; remove Studio's text
splicing, keyword lists and `sub`/`end` scanning. **Exit:** counter and gallery
examples run; Studio drag-and-drop edits K2 files (verified by rendering).

**7. Compile-time attributes** — attribute framework as a pre-lowering pass;
`[Table]`/`[Column]`/`[Auto]` generating insert/select over `libs/db`;
`Query<T>(x => …)` translated to parameterised SQL at compile time (comparisons,
`&&`/`||`, member access, captured values only); `[Packed]` generating binary
`Read`/`Write`; `[Dll]`/`[CLayout]` interop replacing `dll`/`is c`.
**Exit:** the sketch's `Outfit`/`Inventory` run against SQLite and MariaDB.

**8. Tooling** — LSP semantic features on `k2-sema` (types on hover,
diagnostics, typed completion; the syntax-level index landed in Phase 2); VS Code and Kate grammars; debugger type names and value
display (`debug/src/value.rs`, `cli/src/dap.rs`); debug info for lambdas and
monomorphised instances.

**9. Migration** — `kiln migrate`: 1.x AST → K2 AST → printer. Converts the
mechanical 90% (syntax, naming, indexing kept); flags semantics it cannot
decide (error-slot idioms → `Result`). Run it over the corpus — examples,
templates, inline test sources (`.kdecl` and libinfo examples already moved in
Phase 4). Rewrite `language.md` and the
teaching pages; update `gen-docs.sh` wrapping and the `check-docs.sh` sample
regex; README, landing page.

**10. Release 2.0** — K2 becomes the default; delete `ir/` front end and 1.x
paths; cut a `1.x` maintenance branch from the last 1.x release. **Acceptance:**
the server port's login server migrated to K2 and run against its real client
(ten logins in a row from a fresh launch, the 750 ms reply delay kept).

## Coordination

- The 1.x server port is frozen; it resumes in K2 at Phase 10.
- K2 work stays on `main` in new crates; 1.x maintenance releases remain
  possible from `main` until the flip.

## Settled in Phase 0

`defer` stays; `??` is the fallback for `T?` and `Result<T>`; interface values
use vtables, constrained generics monomorphise; records compare by value,
classes by reference. See [`spec.md`](spec.md) §14.

## Verification

- **Phase 1:** full 1.x gate unchanged and green through KIR — `cargo test
  --release`, `designer/build.sh
  test`, `tools/check-docs.sh`, `tools/check-release.sh`, `tools/verify-bundle.sh`.
- **Every K2 phase:** golden tests — parse/print round-trip, and compile+run
  with expected stdout — added under `cli/tests/k2_*.rs`; the 1.x gate stays
  green alongside.
- **UI phases:** render frames headlessly (`KILN_DESIGNER_DUMP`,
  `SDL_VIDEODRIVER=offscreen`) and look at them.
- **Cross targets:** Windows x64 and x86 builds run under wine.
- **Final:** the migrated login server against its real client; `verify-bundle`
  on the 2.0 bundle.
