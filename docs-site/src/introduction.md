<div align="center">
  <img src="./assets/kiln-wordmark.png" alt="Kiln" width="380">
</div>

# Introduction

Kiln is an open implementation of **Easy Programming Language** (易语言,
usually shortened to EPL) — a RAD environment where you build desktop software
by drawing it.

You lay a window out visually, set properties in an inspector, wire a button's
click to a subroutine, and press **Run** — and what comes out is an ordinary
native binary you can hand to someone.

![The Kiln Studio visual designer](./assets/screenshot-designer.png)

The language is English-first and deliberately small. There is one way to call
things, no pointers, no manual memory management, and no ceremony:

```
module hello
target console

sub main
  call print_text("Hello from Kiln.")

  let answer = 6 * 7
  call print_text("six times seven is {answer}")
end
```

Assignment is a statement rather than an expression, so `if x = 5` compares —
it cannot silently assign.

## What makes it different

**The visual designer is not a separate tool.** The form you draw and the file
you edit are the same thing. Drag a button in the designer and the source
changes; edit the source and the canvas follows.

**Programs are ordinary native binaries.** Your project is compiled to machine
code and linked with the system linker. Nothing is unpacked at startup, no
support libraries are loaded at run time, and there is no interpreter inside.
Programs stay small, start immediately, and look unremarkable to antivirus
software.

**One project builds every artifact.** The same source can become a console
program, a windowed program, a shared library or a static library — a build
option rather than a rewrite. See [Build targets](./build-targets.md).

**A program that waits needs no window.** A timer or an HTTP server is a
component with no rectangle, declared beside the form or without one, and the
runtime's event loop keeps the program alive while any such source is live.
See [Components](./components.md).

## Its relationship to EPL

Easy Programming Language is a Chinese RAD environment with a large following:
a visual designer, a component library, event-driven code, and a compiler that
produces standalone native executables. That model is what Kiln implements.

It is an open implementation of the idea rather than a clone. Kiln does not
read or run existing EPL programs, and its keywords are English rather than
Chinese, so the language is approachable to people who do not read Chinese —
and the whole toolchain is open source, cross-platform and inspectable.

## Where to start

- [Installation](./installation.md) — download or build it
- [Quick start](./quick-start.md) — a program in about a minute
- [Your first GUI app](./first-gui-app.md) — draw a window and wire a button
- [A tour of the language](./tour.md) — arrays, records, dictionaries,
  subroutines and the error slot, in the order you meet them

## What is not here yet

Kiln is young. [Limitations](./limitations.md) lists what does not exist
yet, plainly — it is worth reading before you plan anything around it.
