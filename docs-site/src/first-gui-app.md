# Your first GUI app

## Create it

```sh
kiln new gui-app my-app
bin/kiln-studio my-app/main.kiln
```

Or start Studio with no arguments and pick **GUI Application** from the
welcome screen.

## Draw the window

![The visual designer](./assets/screenshot-designer.png)

- **Toolbox** on the left — drag a component onto the form, or click to add it.
- **Canvas** in the middle — drag components to move them, drag the handles to
  resize, and drag the form's corner to resize the window itself.
- **Inspector** on the right — every property of the selected component.
  Type a value and press Enter.
- **Code preview** below — the source for whatever is selected. Click it to
  open the editor.

Everything you do here edits the file. There is no separate designer format
that could drift from your source.

## Wire the button

Select the button, switch the inspector to **Events**, and give `Click` a
handler name. If the method does not exist yet, Kiln writes an empty one for
you.

Then open the **Code** tab and fill it in:

```
void OnOkClick()
{
    greeting.Text = "Button clicked.";
}
```

`greeting` is the label's name, and `Text` is one of its properties — the same
property you can see in the inspector. Setting it from code and setting it in
the inspector are the same operation.

## Run it

Press **Run**. Studio builds the project, launches it, and streams whatever it
prints into the console pane along with its exit code.

**Build Binary** does the same without running, leaving an artifact you can
ship.

## The whole file

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

The first `partial form` block is the designer's half — Studio reads and writes
it. The second is yours: state and the methods behind the events. They are two
blocks in the one file so that the whole window, how it looks and what it does,
is one thing you can read top to bottom.

`using Kiln.Ui;` brings in the visual components. See [Forms and
events](./forms-and-events.md) for the full shape, and
[Components](./reference-components.md) for every component and property.
