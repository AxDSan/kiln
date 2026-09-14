# Moving to Kiln 2

Kiln 2 is not source-compatible with 1.x. Nothing you have written stops
working — 1.x still builds with `kiln build` and still ships — but a program
is one language or the other, decided by its first line, and moving it across
is a deliberate step.

`kiln migrate` does the mechanical part:

```sh
kiln migrate old.kiln > new.kiln
kiln k2 new.kiln --run
```

It converts syntax, naming and structure, and leaves a `// TODO(migrate):`
comment above anything it will not decide for you. What it writes compiles; if
it cannot produce something that compiles, it says so instead of guessing.

## What changes

| Kiln 1.x | Kiln 2 |
| --- | --- |
| `module m` | `namespace M;` and a `static class` |
| `sub name` … `end` | `void Name()` with braces |
| `use text` | `using Kiln.Text;` |
| `let x: int = 1` | `int x = 1`, or `var x = 1` |
| `call f(x)` | `f(x)` |
| `and` `or` `not` | `&&` `\|\|` `!` |
| `band` `bor` `shl` | `&` `\|` `<<` |
| `concat(a, b)` | `$"{a}{b}"` |
| `snake_case` names | `PascalCase` types and methods, `camelCase` locals |
| `SCREAMING_CASE` constants | `PascalCase` |
| `check` and the error slot | `Result<T>`, `?` and `??` |
| `#` comments | `//` comments |

**Positions are not rebased.** Kiln counts from 1 and Kiln 2 counts from 1;
`substr(s, 1, 3)` and `s.Substr(1, 3)` mean the same thing. Byte offsets stay
0-based in both. This is the one thing a migration usually gets wrong, and here
there is nothing to get wrong.

## A worked example

A 1.x program:

```kiln
module greeter
use text

const TIMES = 3

sub main
  let name: text = "world"
  var i: int = 0
  while i < TIMES
    call print_text(concat("hello, ", name))
    i = i + 1
  end
end
```

`kiln migrate` turns it into this, which compiles and prints the same three
lines:

```k2
// Migrated from Kiln 1.x by `kiln migrate`. Positions are still 1-based.

namespace Greeter;

using Kiln.Text;

public static class Greeter
{
    public const int Times = 3;

    public static void Main()
    {
        string name = "world";
        int i = 0;
        while (i < Times)
        {
            Console.WriteLine($"hello, {name}");
            i = i + 1;
        }
    }
}
```

Note what happened to `concat`: a chain of concatenations becomes one
interpolated string, because that is what it was for.

## What it will not decide

Some 1.x idioms have more than one reasonable Kiln 2 shape, and picking one for
you would be a worse outcome than telling you. These get a comment and are left
as they were:

- **The error slot.** `check f(x)` and `otherwise` become `Result<T>` only where
  the intent is unambiguous. Where a program tests `last_error()` by hand, the
  shape it wants might be `?`, `??`, or a `T?` — that is a design decision.
- **An indirect call.** 1.x calls a subroutine by name through a string; Kiln 2
  has real function values, and which one a given call should become depends on
  what it is for.
- **A `match`.** Kiln 2 has `switch` expressions with patterns, and the mapping
  is rarely one-to-one.

Search the output for `TODO(migrate)` before you build.

## Forms move too

A form migrates like anything else, and Studio opens the result:

```k2
namespace Counter;

public partial form MainWindow
{
    Title = "Counter";
    Width = 320;

    Label count
    {
        Text = "0";
        Left = 40;
        Top = 40;
    }

    Button add
    {
        Text = "Add one";
        Left = 40;
        Top = 100;
        Click += OnAdd;
    }
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

The designer's half and yours are two `partial form` blocks in one file.
Studio reads the file through `kiln inspect` and writes it through
`kiln edit`, never as text — so your comments and your code survive a change
made by dragging something.

## Doing it gradually

You do not have to move everything at once, because you do not have to move
anything at all yet. A sensible order:

1. Migrate a small program and run it. The output is the best documentation of
   what the two languages have in common.
2. Migrate a form and open it in Studio, to see that the RAD loop is the same
   loop.
3. Leave anything with a `TODO(migrate)` until you have decided what it should
   become — the 1.x version still builds in the meantime.

`design/k2/status.md` in the repository records exactly what is built and what
is not, so you can check before relying on something.
