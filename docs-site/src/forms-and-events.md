# Forms and events

A windowed program declares a `partial form`. The form is the window; the
components inside it are what the window contains.

```k2
namespace my_app;

using Kiln.Ui;

public partial form MainWindow
{
    Title = "my_app";
    Width = 480;
    Height = 300;
    BackgroundColor = "#1e2233";

    Label greeting
    {
        Text = "Click the button.";
        Left = 40;
        Top = 50;
        Width = 200;
        Height = 24;
    }

    Button ok_button
    {
        Text = "Click me";
        Left = 40;
        Top = 110;
        Width = 160;
        Height = 32;
        Click += OnOkClick;
    }
}

public partial form MainWindow
{
    void OnOkClick()
    {
        greeting.Text = "Button clicked.";
    }
}
```

## The shape

```
public partial form <name>
{
    <Property> = <value>;      // the window's own properties

    <Type> <component>         // a component
    {
        <Property> = <value>;
        <Event> += <Handler>;
    }
}

public partial form <name>     // your half
{
    <fields and handler methods>
}
```

Both blocks name the same form: the first is the designer's, the second is
yours. A program that does not use the designer writes one block. Component
names are unique within the file and share one namespace with methods and
fields, so nothing can be two things at once.

A component with no rectangle — `Timer`, `Action`, `Datasource`, `Httpserver`
— is not declared inside the form but beside it, at the top level, in the same
block shape. A console program can declare one without any form at all; that
is how a program waits for something. See [Components](./components.md).

## Properties

Properties are set with literals here, and with ordinary statements once the
program is running:

```
greeting.Text = "Ready.";
```

Both are checked at build time — an unknown property, or a value of the wrong
type, is a compile error naming the component and the property. A colour is a
text value such as `"#4a86e8"` on `Color` or `BackgroundColor`.

Every component and every property is listed in
[Components](./reference-components.md).

## Events

`Click += OnOkClick;` binds an event to a method of the form.

```
Button ok_button
{
    Click += OnOkClick;
}
```

Bindings are resolved when the program is built: if the method does not exist,
the build fails rather than the button quietly doing nothing.

Some events hand the handler a value — a `Grid`'s `Select` hands the row, a
`Timer`'s `Tick` hands the tick count. The handler takes exactly that, or
nothing at all; the compiler makes the two agree and shows the header to paste
when they do not. `Click` hands nothing, so a click handler is always the
plain shape above. What each event hands over is in
[Components](./components.md).

## Doing this visually

You do not have to write any of this by hand — the designer produces exactly
this shape, and reads it back. Drag a button onto the canvas and the `Button`
block appears; type in the inspector and the property changes; wire an event
and `Click += ...` is written, along with an empty method if you have not made
one yet.

There is no separate designer file. What you draw and what you edit are the
same source, which is why they cannot disagree.

## Order of events

For a windowed program:

1. the window and its components are created,
2. `Main` runs, if the program has one,
3. the event loop starts and your handlers run as things happen.

`Main` is optional here. It is a place for setup that has to happen before
anything is shown.

## Accessibility

Components carry an accessibility role and name, and the running program
publishes a live accessibility tree that assistive technology can read. This
is part of the component model rather than something bolted on afterwards, so
it is true of anything you build without extra work.
