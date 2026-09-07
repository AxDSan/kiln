# Components

A component is a thing with properties you set and events you bind to
subroutines. A button is one; so is a timer, which has no rectangle at all.
Every component and every property is listed in the generated
[reference](./reference-components.md); this page is what they are for, and
what their events hand a handler.

## Visual and non-visual

A component is one of two kinds, and the toolchain tells you which:
`kiln commands` prints `kind: button visual` and `kind: timer nonvisual`.

A **visual** component draws a rectangle, so it lives inside a form and has
`left`, `top`, `width` and `height`. A **non-visual** one has nothing to draw,
so it is declared beside the form — at module level, in the same block shape —
and the compiler refuses one placed inside a form: `timer is not a visual
component — declare it at module level, outside the form`.

```
module countdown
target console

var remaining: int = 3

timer tick_source
  interval = 500
  on tick: on_tick
end

sub main
  call print_text("3...")
end

sub on_tick
  remaining -= 1
  if remaining <= 0
    call print_text("Liftoff.")
    call quit()
  else
    call print_text("{remaining}...")
  end
end
```

That split is why a console program can wait. A `timer` is part of the core
runtime, not of `use ui`, and a program that declares one stays in the
runtime's event loop after `main` returns, for as long as any source is live —
it ends when a handler calls `quit()`. The same is true of an `httpserver`,
which is a program's whole reason to keep running. See
[Networking](./networking.md).

The non-visual components that exist:

| Component | From | Does |
| --- | --- | --- |
| `timer` | core | fires `tick` every `interval` milliseconds while `enabled` |
| `action` | `ui` | one command — caption, shortcut, enabled — offered by several controls |
| `datasource` | `ui` | rows of text that one or more grids show |
| `httpserver` | `net` | accepts HTTP on `port`, fires `request` for each |

In Studio these sit in the **tray** under the canvas rather than on it —
select one there to edit its properties and wire its events exactly as you
would a button's. [The IDE](./studio.md) describes the tray.

## Events that hand something over

An event handler is an ordinary subroutine, and some events give it a value:

| Component | Event | Hands the handler |
| --- | --- | --- |
| `timer` | `tick` | `(n: int)` — which tick this is, counting from 1 |
| `grid` | `select` | `(row: int)` — the row now selected |
| `grid` | `activate` | `(row: int)` — the row double-clicked, or Enter on the selection |
| `button` | `click` | nothing |
| `checkbox`, `radiobutton`, `slider`, `spinner`, `editbox`, `memo`, `combobox`, `listbox` | `change` | nothing |
| `action` | `execute` | nothing |
| `httpserver` | `request` | nothing — the handler asks `net_request()` |
| `form` | `load` | nothing — it runs once, after `main` and before the first frame is drawn |

A handler takes exactly what the event hands it, or nothing at all, and
returns nothing. Both shapes are wired the same way, so a handler that has no
use for the value simply does not name it:

```
module two_handlers
target console

timer counted
  interval = 20
  on tick: on_counted
end

timer plain
  interval = 20
  on tick: on_plain
end

var rounds: int = 0

sub on_counted(n: int)
  call print_text("counted tick {n}")
end

sub on_plain
  rounds += 1
  call quit() if rounds >= 3
end

sub main
  call print_text("main returned")
end
```

Get the shape wrong and the compiler shows the header to paste:

```text
event `tick` hands a handler (int), but `on_tick` takes (text) — take exactly
those, or none: `sub on_tick(n: int)`
```

The event parameters are not in `kiln commands` output yet, so the
generated reference lists an event's name without what it hands over; the
table above is kept by hand against `runtime/core_libinfo.c` and the
libraries' `_libinfo.c` files.

## Two ways to name a component

A component's properties are read and written through its identifier —
`table.selected`, `save_action.enabled`. But a component identifier never
reaches the built binary, so anything that refers to a component *from a
string* — a command, or another component's property — uses the component's
`name` property instead:

```
grid table
  name = "table"
  bind = "people"
end
```

`table.selected` is the identifier; `grid_cell("table", row, 1)` is the name;
`bind = "people"` names a datasource. Set `name` on any component you will
address from a command, and it is easiest to make it the identifier.

## Grid and datasource

A `grid` never holds its rows itself. It is bound by name to a `datasource`,
shows whatever that holds, and every grid bound to the same datasource shows
the same rows. Rows are one text: a newline between rows, a tab between cells,
because a property value must be a literal.

```
module people
use ui

datasource people
  name = "people"
  columns = "Name\tCity\tAge"
  rows = "Ada\tLondon\t36\nGrace\tArlington\t45"
end

form win
  title = "People"
  width = 520
  height = 300

  grid table
    name = "table"
    bind = "people"
    left = 20
    top = 20
    width = 480
    height = 200
    on select: on_select
  end

  label status
    text = "Pick a row."
    left = 20
    top = 240
    width = 480
  end
end

sub main
  call datasource_add_row("people", "Dennis\tNew York\t70")
end

sub on_select(row: int)
  status.text = "Selected " + grid_cell("table", row, 1)
end
```

The commands are the way around building that text by hand:
`datasource_add_row` and `grid_add_row` append a row, `grid_cell` and
`grid_set_cell` read and write one cell, `grid_row_count` counts, and every
row and column counts from 1. `selected` is the current row, `0` for none, and
can be assigned to move the selection. Rows added in `main` are on screen from
the first frame.

This is the shape a database kit will hand its query results into; there is
no such kit yet.

## Actions

An `action` is one command shared by several controls. The caption, the
shortcut, whether it can be invoked, and the code behind it live in the action
and nowhere else, and a button offers it by name:

```
module save_twice
use ui

var saves: int = 0

action save_action
  name = "save"
  text = "Save"
  shortcut = "ctrl+s"
  on execute: on_save
end

form win
  title = "Actions"
  width = 320
  height = 120

  button toolbar_save
    action = "save"
    left = 20
    top = 20
    width = 130
    height = 34
  end

  button menu_save
    action = "save"
    left = 170
    top = 20
    width = 130
    height = 34
  end
end

sub on_save
  saves += 1
  save_action.text = "Saved {saves}"
end
```

Both buttons show the action's caption, both fire it, and
`save_action.enabled = false` greys both — neither button knows the other
exists. The shortcut works whichever control has focus.

## Accessibility

Every component carries an accessibility role and name, and a running program
publishes a live accessibility tree that assistive technology reads. This is
part of the component model rather than something added to each control, so
it is true of anything you build without extra work.
