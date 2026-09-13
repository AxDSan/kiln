# Kiln 2 — implementation status

Updated 2026-09-13. Tracks what of the [spec](spec.md) is actually built, so the
[plan](plan.md)'s phases can be checked off against running code.

## Crates

| Crate | Role | State |
|---|---|---|
| `kir` | the typed middle IR + LLVM emitter | types, emitter, 5 tests (4 exit fixtures + slot-ABI) |
| `k2` | lexer + parser + lowerer (k2-syntax/k2-lower, one crate for now) | runnable subset, 6 end-to-end tests |
| `backend` | 1.x → LLVM, split into `lower/` modules | unchanged behaviour, 61 tests |
| `cli` | `kiln k2 <file> [-o] [--run] [--emit-ir]` | builds+runs K2 via clang, libc only |

## Language: built vs. pending

**Built and running** (each covered by an end-to-end test that compiles with
clang and checks stdout):

- `namespace` (file-scoped and block), `using` (parsed)
- `static class`, `class`, `record` (positional + body), `enum` (with backing type)
- methods: static and instance (implicit `this`), expression-bodied `=>`
- primitive types: `int/uint/long/ulong/short/ushort/byte/sbyte/nint/nuint/float/double/bool/char/string`
- `var`/`let`/typed locals; fields; `const` (literal, inlined)
- `if`/`else`, `while`, C-style `for`, `foreach` over an inclusive/half-open range
- arithmetic, comparisons, bitwise, shifts — **unsigned ops select udiv/ult/lshr**
- short-circuit `&&`/`||`, ternary `?:`, unary `- ! ~`, casts `(T)e`
- `new T(...)` / `new T { f = v }`, field access, `this.field`
- `$"…{expr}…"` interpolation and `Console.WriteLine`/`Write` (lowered to `printf`)
- enum members and `Type.Const` references
- **generic methods**: `T Max<T>(T a, T b)` — type arguments inferred from the
  arguments, monomorphised per instantiation with mangled symbols, instances
  cached so one type argument set emits one function (`where` clauses parse;
  constraints are not yet enforced)
- **lambdas**: `x => e`, `(a, b) => e`, `() => { … }` against a `Func<...>` /
  `Action<...>` target type — lifted to their own function and called
  indirectly through the KIR `{fn, env}` closure pair. Non-capturing only; a
  capture is reported (`captures \`k\``), never mis-compiled

**Parsed but not yet lowered** (parser accepts; lowering errors clearly):

- `T?`, `??`, `?` propagation, `defer`
- generic type arguments (`List<T>` maps to an array type; others rejected)
- interface bases (`: I` parsed and ignored)

**Not yet built** (next phases, in rough order):

1. `List<T>`/`Dictionary<K,V>` against the real Kiln runtime (currently only the array *type* exists; no element ops link a runtime)
2. `Result<T>` + `?` + `??` over the error slot
3. generic *types* (`class Cache<K,V>`) and generic instance methods — only generic static methods are built
4. *capturing* lambdas: hoist captured locals into an env record at their declaration (KIR already lowers by-reference capture)
5. the standard-library surface (Phase 4): real `File.`, `Db.`, `s.Length`, etc., replacing the `printf` shim
6. compile-time attributes: `[Table]`, `Query<T>`, `[Packed]`, `[Dll]`
7. forms + events + ABI v5
8. LSP, `kiln migrate`, folding K2 into `kiln build`/`run` with runtime linking

## Known shortcuts to revisit

- **Records are C-layout + `malloc`**, even `class`. Managed records / GC
  integration and reference vs value equality land with the runtime work.
- **`Console.WriteLine` is `printf`**, not the Kiln runtime. Replaced in Phase 4.
- **The K2 path links libc only** via a generated `main` shim; no runtime, no
  collector roots. Folded into `kiln build` (with runtime linking) later.
- **One `k2` crate** holds syntax + lowering; splits into `k2-syntax`/`k2-sema`/
  `k2-lower` as `k2-sema` grows (it is currently folded into the lowerer).
- Parser cast heuristic `(T)e` is conservative; revisit with the full grammar.
