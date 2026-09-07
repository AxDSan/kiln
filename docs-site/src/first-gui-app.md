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

Select the button, switch the inspector to **Events**, and put a subroutine
name against `click`. If the subroutine does not exist yet, Kiln writes an
empty one for you.

Then open the **Code** tab and fill it in:

```
sub on_ok_click
  greeting.text = "Button clicked."
end
```

`greeting` is the label's name, and `text` is one of its properties — the same
property you can see in the inspector. Setting it from code and setting it in
the inspector are the same operation.

## Run it

Press **Run**. Studio builds the project, launches it, and streams whatever it
prints into the console pane along with its exit code.

**Build Binary** does the same without running, leaving an artifact you can
ship.

## The whole file

```
module my_app
target gui
use ui

form main_window
  title = "my_app"
  width = 480
  height = 300
  background_color = "#1e2233"

  label greeting
    text = "Click the button."
    left = 40
    top = 50
    width = 200
    height = 24
  end

  button ok_button
    text = "Click me"
    left = 40
    top = 110
    width = 160
    height = 32
    on click: on_ok_click
  end
end

sub on_ok_click
  greeting.text = "Button clicked."
end
```

`use ui` brings in the visual components. See [Forms and
events](./forms-and-events.md) for the full shape, and
[Components](./reference-components.md) for every component and property.
