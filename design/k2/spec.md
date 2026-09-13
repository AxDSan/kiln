# Kiln 2 — language specification

Status: **draft, Phase 0**. This is the contract the K2 front end
(`k2-syntax`, `k2-sema`, `k2-lower`) is built against. Where this file and the
code disagree, one of them is a bug; decide which and fix it here first.

Kiln 2 is C#'s expressiveness compiled natively — no runtime to install, no
metadata in the binary, no .NET — with a form you draw as the source. It is
not source-compatible with Kiln 1.x. `kiln migrate` (Phase 9) converts 1.x
programs.

## 1. Decisions

| Area | Rule |
|---|---|
| Shape | C#: braces, `;`, `var`, `using`, `namespace`, `=>`, `switch` expressions, lambdas, generics, `[Attributes]` |
| Naming | PascalCase: types, methods, properties, fields that are public, constants, namespaces, enum members. camelCase: locals, parameters, private fields. The standard library follows it (`File.ReadText`, `list.Count`) |
| Positions | **1-based**: list/string indexes, `Find` results, `Substring` starts, `for` over a collection's indexes |
| Offsets | **0-based**: byte offsets into a `Bytes`, pointer arithmetic, `[Packed]`/`[CLayout]` field offsets. A value named `...Offset` is 0-based; one named `...Index`/`Position` is 1-based |
| Ranges | `a..b` is **inclusive** at both ends. `a..<b` is half-open |
| Logical ops | `&&` `\|\|` `!` |
| Bitwise ops | `&` `\|` `^` `~` `<<` `>>` (`>>` is arithmetic on signed, logical on unsigned) |
| Interpolation | `$"…{expr}…"` only. Plain `"…"` is literal; `{` needs no escape in it |
| Errors | `Result<T>`; `?` propagates; `??` falls back. **No exceptions** |
| Absence | only through `T?`; reading a `T?` as `T` without a check is a compile error |
| Types | `class` (reference, reference equality), `record` (reference, value equality, immutable by default), `enum`, `interface`. **No inheritance** |
| Dispatch | generic constraints are monomorphised (static); an interface-typed value is a fat pointer with a vtable |
| Cleanup | `defer` stays |
| Immutability | `let` declares an immutable local (not C#, kept from 1.x); `var` is mutable; `readonly` fields |
| Metaprogramming | compile time only. No reflection, no type metadata in the binary |
| Memory | the existing conservative collector. No destructors, no finalisers |
| Forms | `form` is a keyword; the designer owns the property block |
| Files | `.kiln`, UTF-8, one or more per project |

## 2. Lexical structure

- **Comments**: `// line`, `/* block */` (nests). `///` is a doc comment and
  attaches to the next declaration; its text is Markdown (not XML).
- **Identifiers**: `[A-Za-z_][A-Za-z0-9_]*`, plus `@keyword` to use a keyword as
  a name. Case-sensitive.
- **Keywords**: `namespace using public private internal static const var
  let class record struct enum interface form new this return if else switch
  case default for foreach in while do break continue defer true false null
  is as and or not when where typeof sizeof ref out`
  — `and or not` are **pattern** keywords only (`x is > 0 and < 10`), not
  expression operators. Contextual: `get set init value partial event`.
- **Integer literals**: `42`, `0x2A`, `0b101010`, `1_000_000`. Suffixes `u`,
  `L`, `UL`. An unsuffixed literal takes the type the context expects; with no
  context it is `int` if it fits, else `long`.
- **Float literals**: `1.5`, `1e9`, `1.5f` (`float`), `1.5d`/unsuffixed (`double`).
- **Strings**: `"…"` with `\n \t \\ \" \0 \u{1F600}` escapes;
  `$"…{expr}…{expr:format}…"` interpolates (`{{` `}}` for literal braces
  inside `$` strings only); `"""…"""` raw multi-line, leading indentation of
  the closing delimiter stripped; `$"""…"""` raw and interpolated.
- **Chars**: `'a'`, `'\n'` — a `char` is a Unicode scalar (`uint32` inside).
- Statements end with `;`. The printer (`kiln fmt`) is canonical: four-space
  indent, braces on their own line (C#'s style, as below), trailing commas in
  multi-line lists.

## 3. Program structure

```csharp
namespace Game.Server;

using Kiln.Db;
using static Kiln.Math;

public static class Program
{
    public static int Main(string[] args)
    {
        Console.WriteLine($"{args.Count} arguments");
        return 0;
    }
}
```

- One file-scoped `namespace` per file (block-scoped `namespace X { }` is
  accepted and printed file-scoped).
- **Top-level statements** are allowed in exactly one file of a project and
  become `Main`. `args` is in scope.
- A project is the set of `.kiln` files under the project root (or listed in
  `kiln.json`). Declaration order does not matter across or within files.
- `using X;` imports a namespace; `using static X.Y;` imports a type's
  static members; `using Alias = X.Y;` aliases. Standard library namespaces
  live under `Kiln.*`; a `Kiln.X` namespace pulls in the matching library
  (replacing 1.x `use x`).
- Visibility: `public`, `internal` (default for types — project-visible),
  `private` (default for members). No `protected` (no inheritance).

## 4. Types

### 4.1 Built-in value types

| K2 | Size | 1.x | Notes |
|---|---|---|---|
| `bool` | 1 byte (int slot at the command ABI) | `bool` | |
| `sbyte` `byte` | 8 | `byte` (c-record only) | |
| `short` `ushort` | 16 | `int16`/`word` (c-record only) | |
| `int` `uint` | 32 | `int` | |
| `long` `ulong` | 64 | `int64` | |
| `nint` `nuint` | pointer | `ptr` arithmetic | |
| `float` | 32 | c-record `float` | |
| `double` | 64 | `double` | |
| `char` | 32 | — | Unicode scalar |
| `string` | pointer | `text` | immutable UTF-8, `null`-free (empty is `""`) |
| `Bytes` | pointer | `bytes` | mutable byte buffer, 0-based offsets |
| `Ptr` | pointer | `ptr` | raw address for interop; `Ptr.Null` |

Conversions: widening numeric conversions are implicit (`int`→`long`,
`int`→`double`, `uint`→`ulong`); everything else is explicit `(T)x`, which
truncates/wraps. Checked conversion is `x.To<T>()` → `Result<T>`. Mixing
signed and unsigned of the same width without a cast is an error.

Integer overflow wraps (two's complement). Division by zero traps at runtime
with a message and the source position (as 1.x).

### 4.2 Reference types

- `class C { … }` — heap object, reference equality, mutable fields.
- `record R(int A, string B);` — positional record: heap object, **value
  equality** (`==` compares fields), `init`-only properties, `with`
  expressions (`r with { B = "x" }`), a generated `ToString`. A record may
  also have a body with methods. `record struct` is not in 2.0.
- `enum E { A, B = 4 }` — a distinct type over `int` (`enum E : byte` picks
  the width). No implicit conversion to or from integers; `(int)e`, `(E)n`.
  `[Flags]` enables `|` `&` on it.
- `interface I { int Area(); string Name { get; } }` — implemented by
  `class C : I` / `record R(...) : I`. No default method bodies in 2.0.
- `T[]` — fixed-length array; `List<T>` — growable list;
  `Dictionary<K, V>`; `HashSet<T>`. All compiler-known (Phase 4), nest freely
  (`List<List<int>>`), 1-based indexing: `list[1]` is the first element.
- `Func<A, B, R>` / `Action<A, B>` — function types. Lambdas, method groups
  and local functions convert to them (§8).
- `(int Slot, uint Template)` — tuples, value semantics, named or positional
  elements (`t.Slot`, `t.Item1`). Deconstruction `var (a, b) = t;`.

### 4.3 Absence: `T?`

`T?` is a `T` or `null`, for any `T`. `null` is assignable only to `T?`.

A `T?` is narrowed to `T` by flow analysis after `if (x != null)`,
`if (x is { } v)`, `if (x is T v)`, an early `return`/`break`/`continue` on
`x == null`, or `x!` (asserts, traps with position if null). Member access
through `x?.M`, fallback `x ?? y`, assignment `x ??= y`.

Reading a `T?` where a `T` is required is a compile error — not a warning.

### 4.4 Generics

```csharp
public static T Max<T>(T a, T b) where T : IComparable<T> =>
    a.CompareTo(b) >= 0 ? a : b;

public class Cache<K, V> where K : IEquatable<K> { … }
```

Type parameters on types and methods; constraints `where T : I1, I2`,
`where T : class`, `where T : new()`. Type arguments are inferred from
arguments where possible. **Monomorphised**: each instantiation is its own
code with a mangled symbol (`Max<int>` → `K2Max$i32`). No variance, no
higher-kinded types.

## 5. Declarations

```csharp
public class Inventory
{
    private readonly List<CharacterItem> items = new();   // field
    public const int MaxSlots = 48;                      // constant
    public int Count => items.Count;                     // expression-bodied property
    public string Owner { get; set; } = "";              // auto property
    public bool IsFull { get { return Count >= MaxSlots; } }

    public Inventory(string owner) { Owner = owner; }    // constructor

    public Result<CharacterItem> Take(int position) { … }
    public static Inventory Empty() => new("");
}
```

- Fields, `const` (compile-time value), `static readonly` (initialised once
  at startup, in dependency order; a cycle is an error).
- Properties: auto, expression-bodied, `get`/`set`/`init` accessors.
- Constructors; primary constructors on `record` and `class`
  (`class Point(int x, int y)`). No finalisers.
- Methods: instance or `static`; expression-bodied `=> expr`; default
  parameter values; named arguments; `params T[]`; overloads by parameter
  types (not by return type).
- `static class` holds only static members (the replacement for a 1.x
  module's free subs).
- Extension methods: `public static int WordCount(this string s)` in a
  static class, found through `using`.
- Local functions inside a method body.
- Operator overloading: `==`/`!=` on records is generated; user operators
  (`public static V operator +(V a, V b)`) are allowed on records and classes.
- `partial class` lets one type span files — the form (§11) uses it.

## 6. Statements

```csharp
var x = 1;                 // inferred, mutable
let y = 2;                 // inferred, immutable (C# lacks this; K2 keeps 1.x's `let`)
int z = 3;                 // explicit type
if (c) { … } else if (d) { … } else { … }
while (c) { … }
do { … } while (c);
for (var i = 1; i <= n; i++) { … }
foreach (var item in items) { … }
foreach (var (position, item) in items.Indexed()) { … }   // position is 1-based
foreach (var i in 1..10) { … }                            // inclusive
switch (x) { case 1: … break; case > 10: … break; default: … break; }
break; continue; return expr;
defer File.Close(h);       // runs when the enclosing block is left, reverse order
x += 1; x++; x ??= y;
```

- Braces are required for `if`/`while`/`for`/`foreach` bodies except a single
  statement on the same line — the printer adds them otherwise.
- `switch` statement `case` blocks do not fall through; `break` is optional
  and removed by the printer.
- `defer STMT;` / `defer { … }` — as 1.x: runs on every exit from the block
  it is written in (`return`, `break`, `continue`, falling off the end, `?`).
- `assert(cond, message)` — traps in debug builds, removed in `--release`.

## 7. Expressions

Precedence, high to low (C#'s):
primary (`x.y` `f()` `a[i]` `x?.y` `x!` `new` `typeof` `sizeof` postfix `++`)
→ unary (`+ - ! ~ ++x (T)x`) → range (`..` `..<`) → `switch` → `* / %`
→ `+ -` → `<< >>` → relational and type tests (`< > <= >= is as`) → `== !=`
→ `&` → `^` → `|` → `&&` → `||` → `??` → `?:` → assignment and lambda (`=>`).

- `?` postfix (Result propagation, §9) binds as primary.
- `switch` expression:
  `var name = weapon switch { Profession.Warrior => 1000, _ => 0 };`
  Exhaustive over an `enum` or a closed set; otherwise `_` is required.
- Patterns (in `is`, `switch`, `case`): constant, `null`, type `T v`,
  relational `> 0`, `and`/`or`/`not` combinators, property `{ Slot: 3 }`,
  positional `(1, _)`, discard `_`, `var v`.
- Object and collection initialisers: `new Item { Slot = 1 }`,
  `new List<int> { 1, 2, 3 }`, and collection expressions `[1, 2, 3]`
  (target-typed to `T[]`, `List<T>`, `HashSet<T>`), spread `[.. a, .. b]`.
- Target-typed `new()`.
- `nameof(x)`, `sizeof(T)` (compile-time), `default(T)` / `default`.
- String `+` concatenates `string` with `string` only; use interpolation to
  mix types.
- Indexing: `list[1]` first, `list[^1]` last (`^n` counts from the end,
  `^1` is last — consistent with 1-based). Slices take ranges:
  `list[2..4]` is elements 2, 3 and 4. An index out of range traps with
  position.

## 8. Lambdas and closures

```csharp
Func<int, int> twice = x => x * 2;
var adults = people.Where(p => p.Age >= 18).OrderBy(p => p.Name).ToList();
button.Click += (sender, e) => status.Text = "clicked";
```

- Parameter types inferred from the target `Func`/`Action`; explicit
  `(int x) => …` allowed. Statement bodies `x => { …; return y; }`.
- Captured variables are captured **by reference** (C#'s rule). A lambda
  that captures is lowered to a function taking an env pointer plus a
  collector-allocated env record; one that captures nothing is a plain
  function pointer with a null env.
- `static x => …` forbids capture.
- A method group (`Where(IsAdult)`) converts to a `Func` like a lambda.

## 9. Errors: `Result<T>`

```csharp
public static Result<Config> Load(string path)
{
    var text = File.ReadText(path)?;              // propagates the failure
    var cfg = Json.Parse<Config>(text)?;
    if (cfg.Port == 0) return Error("port is 0");
    return cfg;                                   // implicit Ok
}

var port = Load("app.json").Map(c => c.Port) ?? 8080;
switch (Load("app.json"))
{
    case Ok(var cfg): Run(cfg); break;
    case Err(var e):  Console.WriteLine($"no config: {e.Message}"); break;
}
```

- `Result<T>` is `Ok(T)` or `Err(Error)`. `Result` (no `T`) carries no value.
  `Error` has `Code` (`int`), `Message` (`string`), and `Position` (source
  location of the `Error(...)` or of the failing library call).
- `return value;` in a `Result<T>` method is `Ok`; `return Error(msg)` /
  `return Error(code, msg)` is `Err`.
- `expr?` — if `expr` is `Err`, return it from the enclosing method (whose
  return type must be `Result<_>`; `Main` may return `Result` or
  `Result<int>`: an `Err` prints the message and exits 1). Otherwise the
  value. `defer`s run.
- `expr ?? fallback` — the value, or `fallback` on `Err`.
- Members: `IsOk`, `IsErr`, `Value` (flow-narrowed like `T?`), `Error`,
  `Map`, `Then` (bind), `OrElse`.
- Ignoring a `Result` from a call statement is a **compile error**; discard
  explicitly with `Save().Ignore();` (reads as intent) or `_ = Save();` (C#).
  Both are accepted; `Ignore()` is the idiom in fire-and-forget GUI handlers
  where `_ =` on every line is noise. *(Provisional — the `LuaHook` sketch used
  a `?? Skip` spelling that this replaces.)*
- **Lowering**: a library command that can fail returns its value and writes
  the error slot (1.x ABI, unchanged). The K2 wrapper surface types it as
  `Result<T>`; `?`/`??`/`switch` directly on a call lower to an error-slot test,
  as 1.x `check`/`otherwise` do. Only a `Result` that is *stored, passed or
  returned* is materialised as a two-part value `{T value; Error* err}`.

## 10. Standard library surface

Library C implementations and the slot ABI are unchanged. Phase 4 adds a
namespace/member mapping to libinfo so the same command appears as a K2 name.

| 1.x | K2 |
|---|---|
| `use file` / `file_read_text(p)` | `using Kiln.IO;` / `File.ReadText(p)` → `Result<string>` |
| `length(s)` | `s.Length` |
| `find(s, x)` | `s.Find(x)` → `int?` (1-based) |
| `mid(s, i, n)` | `s.Substring(i, n)` (1-based `i`) |
| `int_to_text(n)` | `n.ToString()` / `$"{n}"` |
| `text_to_int(s)` | `int.Parse(s)` → `Result<int>` |
| `print_text(s)` | `Console.WriteLine(s)` |
| `db_open(...)` | `Db.Open(...)` → `Result<Db>` |
| `db_exec(db, sql, …)` | `db.Execute(sql, args…)` → `Result<int>` |
| `last_error_code()` | gone from user code — `Result` carries it |
| `time_now()` | `DateTime.Now` |
| `json_parse(s)` | `Json.Parse(s)` → `Result<JsonValue>`; `Json.Parse<T>(s)` via `[Json]` (§12) |

The full map is generated into `design/k2/stdlib-map.md` in Phase 4 from
libinfo; this table fixes the conventions: **type-like prefixes become static
classes, first-parameter-is-the-object commands become instance members, can-fail
commands return `Result`, not-found returns `T?`.**

## 11. Forms and events

```csharp
namespace Counter;

using Kiln.UI;

public partial form MainWindow
{
    // ── designer-owned: `kiln edit` rewrites this block; hand edits survive
    //    only if they are property assignments it can read back.
    Title = "Counter";
    Width = 320; Height = 200;

    Label count { Text = "0"; Left = 40; Top = 40; Width = 200; Height = 24; }
    Button add  { Text = "Add"; Left = 40; Top = 90; Width = 120; Height = 32;
                  Click += OnAdd; }
}

public partial form MainWindow
{
    private int n;

    private void OnAdd(Button sender, ClickEventArgs e)
    {
        n++;
        count.Text = $"{n}";
    }
}
```

- A `form` is a `partial class`-like type with a designer block: property
  assignments and child component declarations only. Studio reads and writes
  it through `kiln edit` (tree-based); it is never spliced as text.
- Components are fields of the form (`count`, `add`).
- `Event += handler` — a method group or lambda. `-=` unsubscribes (method
  groups only; a lambda cannot be removed, as C#). An event may have several
  handlers; they run in subscription order.
- Handler signature: `(TSender sender, TArgs e)`; either may be omitted when
  unused (`void OnAdd()`) — the compiler adapts.
- Non-visual components (`Timer`, `HttpServer`, …) declare at form or
  namespace level with the same block syntax.
- `Application.Run<MainWindow>()` or top-level `Run<MainWindow>();` starts a
  GUI program; a project with a `form` and no `Main` gets this generated.
- **ABI v5**: a handler is `{fn, env}`, `fn(env, sender, args)`; the runtime
  keeps handler lists in a collector-rooted registry.

## 12. Attributes (compile time)

Attributes are compiler plugins that run before lowering and generate
ordinary K2 declarations. No attribute is visible at runtime.

| Attribute | Generates |
|---|---|
| `[Table("character_items")]` on a record, `[Column("x")]`, `[Key]`, `[Auto]` | `Insert`, `InsertMany`, `Update`, `Delete`, `Query<T>` row mapping over `Kiln.Db` |
| `db.Query<T>(x => x.CharacterId == id && x.Slot < 10)` | parameterised SQL at compile time; supports comparisons, `&&` `\|\|` `!`, member access, captured values, `.Contains` on strings/lists. Anything else is a compile error naming the unsupported node |
| `[Packed]` on a record, `[Offset(n)]` (0-based) | `static Result<T> Read(Bytes, int offset)`, `void Write(Bytes, int offset)`, `const int Size` |
| `[Json]` | `Json.Parse<T>` / `ToJson` members |
| `[CLayout]` on a `struct` | C layout for interop (`struct` exists only with this attribute in 2.0) |
| `[Dll("user32", Convention = StdCall)]` on an `extern static` method | a dll import (replaces 1.x `dll`) |
| `[Flags]` on an enum | bitwise operators on it |

## 13. Interop

```csharp
[CLayout]
public struct Rect { public int Left, Top, Right, Bottom; }

[CLayout]
public struct ProcessEntry32
{
    public uint Size;
    // …
    [Fixed(260)] public byte[] ExeFile;   // char szExeFile[MAX_PATH], inline
}

public static partial class User32
{
    [Dll("user32", Convention = CallConv.StdCall)]
    public static extern bool GetWindowRect(Ptr hwnd, ref Rect rect);
}
```

- `ref`/`out` on extern parameters pass an address. `Ptr` arithmetic:
  `p + offset` (0-based `nint`). Callbacks: a `static` lambda or method group
  converts to a C function pointer for an extern parameter of a `delegate
  unmanaged` type.
- **`[Fixed(n)]`** on a `[CLayout]` array field lays the array *inline* (`n`
  elements, not a pointer) — the fixed-size buffers real C structs are full of
  (`char name[260]`). Read it with `Bytes.ReadCString(field)` / index it;
  `sizeof(T)` accounts for it.
- **`Ptr` sentinels**: `Ptr.Null` is address 0; `Ptr.Invalid` is all-ones
  (Win32 `INVALID_HANDLE_VALUE`, `-1`), since a handle API returns that, not
  null. `Handle.Invalid` is an alias. `p.IsNull` tests 0.
- **`Bytes` interop helpers**: `Bytes.OfCString(s)` (a NUL-terminated copy of a
  `string`), `Bytes.ReadCString(bytes)` (up to the first NUL), `bytes.Address`
  (a `Ptr` to the buffer, for `WriteProcessMemory` and friends).

## 14. Settled open items

- **`defer`** stays; C#'s `using` statement is not added (nothing is
  `IDisposable` without destructors — `defer` says the same thing plainly).
- **Fallback** is `??` for both `T?` and `Result<T>`; 1.x `otherwise` is gone.
- **Interface dispatch**: constrained generics monomorphise; a value of
  interface type (`List<IShape>`) is `{object, vtable}`.
- **Equality**: `record` by value, `class` by reference, `string` by value.

## 15. Not in 2.0

Inheritance and `abstract`/`virtual`; exceptions; `async`/`await` (threads
and the event loop stay as 1.x provides them, surfaced as `Thread.Start(() =>
…)`, `Mutex`); reflection; `dynamic`; LINQ query syntax (`from x in …`);
`record struct`; user-defined generic variance; `unsafe` blocks beyond `Ptr`;
default interface methods; source generators beyond the built-in attributes.

## 16. Worked example

A game server's starter kit — the port that motivated K2 — written to this spec:

```csharp
namespace Game.Server;

using Kiln.Db;

public enum Profession : byte { Warrior, Champion, Mage, Priest }

[Table("character_items")]
public record CharacterItem(
    [Key, Auto] ulong Id,
    uint CharacterId,
    ushort Slot,
    uint TemplateId,
    uint ClassValue,
    uint Flags);

/// What a new character is given. Copied from a live character, not invented:
/// every template id resolves through the client's own item tables.
public static class StarterInventory
{
    private const uint EggFlags        = 0x0101_0107;
    private const uint PlainFlags      = 0x0101_0101;
    private const uint ConsumableFlags = 0x0201_0101;
    private const uint SackFlags       = 0x0301_0101;

    private static readonly (ushort Slot, uint Template, uint ClassValue, uint Flags)[] Kit =
    [
        (0,  10157, 1747, EggFlags),          // Ghost Egg
        (1,  10154, 1747, EggFlags),          // Easter Bunny Egg
        (2,   4199, 1459, SackFlags),         // Giant Gold Sack
        (3,   4103, 2207, PlainFlags),        // Parcel Stone
        (4,   4535, 2207, ConsumableFlags),   // Enduring Strong Exp Potion
        (5,   4538, 2207, ConsumableFlags),   // Enduring Strong TP Potion
        (6,   4533, 2207, ConsumableFlags),   // Durable Super Pet Exp Potion
        (7,   4469, 2207, ConsumableFlags),   // Golden Leaf
        (8,   3922, 2207, PlainFlags),        // Newbie Training Bag (Lv10)
        (9,   4151, 2207, ConsumableFlags),   // Medium Money Bag
        (10,  3875, 2207, PlainFlags),        // Lost Book
        (11,  3875, 2207, PlainFlags),        // Lost Book
        (12,  3875, 2207, PlainFlags),        // Lost Book
    ];

    public static List<CharacterItem> KitFor(uint characterId) =>
        Kit.Select(k => new CharacterItem(0, characterId, k.Slot, k.Template, k.ClassValue, k.Flags))
           .ToList();

    public static uint WeaponFor(Profession p) => p switch
    {
        Profession.Warrior  => 1000,   // Short Sword
        Profession.Champion => 1400,   // Spear
        Profession.Mage     => 1700,   // Short Wand
        Profession.Priest   => 1800,   // Long Staff
        _                   => 0,
    };

    public static Result Give(Db db, uint characterId)
    {
        var tx = db.Begin()?;
        defer tx.RollbackUnlessCommitted();
        db.InsertMany(KitFor(characterId))?;
        return tx.Commit();
    }
}
```
