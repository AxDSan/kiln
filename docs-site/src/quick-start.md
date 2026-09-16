# Quick start

## From the IDE

```sh
bin/kiln-studio
```

Studio opens on a welcome screen. Pick a project kind and it is created and
opened for you.

![Choosing a project template](./assets/screenshot-welcome.png)

## From the command line

```sh
kiln templates                 # what you can create
kiln new console-app hello     # create a project
kiln run hello/main.kiln        # build it and run it
```

Every template builds and runs the moment it is created — `kiln new` is
never a starting point you have to repair first.

That last command prints:

```text
Hello from Kiln.
six times seven is 42
```

## Writing it yourself

Create `hello.kiln`:

```k2
namespace hello;

public static class Program
{
    public static void Main()
    {
        Console.WriteLine("Hello from Kiln.");

        let answer = 6 * 7;
        Console.WriteLine($"six times seven is {answer}");
    }
}
```

Then:

```sh
kiln run hello.kiln
```

`kiln build hello.kiln` leaves a binary called `hello` next to the source
instead of running it. It has no dependency on Kiln — copy it to another
machine of the same platform and it runs.

## A program that waits

`console-app` prints and stops. Some programs should not: they wait for
something and act when it arrives. `examples/loopdemo.kiln` is that shape.

```sh
kiln run examples/loopdemo.kiln
```

```text
main returned; the timer keeps the program alive
tick 1
tick 2
tick 3
```

`Main` printed one line and returned — and the program kept running, because
the program declares a `Timer`, and the runtime stays in its event loop while
any source is live. It ends when the tick handler calls `Quit()`. A timer draws
nothing, so it is declared at the top of the file rather than inside a form: a
program that waits for something needs no window at all.

## In your editor

`kiln lsp` is a language server, and every editor that speaks LSP can use
it. You get errors underlined as you type, completion for commands, components
and your own methods, the parameter list while you are typing a call, and
go-to-definition on any name.

```sh
kiln lsp        # started by your editor, not by you
```

[Editor setup](./editors.md) has
ready-made configuration for Neovim, VS Code, Helix and Zed. The language
server resolves libraries and kits exactly as the compiler does, so anything it
underlines is something that would genuinely fail to build.

## What the pieces mean

- **`namespace hello;`** names the compilation unit. Every sample here starts
  with one.
- **`public static class Program`** holds the program's methods, and **`Main`**
  is where it starts. Top-level statements are the shorter spelling of the same
  thing: they become `Main`.
- **`Console.WriteLine(...)`** writes a line of text. A standard-library
  command is an ordinary method: `using Kiln.File;` makes `file_read_text`
  reachable as `File.ReadText`.
- **`let`** declares a value that will not change. Use `var` when it will.
- **`$"..."`** puts a value inside text: `$"six times seven is {answer}"`.
- **Everything counts from 1.** The first element of a list is `xs[1]`, the
  first character of a text is at position 1, and `0` is free to mean *not
  found* — which is what `Find` answers when there is nothing there.
- **There is no target to declare.** A file with a `partial form` is a windowed
  program and links the UI library; anything else is a console program.

Continue with [Your first GUI app](./first-gui-app.md), or read
[Kiln 2](./kiln-2.md) — the whole language, in the order you meet it.
