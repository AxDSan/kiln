# Kiln 2 — implementation status

Updated 2026-09-15. Tracks what of the [spec](spec.md) is actually built, so the
[plan](plan.md)'s phases can be checked off against running code.

## Crates

| Crate | Role | State |
|---|---|---|
| `kir` | the typed middle IR + LLVM emitter | types, emitter, **module variables as collector roots**, 8 fixtures |
| `k2` | lexer + parser + lowerer (k2-syntax/k2-lower, one crate for now) | the language guide's surface, 70 end-to-end tests |
| `backend` | 1.x → LLVM, split into `lower/` modules | unchanged behaviour, 61 tests |
| `k2::print` | the canonical printer — `kiln fmt` | fixed point, comments carried, meaning unchanged |
| `kir::debug` | DWARF line tables | a K2 binary is steppable in gdb |
| `cli::lsp_k2` | the language server's K2 half | diagnostics, outline, formatting, hover, completion |
| `k2::edit` | `kiln edit` — tree-level changes to a form | Studio's writer; everything untouched comes back unchanged |
| `k2::migrate` | `kiln migrate` — 1.x source → K2 source | all 41 1.x examples convert to valid K2 |
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
- **Debug information**: a K2 binary carries a DWARF line table — a compile
  unit naming the source, a subprogram per function, and a location on every
  instruction — so `gdb` breaks on a K2 line, steps from one to the next, and
  prints a backtrace naming both frames. **Variables are described too**: each
  named local and parameter is declared against the slot that holds it, so
  `info args` and `info locals` print them by name and value. Compiler-invented
  names are left out — they are machinery, not the user's variables. **A record
  is described by its fields**, so `ptype r` prints the struct and `p *r` gives
  `{W = 3, H = 4}` rather than an address. **An optional is described as the
  `{ value, present }` struct it is**, so a `T?` local reads as the value it
  holds or as `nothing` — where before it was one unnamed pointer, and an
  `int?` read as an address and a `string?` as text
- **Editor grammars**: the VS Code and Kate definitions highlight K2 — `//` and
  `/* */` comments, the keyword and type sets, `[Attributes]`, `$"…{x}…"`
  interpolation — while still highlighting 1.x, since both share `.kiln` during
  the transition. A test reads the grammars against the keyword set so they
  cannot drift from the lexer
- **Language server**: a `.kiln` file is recognised as K2 or 1.x by its first
  line, and a K2 file gets diagnostics from the front end itself — the same
  parse and lowering a build runs, so the editor cannot disagree with the
  compiler — plus a document outline (types, members, a form's components),
  whole-document formatting, **hover** (a declaration with its signature, or a
  sentence for a keyword that has none) and **completion** (what the file
  declares plus the language's words; after a dot, only that thing's members).
  A failed parse is retried with the caret's line blanked out, since asking
  what follows a dot means looking at a file that does not parse
- **`kiln inspect` reads a K2 file**, in exactly the lines it gives for 1.x —
  `module:`, `use:`, `sub:`, `form:`, `component:`, `prop:`, `handler:` — so
  Studio reads a K2 form without a second parser and without being changed. A
  `partial form` reports once, with a span covering both halves, and names are
  reported in the spelling the toolchain files things under (`Label` is
  `label`, `BackgroundColor` is `background_color`) so a descriptor is found
- **Studio opens a Kiln 2 form**: the window, its components and their handler
  wiring all render, and the Events tab shows `click`/`on_add` as it would for
  1.x — verified by dumping a frame and looking at it, and pinned by a scripted
  session in the designer's own tests
- **`kiln edit <file> sync`**: a whole form, described in exactly the lines
  `kiln inspect` prints, applied through the tree. That is the shape a
  designer's save has to take — it holds a form and has no record of which
  single edit got it there — and it makes the CLI the only writer as well as
  the only reader. An untouched save is byte-identical; the code half, its
  comments and the designer block's own comments all survive. `stub` adds the
  empty handler a wiring gesture leaves behind, in the half that holds code
- **Studio saves a Kiln 2 form**: dragging a component and saving changes only
  the two coordinates in the file, and the whole RAD gesture — drop a button,
  double-click it — writes `Button button1` into the designer's half and
  `void Button1Click()` into yours, wired and compiling. Its own tests drive
  the gestures and read the file back
- **Studio's code pane highlights K2**: `//` comments, C#'s keyword set and
  `$"…"` as one string. It had 1.x's rules only, which left K2 unpainted and —
  because `//` was not a comment — painted the English inside one
- **`kiln edit`**: `set`, `add`, `remove`, `rename`, `on` change a form's
  designer block **through the tree**, not as text — so comments, the code half
  and everything else survive. This is what Studio calls: the CLI has always
  been the only reader of a project file, and is now the only writer too
- **Documentation**: `docs-site/src/kiln-2.md` is the language guide and
  `moving-to-kiln-2.md` the migration guide. Every ```k2 block in the book is
  built by the same `kiln k2` a reader would type, from two gates, and the
  migration guide's worked example is checked against what `kiln migrate`
  actually prints — so the page cannot teach a conversion nobody will get
- **`kiln migrate`**: a 1.x program becomes K2 source — module → namespace and
  a static class, `sub` → method, records, consts, `use` → `using`, naming to
  PascalCase/camelCase, `if`-arms nested, concat chains rebuilt as `$"…"`
  interpolation, and `@keyword` where a name now collides. What it will not
  decide (a `match`, an indirect call, a form) gets a `// TODO(migrate):`
  comment above it. **Positions are not rebased** — K2 counts from 1 as 1.x
  does. Every shipped 1.x example converts to valid K2
- **`kiln fmt`**: one canonical spelling per program — four-space indent,
  braces on their own line, precedence-aware parentheses. Formatting is a fixed
  point, does not change what a program compiles to, and **carries comments**:
  ordinary and doc comments ride the tree and are put back, so the shipped
  examples reformat without losing a line
- **Generic instance methods** (`t.With(7)`), and **interface constraints
  enforced**: `where T : INamed` is checked when the type argument is chosen,
  and a type that does not implement it is a compile error naming both
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
- **The standard library**: `using Kiln.File;` loads that library and
  `File.ReadText(p)` resolves to its `file_read_text` command — the spec's
  naming rule read backwards at the call site, with the bare name tried too for
  libraries that do not prefix (`Text.Uppercase` → `uppercase`). Arguments and
  the result cross the slot ABI with the signature's own tags, so the whole
  command set is reachable rather than a hand-written handful. A command whose
  first parameter is the receiver is written as a member: `length(s)` is
  `s.Length`, `uppercase(s)` is `s.Uppercase()`. A name in scope is a value, so
  `s.Uppercase()` is never mistaken for a static call. **A command's failure
  becomes a `Result`**: where a `Result<T>` is expected — or under `??` — the
  call is wrapped by reading the error slot it writes, so a missing file is a
  value rather than a sentinel. Only a Result that is stored, passed or
  returned is materialised, as the spec's §9 lowering has it. `?` applies
  straight off a call too, so `File.ReadText(p)?` hands a failure back to the
  caller
- **The Kiln runtime, opt-in**: `kiln k2 --runtime` links the runtime and
  support libraries exactly as a 1.x build does; `Console.WriteLine` becomes
  the `print_text` command over the slot ABI, and **every allocation comes from
  the collector** — records, strings, lists, dictionaries, sets and packed
  buffers alike. A program that allocates megabytes across 400 rounds holds
  49 bytes after a collection
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
- **`HashSet<T>`**: `new HashSet<T>()`, `.Add(x)` (a duplicate is ignored),
  `.Contains(x)`, `.Count`, and `foreach` over one. String elements compare by
  content, as dictionary keys do. **Lookup is hashed**, over the same
  open-addressed index a dictionary uses
- **`Dictionary<K,V>`**: `new Dictionary<K,V>()`, `d[k] = v` (updates in place
  or appends), `.Get(k)` → `V?`, `.ContainsKey(k)`, `.Count`. String keys
  compare by content. Lookup is a linear scan for now
- **`Where` / `Select`** over a `List<T>`, taking a lambda and chaining;
  `Select` infers its result element type from the lambda body, so
  `xs.Where(x => x > 8).Select(x => $"n{x}")` turns a list of int into a list
  of string
- **`OrderBy`** over a `List<T>`, taking a key selector — `pts.OrderBy(p => p.X)`
  — beside `Where`/`Select`/`Any`/`First`. It answers with a **new** list (the
  source is untouched, where `Sort` is in place) and is stable, so equal keys
  keep their order. The key must be a number, a bool, a char or a string,
  because there is no user comparator to call; anything else is refused by name
  where it is written
- **`switch` expressions**: `x switch { 1 => …, < 10 => …, _ => … }` with
  constant and relational patterns, lowered to an if-chain into one temporary
- **`defer`**: runs when its block is left — falling off the end, `return`,
  `break` or `continue` — with several defers in a block unwinding in reverse
  declaration order
- **A program's module variables are collector roots**: the emitter writes a
  `@kn_gc_roots` table of the pointer-typed globals and calls
  `kn_gc_set_roots` before the entry function can run a static field's initial
  value. `GlobalDef::is_gc_root` had existed since KIR was written and the
  emitter ignored it, so a collection freed anything a program held only in a
  module variable — reachable from nowhere else, and found by no stack scan.
  Only in an executable and only with the runtime linked, which is 1.x's rule.
- **`x!` and `x?.`**: `x!` asserts an optional is present and reads as its
  plain type; `x?.Member` and `x?[i]` evaluate to nothing when the receiver is
  empty, without evaluating the member or the index. `x!` on a `Result` is a
  compile error, since the two absences are not the same thing.
- **`Any` and `First`** over a `List<T>`, taking a predicate lambda, beside
  `Where`/`Select`. `First` with no match stops the program by name rather than
  answering with a zero.
- **Written type arguments**: `Make<int>()`, `P.Two<string>(x)` — for a generic
  method with nothing to infer from, and to set the width a literal argument
  lowers to (`Pick<long>(5, 6)`). Told from `a < b` by C#'s rule: a complete
  argument list followed by `(`. User generic types nest as well
  (`Box<Pair<Point, string>>`).
- **Constraints beyond interfaces**: `where T : class` and `where T : new()`
  are enforced where the type argument is chosen, naming both the type and the
  constraint when they do not hold.
- **A format spec in an interpolation is refused by name.** `$"{x:03}"` used to
  drop the spec silently and print `7` where the writer asked for `007`, which
  is the one outcome an interpolation must not have; the hole is now parsed as
  a whole expression and a leftover `:` is reported. That also fixed a ternary
  inside a hole, which the spec splitter had been truncating.
- **A ternary narrows an optional the way an `if` does**, and its arms unify
  when they differ only in optionality — `n == 1 ? "one" : null`. Both used to
  store a `{value, present}` pair into a slot typed from the other arm, which
  clang refused.
- **Every target builds from Kiln 2 source**: console and GUI programs,
  `sharedlib` and `staticlib` with the C header, and the Windows x64/x86 cross
  builds. `kiln build` names a library's entry file and reads the project
  beside it for the target, because a Kiln 2 program does not declare one in
  its source.

- **`[Table]` runs**: `T.Insert(h, row)` and `T.Select(h, x => pred)` build the
  statement at compile time and execute it through `libs/db` — the predicate
  becomes parameterised SQL, captured values bind rather than being pasted, and
  each column is read back with the reader its declared type asks for, into a
  `List<T>`. `InsertSql`/`SelectSql` still answer with the text alone
- **Phase 7's exit runs**: the starter sketch's inventory as a `[Table]` record —
  `Outfit` stocks a character from the kit, `Inventory` reads the bag and its
  consumables back — gives the same answers against SQLite and against MariaDB
  10.11 (`cli/tests/k2_runtime.rs`; the MariaDB half runs when
  `KILN_TEST_MYSQL_DSN` names a server)
- collection expressions `[a, b, c]` and `[]`, target-typed to an array — what
  a command taking a list of values (`db_exec`'s parameters) expects
- **every command argument is checked against the slot its signature declares**,
  and named in words when it does not match. A `List<T>` where a list of `T` is
  declared is converted rather than refused, and a command answering with a list
  converts back — so `List<T>` stays the only list a K2 program sees
- **ABI v5 — events wired at run time**: `button.Click += () => …` and
  `-= OnGo` in code, the handler capturing what it needs. The UI library binds
  a (function, environment) pair, any number per event, and unbinds by the same
  pair a delegate compares. The environment is held in a rooted,
  collector-allocated table in `runtime/kn_handlers.c` for exactly as long as
  the handler is bound. A test churns the heap and forces a collection between
  wiring and clicking; with the hold removed the same build segfaults on the
  first click, which is how the test is known to test something
- a `[Dll]` extern declared inside a form calls its function (it compiled to an
  empty body returning 0)
- interface bases (`: I` parsed and ignored)
- closures over a `foreach` variable: each turn binds its own cell, so a lambda
  made in a loop holds that turn's value (C#'s post-5.0 rule), while a write
  within the turn is still shared. `Action` with no type arguments as well

**Not yet built** (next phases, in rough order):

1. Richer null flow: `x != null` narrows, and `x!` and `x?.` are built, but
   narrowing through `&&`, through an early `return` and through an `is T v`
   pattern is not.
2. A handler wired at run time cannot yet take an event's arguments (a grid's
   row) — `kn_ui_on_env` refuses those with 2 rather than calling wrongly.
3. The `.kdecl` declaration bundles (2,653 lines across `kits/`) are still
   Kiln 1.x: Kiln 2 reaches a kit's *commands*, but its declared structs and
   constants do not cross yet. This is Phase 4's remaining exit.
4. Studio's code pane highlights K2 from its own line tokenizer rather than
   from the language server's semantic tokens — the shapes are right, but the
   toolchain is not the one deciding them.
5. `List<T>` is K2's own structure, converted to and from a runtime array at the
   command boundary. Re-platforming it onto `Kiln_Array` buys nothing now that
   allocation is collected.
6. One `k2` crate holds syntax + lowering; it splits into `k2-syntax`/
   `k2-sema`/`k2-lower` as `k2-sema` grows.

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

- **Records and classes are C-layout structs with no runtime type tag.** What a
  program can do with one is fixed when it compiles, which is the point: no
  metadata reaches the binary.
- **The libc-only path is a subset, and `kiln build` never takes it.** `kiln k2
  <file>` with no `--runtime` links libc alone behind a generated `main`, and
  there strings come from `snprintf`/`malloc`, `Console.WriteLine` is `printf`,
  and nothing is freed. `kiln build` always links the runtime and its collector,
  which is what a shipped program gets.
- **One `k2` crate** holds syntax + lowering; splits into `k2-syntax`/`k2-sema`/
  `k2-lower` as `k2-sema` grows (it is currently folded into the lowerer).
- Comments are carried as *leading* trivia, so one written at the end of a line
  moves above the next construct. Blank lines between constructs are
  normalised rather than preserved — the printer puts one between components
  and between methods, and drops the rest.
- Parser cast heuristic `(T)e` is conservative; revisit with the full grammar.
