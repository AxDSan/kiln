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
- `static class`, `class`, `record` (positional + body), `enum` (with backing
  type) — usable as a type, resolving to its backing integer
- methods: static and instance (implicit `this`), expression-bodied `=>`
- primitive types: `int/uint/long/ulong/short/ushort/byte/sbyte/nint/nuint/float/double/bool/char/string`
- `var`/`let`/typed locals; fields; `const` (literal, inlined)
- `if`/`else`, `while`, C-style `for`, `foreach` over an inclusive/half-open range
- arithmetic, comparisons, bitwise, shifts — **unsigned ops select udiv/ult/lshr**
- short-circuit `&&`/`||`, ternary `?:`, unary `- ! ~`, casts `(T)e`
- `new T(...)` / `new T { f = v }`, field access, `this.field`
- `$"…{expr}…"` interpolation **as a value** (assignable, returnable) and
  `string + string` concatenation, built with libc `snprintf`/`malloc`;
  `Console.WriteLine`/`Write` lower to `printf`
- enum members and `Type.Const` references
- **generic methods**: `T Max<T>(T a, T b)` — type arguments inferred from the
  arguments, monomorphised per instantiation with mangled symbols, instances
  cached so one type argument set emits one function (`where` clauses parse;
  constraints are not yet enforced)
- **lambdas**: `x => e`, `(a, b) => e`, `() => { … }` against a `Func<...>` /
  `Action<...>` target type — lifted to their own function and called
  indirectly through the KIR `{fn, env}` closure pair
- **capturing closures, by reference**: a captured local or parameter lives in a
  heap cell; a lambda's environment is a record of pointers to the cells it
  needs, so both sides read and write the same memory. A closure sees writes
  made after it was built, and its own writes are visible outside
- **`Result<T>`**: synthesised per value type as `{ok, value, err}`;
  `return x;` is an implicit `Ok`, `Error("…")` a failure, `?` propagates out of
  a Result-returning method, `??` supplies a fallback, and `.IsOk`/`.IsErr`/
  `.Value`/`.Error` read it. No exceptions anywhere
- **`T?` optionals**: `return null;`, implicit `T` → `T?` wrapping, `x != null` /
  `x == null` as presence tests that **narrow** the variable in the proven
  branch (so it reads as `T`), `??` fallback, `.HasValue` / `.Value`
- **`List<T>`**: `new List<T>()`, `.Add(x)` (buffer grows 4 → 8 → 16 …),
  `.Count`, 1-based indexing `xs[1]`, and `foreach` over one. KIR gained real
  indexed element access for this
- **`[Dll]` interop**: `[Dll("c")] public static extern int abs(int n);` calls a
  C function directly, with `Entry = "..."` to rename and
  `Convention = CallConv.StdCall` for Win32. Attributes parse on types and
  members generally
- **`Dictionary<K,V>`**: `new Dictionary<K,V>()`, `d[k] = v` (updates in place
  or appends), `.Get(k)` → `V?`, `.ContainsKey(k)`, `.Count`. String keys
  compare by content. Lookup is a linear scan for now
- **`Where` / `Select`** over a `List<T>`, taking a lambda and chaining;
  `Select` infers its result element type from the lambda body, so
  `xs.Where(x => x > 8).Select(x => $"n{x}")` turns a list of int into a list
  of string
- **`switch` expressions**: `x switch { 1 => …, < 10 => …, _ => … }` with
  constant and relational patterns, lowered to an if-chain into one temporary
- **`defer`**: runs when its block is left — falling off the end, `return`,
  `break` or `continue` — with several defers in a block unwinding in reverse
  declaration order

**Parsed but not yet lowered** (parser accepts; lowering errors clearly):

- `x!` (null-forgiving) and `x?.M`; `??` covers both `Result` and `T?`
- generic type arguments (`List<T>` maps to an array type; others rejected)
- interface bases (`: I` parsed and ignored)

**Not yet built** (next phases, in rough order):

1. `HashSet<T>`; hashing for `Dictionary` (lookup is a linear scan); moving strings, lists and dictionaries off libc onto the Kiln runtime and collector
2. richer null flow (narrowing through `&&`, early `return`, `is T v` patterns) — the `if (x != null)` form is done
3. generic *types* (`class Cache<K,V>`) and generic instance methods — only generic static methods are built
4. capturing a `foreach` loop variable (locals and parameters are done; loop variables are reported, not compiled)
5. the standard-library surface (Phase 4): real `File.`, `Db.`, `s.Length`, etc., replacing the `printf` shim
6. the generating attributes: `[Table]`, `Query<T>`, `[Packed]` (`[Dll]` is done)
7. forms + events + ABI v5
8. LSP, `kiln migrate`, folding K2 into `kiln build`/`run` with runtime linking

## Milestone

`examples/k2/starter.kiln` — the program that motivated Kiln 2 — compiles and
runs, and is covered by a test. In 1.x it was a dictionary keyed by stringified
indexes and SQL built by concatenating `int_to_text()` calls; in K2 it is a
record, a `List<T>`, a `Select` lambda and a `switch` over an enum.

## Known shortcuts to revisit

- **Records are C-layout + `malloc`**, even `class`. Managed records / GC
  integration and reference vs value equality land with the runtime work.
- **`Console.WriteLine` is `printf`**, strings are built with
  `snprintf`/`malloc`, and `List<T>` uses `malloc`/`realloc` — not the Kiln text
  runtime or collector. Nothing built this way is freed. Replaced in Phase 4.
- **The K2 path links libc only** via a generated `main` shim; no runtime, no
  collector roots. Folded into `kiln build` (with runtime linking) later.
- **One `k2` crate** holds syntax + lowering; splits into `k2-syntax`/`k2-sema`/
  `k2-lower` as `k2-sema` grows (it is currently folded into the lowerer).
- Parser cast heuristic `(T)e` is conservative; revisit with the full grammar.
