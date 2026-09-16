# Kiln 2

Kiln 2 is Kiln's language: C#-shaped, compiled to one native binary, with the
form you draw still the source. It is not source-compatible with Kiln 1.x —
`kiln migrate` converts a 1.x program, and 1.x still builds, with a note. Build
and run one with:

```sh
kiln run hello.kiln
```

Everything on this page compiles; the samples are checked on every build.

## Hello

```k2
namespace Hello;

Console.WriteLine("Hello from Kiln 2!");
```

Top-level statements become `Main`. A program that prefers the long form writes
it out:

```k2
namespace Hello2;

public static class Program
{
    public static void Main()
    {
        Console.WriteLine("Hello from Kiln 2!");
    }
}
```

## Values

Types are C#'s, and positions are still counted from **1** — that has not
changed, and it is deliberate. Offsets into bytes are counted from 0.

```k2
namespace Values;

public static class P
{
    public static void Main()
    {
        var count = 10;          // int, inferred
        let limit = 100;         // immutable
        long big = 5_000_000_000;
        uint mask = 0xFF00_FF00;
        double ratio = 0.5;
        bool ready = true;
        string name = "kiln";

        Console.WriteLine($"{name}: {count} of {limit}, {ratio}");
        Console.WriteLine($"{big} {mask} {ready}");
    }
}
```

Unsigned arithmetic is genuinely unsigned: `mask >> 4` shifts zeroes in, and
`a / b` on `uint` divides without sign.

## Choosing and repeating

```k2
namespace Flow;

public static class P
{
    public static string Size(int n) => n switch
    {
        <= 0 => "none",
        < 10 => "small",
        _    => "large",
    };

    public static void Main()
    {
        foreach (var i in 1..5)          // inclusive, 1-based
        {
            if (i % 2 == 0)
                continue;
            Console.WriteLine($"{i} is {Size(i)}");
        }

        var n = 0;
        while (n < 3)
            n = n + 1;
        Console.WriteLine($"{n}");
    }
}
```

## Records, classes and interfaces

A `record` compares by value and is written positionally; a `class` holds
mutable state. There is no inheritance — an interface is how two types share a
shape.

```k2
namespace Shapes;

public interface IShape
{
    int Area();
    string Name();
}

public record Rect(int W, int H) : IShape
{
    public int Area() => W * H;
    public string Name() => "rect";
}

public record Square(int Side) : IShape
{
    public int Area() => Side * Side;
    public string Name() => "square";
}

public static class P
{
    public static string Describe(IShape s) => $"{s.Name()} of {s.Area()}";

    public static void Main()
    {
        Console.WriteLine(Describe(new Rect(3, 4)));
        Console.WriteLine(Describe(new Square(5)));
    }
}
```

## Collections and lambdas

`List<T>`, `Dictionary<K,V>` and `HashSet<T>` are built in. A lambda is a value.

```k2
namespace Collections;

public static class P
{
    public static void Main()
    {
        var xs = new List<int>();
        foreach (var i in 1..10)
            xs.Add(i);

        var evens = xs.Where(x => x % 2 == 0);
        var labels = evens.Select(x => $"n{x}");
        Console.WriteLine($"{evens.Count} even, first {xs[1]}");
        foreach (var l in labels)
            Console.Write($"{l} ");
        Console.WriteLine("");

        var ages = new Dictionary<string, int>();
        ages["ada"] = 36;
        ages["grace"] = 45;
        Console.WriteLine($"{ages.Get("ada") ?? 0} {ages.ContainsKey("nobody")}");
        foreach (var (who, age) in ages)       // in the order the keys were added
            Console.WriteLine($"{who} is {age}");

        var seen = new HashSet<int>();
        seen.Add(1);
        seen.Add(1);
        Console.WriteLine($"{seen.Count}");
    }
}
```

A closure captures **by reference**, so it sees later writes:

```k2
namespace Capture;

public static class P
{
    public static void Main()
    {
        var total = 0;
        Action<int> add = n => { total = total + n; };
        foreach (var i in 1..4)
            add(i);
        Console.WriteLine($"{total}");
    }
}
```

A loop variable is the exception worth knowing: each turn of a `foreach` binds
its own, so a closure made in one turn holds that turn's value rather than the
last one. Writing to it inside the same turn is still seen by that turn's
closure — it is per-iteration, not a copy.

```k2
namespace PerTurn;

public static class P
{
    public static void Main()
    {
        var fs = new List<Action>();
        foreach (var i in 1..3)
            fs.Add(() => { Console.Write($"{i} "); });
        foreach (var f in fs)
            f();                                // 1 2 3, not 3 3 3
        Console.WriteLine("");
    }
}
```

## Generics

Methods and types both take type parameters, and each set of type arguments is
compiled separately — there is no boxing and no runtime type information.

```k2
namespace Generics;

public interface INamed { string Name(); }

public record Tag(string Label) : INamed
{
    public string Name() => Label;
}

public class Box<T>
{
    T value;
    public Box(T v) { value = v; }
    public T Get() => value;
}

public static class P
{
    public static T Max<T>(T a, T b) => a >= b ? a : b;
    public static string Show<T>(T x) where T : INamed => $"[{x.Name()}]";

    public static void Main()
    {
        Console.WriteLine($"{Max(3, 9)} {Max(2.5, 1.5)}");
        Console.WriteLine($"{new Box<int>(41).Get()}");
        Console.WriteLine(Show(new Tag("core")));
    }
}
```

A constraint is checked when the type argument is chosen: passing a type that
does not implement `INamed` to `Show` is a compile error naming both.

## When something fails

There are no exceptions. A failure is a value: `Result<T>` for an operation
that can fail, `T?` for a value that may not be there.

```k2
namespace Failing;

public static class P
{
    public static Result<int> Half(int n)
    {
        if (n % 2 != 0) return Error("not even");
        return n / 2;
    }

    // `?` hands the failure back to the caller.
    public static Result<int> Quarter(int n)
    {
        var half = Half(n)?;
        return Half(half)?;
    }

    public static int? Digits(string s) => s.Length > 0 ? s.Length : null;

    public static void Main()
    {
        Console.WriteLine($"{Half(84) ?? -1}");     // 42
        Console.WriteLine($"{Half(7) ?? -1}");      // -1
        var q = Quarter(6);
        Console.WriteLine($"{q.IsOk} {q.Error}");

        var d = Digits("kiln");
        if (d != null)
            Console.WriteLine($"{d}");              // narrowed to int
    }
}
```

`defer` runs when its block is left, however it is left:

```k2
namespace Cleanup;

public static class P
{
    public static void Main()
    {
        Console.WriteLine("open");
        defer Console.WriteLine("close");
        Console.WriteLine("work");
    }
}
```

## Forms

A form is the window, and the block that declares it is the designer's half.
Your code is the other half of the same `partial form`. Build a form program the
same way; it links the UI library automatically.

```k2
namespace Counter;

public partial form MainWindow
{
    Title = "Counter";
    Width = 320;
    Height = 200;

    Label count { Text = "0"; Left = 40; Top = 40; Width = 220; Height = 28; }
    Button add  { Text = "Add one"; Left = 40; Top = 100; Width = 140; Height = 34;
                  Click += OnAdd; }
}

public partial form MainWindow
{
    int n;

    void OnAdd()
    {
        n = n + 1;
        count.Text = $"{n}";
    }
}
```

An event takes a method by name or a lambda written where it is wired:

```k2
namespace Inline;

public partial form MainWindow
{
    Title = "Inline";
    Label count { Text = "0"; }
    Button add { Text = "Add"; Click += () => { n = n + 1; count.Text = $"{n}"; }; }
}

public partial form MainWindow { int n; }
```

Studio edits the designer's half through `kiln edit`, never as text, so your
comments and your code survive a change made in the designer.

## Reaching C, and bytes

```k2
namespace Interop;

[Packed]
public record Header(ushort Opcode, ushort Length, uint Sequence);

public static class Libc
{
    [Dll("c")]
    public static extern int abs(int n);
}

public static class P
{
    public static void Main()
    {
        Console.WriteLine($"{Libc.abs(-42)}");

        var buf = Bytes.Alloc(64);
        var h = new Header(0x1234, 40, 7);
        h.Write(buf, 0);
        var back = Header.Read(buf, 0);
        Console.WriteLine($"{Header.Size} {back.Opcode} {back.Sequence}");
    }
}
```

`[Packed]` lays a record out with no padding and gives it `Size`, `Read` and
`Write` — an exact wire format. `[Dll]` calls a C function directly; add
`Convention = CallConv.StdCall` for Win32.

## Talking to a database

`[Table]` turns a record into a table, and a query predicate is translated to
SQL **while compiling** — no reflection reaches the binary.

```k2
namespace Data;

using Kiln.Db;

[Table("items")]
public record Item(
    [Auto] long Id,
    int OwnerId,
    [Column("name")] string Name);

public static class P
{
    public static void Main()
    {
        var h = Db.Open("sqlite::memory:");
        Db.Exec(h, "create table items (id integer primary key, owner_id int, name text)", []);

        Item.Insert(h, new Item(0, 7, "sword"));
        Item.Insert(h, new Item(0, 9, "hat"));

        var wanted = 7;
        foreach (var it in Item.Select(h, i => i.OwnerId == wanted))
            Console.WriteLine($"{it.Id} {it.Name}");

        // The statement itself, if you want to see or log it.
        Console.WriteLine(Item.InsertSql());
    }
}
```

`Insert` and `Select` run the statement; `InsertSql` and `SelectSql` answer
with its text. Both build the same SQL while compiling, so nothing about the
row's shape reaches the binary — which column is read with which `db_` reader
is decided here, from the record's declared types.

Anything a database cannot evaluate is a compile error naming it, so a query
never half-runs in the wrong place. A captured value becomes a `?` parameter
rather than being pasted into the statement: a value that looks like SQL is
data, and stays data.

## The standard library

`using Kiln.File;` loads that library. A command written `file_read_text` is
reached as `File.ReadText`, and one whose first argument is the value it works
on reads as a member: `length(s)` is `s.Length`.

A command's parameters are checked against what you pass, and a mismatch is a
compile error naming the command and the position. Where a command takes or
answers with a list, a `List<T>` converts at the boundary — so `List<T>` is the
only list a program sees.

```k2
namespace Std;

using Kiln.Text;

public static class P
{
    public static void Main()
    {
        var s = "kiln two";
        Console.WriteLine($"{s.Length}");
        Console.WriteLine(s.Uppercase());
    }
}
```

A command that can fail writes the error slot. Where a `Result` is expected — or
under `??` — that becomes a value:

```k2
namespace StdFail;

using Kiln.File;

public static class P
{
    public static void Main()
    {
        Console.WriteLine(File.ReadText("/does/not/exist") ?? "(missing)");
    }
}
```

## Tools

| Command | What it does |
| --- | --- |
| `kiln build <file>` | build a program into one native binary; `--release` optimises it |
| `kiln run <file>` | build it and run it |
| `kiln k2 <file> --emit-ir` | print the LLVM the build hands clang |
| `kiln fmt <file>` | print it in the canonical spelling; `--check` reports without writing |
| `kiln migrate <file>` | convert a Kiln 1.x program to Kiln 2 source |
| `kiln edit <file> …` | change a form's designer block through the tree |

A built binary carries debug information, so `gdb` stops on a line, prints
locals by name, and shows a record as its fields.

## What is not here yet

Kiln 2 is not finished, and it is worth knowing where the edges are:

- It builds for the machine it runs on and cross-builds for Windows, 64-bit and
  32-bit (`--os windows --arch x86`).
- Non-interface generic constraints (`where T : class`, `new()`) parse and are
  ignored.
- Studio opens, edits and saves a Kiln 2 form; its code pane highlights Kiln 2
  from its own tokenizer rather than from the language server.
- `kiln fmt` puts a comment written at the end of a line above the next
  construct, and normalises blank lines between constructs rather than keeping
  them.

`design/k2/status.md` in the repository tracks this precisely.
