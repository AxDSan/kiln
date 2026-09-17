# Components

A component is a thing with properties you set and events you bind to
methods. A button is one; so is a timer, which has no rectangle at all.
Every component and every property is listed in the generated
[reference](./reference-components.md); this page is what they are for, and
what their events hand a handler.

## Visual and non-visual

A component is one of two kinds, and the toolchain tells you which:
`kiln commands` prints `kind: button visual` and `kind: timer nonvisual`.

A **visual** component draws a rectangle, so it lives inside a form and has
`Left`, `Top`, `Width` and `Height`. A **non-visual** one has nothing to draw,
so it is declared beside the form — at the top level of the file, in the same
block shape.

```k2
namespace Countdown;

Timer tickSource
{
    Interval = 500;
    Tick += OnTick;
}

public static class P
{
    static int remaining = 3;

    public static void Main()
    {
        Console.WriteLine("3...");
    }

    static void OnTick()
    {
        remaining = remaining - 1;
        if (remaining <= 0)
        {
            Console.WriteLine("Liftoff.");
            Quit();
        }
        else
        {
            Console.WriteLine($"{remaining}...");
        }
    }
}
```

That split is why a console program can wait. A `Timer` is part of the core
runtime, not of `Kiln.Ui`, and a program that declares one stays in the
runtime's event loop after `Main` returns, for as long as any source is live —
it ends when a handler calls `Quit()`. The same is true of an `Httpserver`,
which is a program's whole reason to keep running. See
[Networking](./networking.md).

The non-visual components that exist:

| Component | From | Does |
| --- | --- | --- |
| `Timer` | core | fires `Tick` every `Interval` milliseconds while `Enabled` |
| `Action` | `Kiln.Ui` | one command — caption, shortcut, enabled — offered by several controls |
| `Datasource` | `Kiln.Ui` | rows of text that one or more grids show |
| `Httpserver` | `Kiln.Net` | accepts HTTP on `Port`, fires `Request` for each |

In Studio these sit in the **tray** under the canvas rather than on it —
select one there to edit its properties and wire its events exactly as you
would a button's. [The IDE](./studio.md) describes the tray.

## Events that hand something over

An event handler is an ordinary method, and some events give it a value:

| Component | Event | Hands the handler |
| --- | --- | --- |
| `Timer` | `Tick` | `(int n)` — which tick this is, counting from 1 |
| `Grid` | `Select` | `(int row)` — the row now selected |
| `Grid` | `Activate` | `(int row)` — the row double-clicked, or Enter on the selection |
| `Button` | `Click` | nothing |
| `Checkbox`, `Radiobutton`, `Slider`, `Spinner`, `Editbox`, `Memo`, `Combobox`, `Listbox` | `Change` | nothing |
| `Action` | `Execute` | nothing |
| `Httpserver` | `Request` | nothing — the handler asks `Net.Request()` |
| `form` | `Load` | nothing — it runs once, after `Main` and before the first frame is drawn |

A handler takes exactly what the event hands it, or nothing at all, and
returns `void`. Both shapes are wired the same way, so a handler that has no
use for the value simply does not name it. A lambda works as well as a method:

```k2
namespace TwoHandlers;

Timer counted
{
    Interval = 20;
    Tick += OnCounted;
}

Timer plain
{
    Interval = 20;
    Tick += OnPlain;
}

public static class P
{
    static int rounds = 0;

    static void OnCounted(int n)
    {
        Console.WriteLine($"counted tick {n}");
    }

    static void OnPlain()
    {
        rounds = rounds + 1;
        if (rounds >= 3)
            Quit();
    }

    public static void Main()
    {
        Console.WriteLine("Main returned");
    }
}
```

The event parameters are not in `kiln commands` output yet, so the generated
reference lists an event's name without what it hands over; the table above is
kept by hand against `runtime/core_libinfo.c` and the libraries' `_libinfo.c`
files.

## Two ways to name a component

A component's properties are read and written through its identifier —
`table.Selected`, `saveAction.Enabled`. But a component identifier never
reaches the built binary, so anything that refers to a component *from a
string* — a command, or another component's property — uses the component's
`Name` property instead:

```
Grid table { Name = "table"; Bind = "people"; }
```

`table.Selected` is the identifier; `GridCell("table", row, 1)` is the name;
`Bind = "people"` names a datasource. Set `Name` on any component you will
address from a command, and it is easiest to make it the identifier.

## Grid and datasource

A `Grid` never holds its rows itself. It is bound by name to a `Datasource`,
shows whatever that holds, and every grid bound to the same datasource shows
the same rows. Rows are one string: a newline between rows, a tab between cells,
because a property value in the designer's half must be a literal.

```k2
namespace People;

using Kiln.Ui;

Datasource people
{
    Name = "people";
    Columns = "Name\tCity\tAge";
    Rows = "Ada\tLondon\t36\nGrace\tArlington\t45";
}

public partial form Win
{
    Title = "People";
    Width = 520;
    Height = 300;

    Grid table
    {
        Name = "table";
        Bind = "people";
        Left = 20;
        Top = 20;
        Width = 480;
        Height = 200;
        Select += OnSelect;
    }

    Label status
    {
        Text = "Pick a row.";
        Left = 20;
        Top = 240;
        Width = 480;
    }
}

public partial form Win
{
    void Main()
    {
        DatasourceAddRow("people", "Dennis\tNew York\t70");
    }

    void OnSelect(int row)
    {
        status.Text = $"Selected {GridCell("table", row, 1)}";
    }
}
```

The commands are the way around building that string by hand:
`DatasourceAddRow` and `GridAddRow` append a row, `GridCell` and `GridSetCell`
read and write one cell, `GridRowCount` counts, and every row and column counts
from 1. `Selected` is the current row, `0` for none, and can be assigned to move
the selection. Rows added in `Main` are on screen from the first frame.

## Actions

An `Action` is one command shared by several controls. The caption, the
shortcut, whether it can be invoked, and the code behind it live in the action
and nowhere else, and a button offers it by name:

```k2
namespace SaveTwice;

using Kiln.Ui;

Action saveAction
{
    Name = "save";
    Text = "Save";
    Shortcut = "ctrl+s";
    Execute += OnSave;
}

public partial form Win
{
    Title = "Actions";
    Width = 320;
    Height = 120;

    Button toolbarSave
    {
        Action = "save";
        Left = 20;
        Top = 20;
        Width = 130;
        Height = 34;
    }

    Button menuSave
    {
        Action = "save";
        Left = 170;
        Top = 20;
        Width = 130;
        Height = 34;
    }
}

public partial form Win
{
    int saves = 0;

    void OnSave()
    {
        saves = saves + 1;
        saveAction.Text = $"Saved {saves}";
    }
}
```

Both buttons show the action's caption, both fire it, and
`saveAction.Enabled = false` greys both — neither button knows the other exists.
The shortcut works whichever control has focus.

## Accessibility

Every component carries an accessibility role and name, and a running program
publishes a live accessibility tree that assistive technology reads. This is
part of the component model rather than something added to each control, so
it is true of anything you build without extra work.
