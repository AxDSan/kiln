# Kiln 2 — implementation status

Updated 2026-09-13. Tracks what of the [spec](spec.md) is actually built, so the
[plan](plan.md)'s phases can be checked off against running code.

## Crates

| Crate | Role | State |
|---|---|---|
| `kir` | the typed middle IR + LLVM emitter | types, emitter, 5 tests (4 exit fixtures + slot-ABI) |
| `k2` | lexer + parser + lowerer (k2-syntax/k2-lower, one crate for now) | runnable subset, 6 end-to-end tests |
| `backend` | 1.x → LLVM, split into `lower/` modules | unchanged behaviour, 61 tests |
| `cli` | `kiln k2 <file> [-o] [--run] [--emit-ir] [--runtime]` | builds+runs K2; libc by default, or the real Kiln runtime with `--runtime` |

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
- **Forms — the RAD half**: a `form` block declares the window's properties and
  its components, `Click += OnAdd` wires an event, form fields hold state
  between events, and `count.Text = $"{n}"` sets a property at run time. It
  lowers to the same `kn_ui_*` interface the 1.x backend emits, so it renders
  through the real UI runtime (`examples/k2/counter.kiln`). **`partial form`**
  splits the designer's half from yours, which is the shape Studio will edit.
  An event wires to a **method group or a lambda written at the wiring site** —
  a form's state is in globals, so a handler lambda needs no environment
  pointer and binds on the ABI as it stands
- **Generic types**: `record Pair<A, B>` and `class Box<T>` instantiate per set
  of type arguments — fields substituted, methods declared and lowered per
  instance, with the template's own name meaning that instance inside it
- **Constructors**: `public Box(T v) { value = v; }` — a zeroed instance is
  bound to `this`, the body runs (bare field names resolve through it, for
  reads and writes alike), and the instance is returned. `new T(...)` prefers a
  declared constructor over positional fields
- **Interfaces, no inheritance**: `interface I { … }` with `class C : I` /
  `record R(...) : I`. An interface value carries the object and the
  implementation's methods, so two unrelated types work through one interface
  and a `List<IShape>` holds both; a missing implementation is a compile error
- **The Kiln runtime, opt-in**: `kiln k2 --runtime` links the runtime and
  support libraries exactly as a 1.x build does, and `Console.WriteLine`
  becomes the `print_text` command over the slot ABI instead of `printf` —
  the first K2 program to reach the real runtime
- **`[Table]` with compile-time SQL**: `[Table("...")]`, `[Column("...")]`,
  `[Auto]` on a record give `T.InsertSql()` and `T.SelectSql(x => ...)`, where
  the predicate is translated to a parameterised `where` clause **at compile
  time** — comparisons, `&&`/`||`/`!`, the row's own columns and constants.
  Captured values become `?`. Anything else is a compile error naming it, so a
  query never silently runs in the wrong place. Columns default to snake_case
- **`[Packed]`**: a record laid out with no padding, plus generated
  `T.Size`, `T.Read(bytes, offset)` and `r.Write(bytes, offset)` over a
  `Bytes` buffer (`Bytes.Alloc(n)`, `buf[i]`) — an exact binary layout, which
  is what protocol work needs
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

1. `HashSet<T>`; hashing for `Dictionary` (lookup is a linear scan); moving strings, lists and dictionaries onto the runtime's own text/array commands and the collector (the link path now exists; the data structures still use libc)
2. richer null flow (narrowing through `&&`, early `return`, `is T v` patterns) — the `if (x != null)` form is done
3. generic *instance* methods, and interface constraints on generics (parsed, not enforced)
4. capturing a `foreach` loop variable (locals and parameters are done; loop variables are reported, not compiled)
5. the standard-library surface (Phase 4): real `File.`, `Db.`, `s.Length`, etc., replacing the `printf` shim
6. binding the generated SQL to `libs/db` and returning rows (the statement text is generated; execution needs the runtime)
7. ABI v5 proper: a handler that captures a *local* (form state and method groups both work without it); Studio editing K2 through `kiln edit`
8. LSP, `kiln migrate`, folding K2 into `kiln build`/`run` with runtime linking

## Milestone: the RAD half runs

`examples/k2/counter.kiln` is a Kiln 2 window — drawn in the `form` block, wired
with `Click += OnAdd`, built by `kiln k2` into a native binary, and rendered by
the real Kiln UI runtime. Verified by dumping a frame and looking at it, as
Studio work must be.

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
