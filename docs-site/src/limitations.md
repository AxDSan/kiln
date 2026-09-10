# Limitations

Kiln is young. This page is the honest list, kept current against the
toolchain — it is more useful than discovering these one at a time, and a
limitation that has since been solved is removed rather than left to teach
you to ignore the page.

## Platforms

**Linux on x86-64, plus a cross build for Windows.** Programs — windowed
and console — and libraries cross-build for Windows x86-64 from Linux with
`--os windows` (see [Build targets](./build-targets.md)). A console program
is tested under wine and runs; a windowed one is tested under wine as far as
a machine without a display allows — it loads with its DLLs and reaches the
UI library — and its drawn window has not been checked under wine or on a
Windows machine. Accessibility is off in a Windows build: the a11y bridge
has no AccessKit Windows adapter yet. Studio cross-builds for Windows too, and
its drawn window has not been seen there either. Nothing is built natively *on*
Windows — the toolchain runs on Linux — and macOS and arm64 are not supported
at all.

**The `win` kit is declarations, and it is Windows-only.** `use win` brings in
the Win32 API — user32, gdi32, kernel32 and advapi32 — and a program that uses
it builds only with `--os windows`; on any other target it is refused by name.
It is written and tested on Linux: the examples in `examples/win/` cross-build
with mingw and run under wine, which is where a wrong struct offset or a
misspelled export shows up. Under wine the display is off, so a window is
created and its WNDPROC really is called back with `WM_PAINT` — that is
checked — but there is no framebuffer to read: **the drawn window has not been
looked at**, under wine or on a Windows machine. The kit binds the ANSI
(`...A`) entry points only, so a string outside the process's code page does
not survive; a program written against it alone builds for the **console**
subsystem, because `--target gui` is the UI stack rather than a subsystem
switch and refuses a module with no form — so a Win32 window made this way has
a console window beside it; it has no COM, and a struct with a union, a
bitfield or non-natural packing — `BITMAPFILEHEADER`, say — has no c-record
and must be laid out by hand.

## Programs you build

**A release build is hardened, not hidden.** `kiln build --release`
compiles at `-O2` with `_FORTIFY_SOURCE`, a stack protector, full RELRO with
symbols bound at load time, position independence, and no symbol table — see
[Build targets](./build-targets.md). That is a smaller, faster, harder-to-attack
program; it is not obfuscation. A native binary can always be disassembled, so
do not rely on one to keep an algorithm secret.

**A windowed release build is not position independent.** The vendored UI stack
is compiled without `-fPIC` and cannot go into a PIE, so a `gui` release says so
and links without it. Console programs and libraries are unaffected.

**TLS is opt-in, and only on the client side.** `net_http_get` speaks
`https://` once `tools/fetch-mbedtls.sh` has vendored mbedTLS; without it the
call fails rather than downgrading. The `httpserver` component never terminates
TLS — put a reverse proxy in front of it. See [Networking](./networking.md).

**A collection pauses the program, and the pause grows with the heap.** Memory
is reclaimed automatically (see [Memory](./memory.md)), but the collector runs
all at once rather than a little at a time: a program holding tens of megabytes
stops for tens of milliseconds when it collects. A server does not notice; a
game drawing at 60 Hz can, and its recourse today is `collect_garbage()` at a
moment it chooses. Generational and incremental collection are the fix and are
not written yet.

**The debugger is Linux-only, and stops at the statement.** Breakpoints,
stepping, the call stack, variables and hover all work — but the engine is
`ptrace`, so a Windows build has no debugger yet, and there is no expression
evaluation: you can see what a name holds, not compute with it. A `--release`
build carries no debug information and cannot be stepped through, which is
what `--release` is for.

## The language

- No user-defined types beyond `record`: an `enum` names a run of `int`s
  rather than making a type of its own — a parameter typed by one takes any
  `int` — and there is no interface and no way to attach a subroutine to a
  type.
- **String interpolation has no format specifiers.** `"{x}"` inserts a value
  the way `int_to_text` would; `"{x:03}"`, `"{x:.2f}"` or any other `:spec`
  is a parse error — *format specs like `{x:03:...}` are not supported yet*.
  Width, padding and decimal places are done by the `text` kit's own commands
  around the value, not inside the braces.
- `match` compares, it does not destructure: a `when` names values to test the
  subject against with `=`, and there is no binding a name out of the value and
  no matching on a record's shape.
- An optional is a **local's** type, and only a local's. A `T?` is a value with
  a hidden truth beside it saying whether the value is there, and a parameter,
  a return type, a list element and a record field each have room for one thing
  rather than two — so `sub f(v: text?)` is refused where it is written. Unwrap
  at the edge and pass a `T`; a subroutine that may have no answer still
  returns the answer plus the sentinel it documents.
- `check` hands its caller the sentinel a failing command of that type already
  returns — `0`, `""`, `false`, `ptr_null()`, or nothing at all. A subroutine
  returning a list, a dictionary or a record has no such sentinel, so `check`
  is refused there and the `if last_error_code() <> 0` is written out. For a
  related reason `defer` takes one simple statement and not a block: "the end
  of the block" is the whole of what it means.
- A dictionary's keys are text, and only text; asking with an `int` is a
  compile error.
- Local variables are visible for the whole subroutine, not just the block
  they were declared in. A `for` loop's variable — and a `for each`'s element,
  value and index bindings — follow the same rule, so two loops in one
  subroutine need different binding names. There are two ways out: a
  `repeat N times` has no counter to name, and a list built by a loop —
  `[n * 2 for each n in xs]` — binds a name that does not outlive its brackets,
  so two of those may each bind `n`. Neither may shadow a local already in
  scope.
- An indirect call — `call through fp(a, b): int` — is checked for the two
  things that can be known: the callee is a `ptr`, and every argument has a
  shape C can be handed. Nothing checks the signature *against the function*,
  because at the call there is no function, only an address; a wrong argument
  count, width or return type is a wrong C prototype, with whatever that does.
  COM is reachable this way — a vtable slot is a `ptr_read_ptr` and a
  `call through` — but nothing is bound for it: `IUnknown`, the `HRESULT`
  conventions and the `this` argument are all written out by hand.
- No mixing of numeric types in one expression: `d + 1` where `d` is a
  `double` is an error, not an implicit conversion. Write
  `d + int_to_double(1)`.
- A counting loop — `for i = 1 to n`, and its shorthand `for i in 1..n` —
  counts with `int`, not `int64`. A range whose bounds are `int64` is a
  compile error; count with an `int` and widen inside the loop.
- One form per module.
- A unit's names are merged into the program's without a prefix, and a
  diagnostic inside one is positioned by the unit's line number. The build
  says which unit file it means; the editor, when the *program* is the open
  file, counts the unit's errors and points at the file rather than misplacing
  a squiggle. Completion and go-to-definition see the open file only, not the
  units it uses. Debug information names the program's file for every
  subroutine, so a breakpoint set in a unit file does not resolve — set it by
  subroutine name instead.
- A form or component property value must be a literal. A component's
  properties can be set again from code, but the form's own cannot — the form
  is not a component — so a window title cannot be computed or translated at
  start-up.
- A library command takes its arguments in order. A `sub` and a `dll` declare
  parameter names, so `connect(host: "example.com")` reaches them; a command's
  library metadata carries types only, and a named argument to one is refused
  rather than matched by guesswork.
- A c-record has no value form: it accepts a record literal where it is
  declared (`var r: rect = rect{left: 10}`, which is the zeroed declaration
  plus those field writes) and nowhere else, and it has no `{...base}` update,
  because there is nothing to copy from.
- A support library cannot take or return a record or a dictionary. The ABI
  has a tag for each, and core's `dict_*` commands use it, but the layouts are
  the runtime's own; a library sees `int`, `int64`, `double`, `bool`, `text`,
  arrays and `bytes`.

## The component library

The components each library provides are listed in
[Components](./reference-components.md), and what they are for is in
[Components](./components.md). The designer's toolbox lists four more greyed
out — a tab control, a splitter, a file dialog and a tray icon: they are part
of the intended shape but are not implemented, and adding one tells you so
rather than placing something that does not work.

A `grid` shows what a `datasource` holds, and a datasource holds text —
rows separated by newlines, cells by tabs. There is no database kit yet to
fill one from a query.

## The IDE

- The console shows the tail of the output rather than a full scrollback.
- No project-wide search, no refactoring, no version-control integration.
- Rename and formatting are not implemented, in Studio or in an external
  editor — the language server offers neither.

## What does work

Design a form, wire an event, build it, run it, and ship the binary — plus
console programs, servers and libraries, on Linux, today — and the same
program or library cross-built for Windows. Everything above is what is
missing from that, not a warning that the core does not function.
