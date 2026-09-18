# Forms in Kiln 2

A form is a window, and the block that declares it is the source — not a
resource file beside the source, not a generated file you are told not to edit.
Kiln 2 keeps that, which is the part of Kiln worth keeping.

```sh
kiln run counter.kiln
```

A program with a form links the UI library and enters the event loop; you do
not say so anywhere.

## The two halves

A form is written as two `partial form` blocks. The first is the designer's:
properties, components and wiring. The second is yours: state and handlers.

```k2
namespace Counter;

// The designer's half — Studio owns this one.
public partial form MainWindow
{
    Title = "Counter";
    Width = 320;
    Height = 200;

    Label count
    {
        Text = "0";
        Left = 40;
        Top = 40;
        Width = 220;
        Height = 28;
    }

    Button add
    {
        Text = "Add one";
        Left = 40;
        Top = 100;
        Width = 140;
        Height = 34;
        Click += OnAdd;
    }
}

// Your half, in the same file.
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

They are two blocks rather than two files so that the whole window — how it
looks and what it does — is one thing you can read top to bottom. Nothing stops
you writing one block if you are not using the designer.

## State

A form's fields hold state between events. There is no form instance and no
`this`: a field is a module global, which is why a handler can reach it without
being handed anything.

```k2
namespace Score;

public partial form MainWindow
{
    Title = "Score";

    Label total { Text = "0"; Left = 20; Top = 20; Width = 200; Height = 24; }

    Button hit
    {
        Text = "+7";
        Left = 20;
        Top = 60;
        Width = 80;
        Height = 30;
        Click += OnHit;
    }

    Button reset
    {
        Text = "Reset";
        Left = 110;
        Top = 60;
        Width = 80;
        Height = 30;
        Click += OnReset;
    }
}

public partial form MainWindow
{
    int points;
    string label = "points";

    void OnHit()
    {
        points = points + 7;
        total.Text = $"{points} {label}";
    }

    void OnReset()
    {
        points = 0;
        total.Text = "0";
    }
}
```

## Wiring an event

`Click += OnAdd` names a method of the form. A handler written where it is
wired works too, for something too small to name:

```k2
namespace Inline;

public partial form MainWindow
{
    Title = "Inline";

    Label shown { Text = "—"; Left = 20; Top = 20; Width = 200; Height = 24; }

    Button go
    {
        Text = "Go";
        Left = 20;
        Top = 60;
        Width = 80;
        Height = 30;
        Click += () => { shown.Text = "went"; };
    }
}
```

## Wiring an event at run time

An event can also be wired from code, with `+=`, and unwired with `-=`. A
handler wired this way may capture what it needs, which is what makes a UI
built from data possible — each button knows which row it belongs to:

```k2
namespace Rows;

public partial form MainWindow
{
    Title = "Rows";

    Label shown { Text = "pick one"; Left = 20; Top = 20; Width = 240; Height = 24; }
    Button first { Text = "First"; Left = 20; Top = 60; Width = 80; Height = 30; }
    Button second { Text = "Second"; Left = 110; Top = 60; Width = 80; Height = 30; }
    Button setup { Text = "Set up"; Left = 200; Top = 60; Width = 80; Height = 30; Click += OnSetup; }
}

public partial form MainWindow
{
    void OnSetup()
    {
        var names = new List<string>();
        names.Add("alpha");
        names.Add("beta");
        var i = 0;
        foreach (var name in names)
        {
            i = i + 1;
            if (i == 1) first.Click += () => { shown.Text = name; };
            if (i == 2) second.Click += () => { shown.Text = name; };
        }
        setup.Click -= OnSetup;
    }
}
```

Each turn of the loop gives its lambda its own `name`, so the two buttons show
different things. `-=` removes a handler by the same rule a delegate follows:
a method comes off by name, and a lambda only if it is the very same one.

A captured variable stays alive exactly as long as the handler that needs it,
however much the program allocates in between — the runtime holds it on the
handler's behalf, where the collector can see it.

## Setting a property at run time

Assigning to a component's property sets it on the live window:

```k2
namespace Live;

public partial form MainWindow
{
    Title = "Live";

    Label status { Text = "ready"; Left = 20; Top = 20; Width = 240; Height = 24; }
    Editbox entry { Text = ""; Left = 20; Top = 56; Width = 160; Height = 26; }

    Button apply
    {
        Text = "Apply";
        Left = 190;
        Top = 56;
        Width = 76;
        Height = 26;
        Click += OnApply;
    }
}

public partial form MainWindow
{
    int applied;

    void OnApply()
    {
        applied = applied + 1;
        status.Text = $"applied {applied}";
        entry.Text = "";
    }
}
```

## What you can put on a form

`kiln commands --use ui` lists every component, its properties and its events —
that listing is generated from the toolchain, so it is never out of date. The
visual ones are `button`, `checkbox`, `combobox`, `editbox`, `grid`,
`groupbox`, `image`, `label`, `listbox`, `memo`, `progressbar`, `radiobutton`,
`slider` and `spinner`. Written in a form they take PascalCase: `Button`,
`Editbox`, `Listbox`.

`action`, `datasource` and `timer` have no rectangle. They are declared beside
the form rather than inside it.

Properties are PascalCase too: `background_color` is `BackgroundColor`,
`border_radius` is `BorderRadius`.

## Themes

Every control is drawn from a *theme*: one palette of colours, radii and text
sizes the whole form shares. A form names the one it wants, and a control that
sets a colour of its own still wins — the theme decides what a control that says
nothing looks like.

```k2
namespace Themed;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "Themed";
    Width = 360;
    Height = 200;
    Theme = "Dark";

    Label caption { Text = "Dark by default"; Left = 20; Top = 20; Width = 260; Height = 24; }
    Button swap { Text = "Light"; Left = 20; Top = 70; Width = 120; Height = 34; Click += OnSwap; }
}

public partial form MainWindow
{
    void OnSwap()
    {
        Ui.SetTheme("Light");
        caption.Text = Ui.Theme();
    }
}
```

| Theme | |
| --- | --- |
| `Light` | the default: a light grey ground, a blue accent |
| `Dark` | the same structure with the surfaces and the text swapped |
| `HighContrast` | black on white, heavier outlines, no shadows |
| `System` | whichever of light and dark the desktop is set to |

`Ui.SetTheme` switches while the program runs. It repaints every control from
the new palette and creates nothing: a value being typed, a selected row, a
wired handler and a scroll position all survive the switch. `Ui.Theme()` answers
the name in force — `System` stays `System`, because that is what the program
asked for. A name that is not a theme is refused and the palette is left alone.

`KILN_UI_THEME=Dark ./app` overrides whatever the form says, which is how a
program is looked at in another palette without touching its source.

## Studio

Studio reads a Kiln 2 form through `kiln inspect` and writes it through
`kiln edit` — never as text. That is not an implementation detail; it is what
makes the file yours:

- Dragging a component and saving changes the coordinates in the file and
  nothing else — not the formatting, not the comments, not a line of your half.
- Dropping a button and double-clicking it writes `Button button1` into the
  designer's block and `void Button1Click()` into yours, wired together.
- Anything you wrote by hand survives, because the file is changed through its
  own syntax tree rather than by splicing lines into it.

The same commands are there for you:

```sh
kiln inspect app.kiln                      # the form, as lines
kiln edit app.kiln set count Text "hello"  # one property
kiln edit app.kiln add Button save         # a component
kiln edit app.kiln on save Click OnSave    # wire an event
```

## Accessibility

A form publishes an accessibility tree as it builds: the window, and every
component with its role and its label. A screen reader sees a real window with
real controls, not a canvas. Nothing is required of you for this — it comes
from the same declarations that draw the form.

## Checking a window without opening one

Set `KILN_UI_DUMP` to write the first frame to a file and
`KILN_UI_EXIT_AFTER_FRAMES` to stop after it:

```sh
KILN_UI_DUMP=frame.ppm KILN_UI_EXIT_AFTER_FRAMES=1 ./counter
```

That runs with no window and no focus stolen, which makes it usable from a
test or a CI job. It is also how the forms on this page were checked.
