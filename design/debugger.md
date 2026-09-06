# The OpenEPL debugger

**Status:** Phase 0 shipped in 0.10.1. Phases 1 and 2 done. Phases 3–8 planned.

OpenEPL ships its own debugger. Not a wrapper around gdb or lldb, and not a
dependency on either being installed — the bundle's promise is "unpack and
run, nothing to install", and a debugger that needs a system debugger breaks
it. Every layer below is ours.

This is what the research settled and what it left open. It was written after
surveying Lazarus/FpDebug (a RAD IDE that wrote its own DWARF debugger in its
own language and migrated *off* gdb — the closest analogue to this), Delve
(a from-scratch native debugger for one language), the Debug Adapter
Protocol, LLVM's textual debug metadata, and how Visual Studio, Delphi,
Qt Creator and JetBrains architect theirs.

## The shape

Three layers, and the middle one is where "ours" lives.

1. **Debug info from the backend.** `backend/src/lib.rs` emits LLVM IR as
   *text*, so `-g` on clang buys nothing: the metadata has to be written into
   the IR by hand. `!DICompileUnit`, `!DIFile`, `!DISubprogram` per sub,
   `!DILocation` on every instruction from `Stmt.line`, `!DILocalVariable` +
   `#dbg_declare` on the alloca slots. This layer is needed whatever sits on
   top of it.
2. **`openepl debug`** — a Rust binary speaking **DAP over stdio**, backed by
   a new `openepl-debug` crate: ptrace for process control, `gimli` for DWARF
   and CFI, `object` for ELF. This is the house shape (the CLI does the work,
   Studio drives it as a subprocess) and it is `lspclient.h` again. It owns
   the *OpenEPL* model: arrays shown 1-based, `text` rendered as characters,
   records with real field names, runtime frames filtered out of a backtrace,
   step-over that does not descend into `print_text`. DAP also means the VS
   Code extension gets debugging for free — one server, many editors.
3. **Studio.** Gutter breakpoints, a stopped-line tint, step/continue on the
   toolbar, Variables and Call Stack panes beside PROBLEMS.

## Why our own engine, concretely

Owning the engine is what buys the RAD tier, and the RAD tier is the point:
breaking inside an `on_click` handler and seeing both your variables *and*
the form's component state. A `.debug_openepl` sidecar section carries the
component → handle map, so Studio can show a Components scope beside Locals
and offer "break when this button is clicked". Classic 易语言 never reached
that tier — its own community's defect list records only single-step, watch,
debug statements and breakpoints.

## Phases

Each ships alone and names the command that proves it.

| # | What | Proven by |
|---|---|---|
| 0 | **Hoist the inline allocas** — done, 0.10.1 | a million command calls in a loop exits 0 |
| 1 | **A line table — done** | `objdump --dwarf=decodedline` lists a row per statement, and gdb breaks on `loops.oir:24`, shows the source, and backtraces |
| 2 | **The symbol layer — done.** `openepl-debug`, a 4th workspace member | the engine's line table is identical to objdump's, row for row |
| 3 | Unwinding via gimli's CFI evaluator, unit-tested on synthetic registers | `cargo test -p openepl-debug` |
| 4 | Launch, int3 breakpoints, stepping, backtrace | `openepl debug --batch 'break x.oir:5; run; where; next'` |
| 5 | Locals, rendered in OpenEPL's value model | `locals` prints `nums = [1, 2, 3]` (1-based), `p = Point { x: 1 }` |
| 6 | DAP, and VS Code for free | `cli/tests/dap.rs` drives the full handshake |
| 7 | Studio: gutter, stopped line, panes, hover — see below | a scripted session plus a rendered frame, per CLAUDE.md |
| 8 | The RAD wins: component state at a breakpoint, break-on-click | a click drives a stop with the button's caption shown |

Phase 2 before Phase 4 is FpDebug's ordering lesson taken literally: they read
debug info for a year before controlling a process, and their DWARF parsers
are the least-churned code in their tree while stepping took 465 commits.

## What Phase 1 actually did

`backend/src/debug.rs` builds the metadata block; `Body` — a `fmt::Write`
wrapper around what used to be a `String` — attaches `!dbg` to every
instruction with **no change to the 198 places that write one**. Only user
subroutines carry debug information; the synthesised functions (entry point,
library initialiser, export wrappers, event thunks) deliberately carry none,
which is what makes the verifier's "a call in a function with debug info must
have a location" rule a non-event rather than the day-one blocker it was
predicted to be.

Two things had to be given locations that have no statement behind them: the
prologue's `alloca`s and the closing `ret`. An instruction with no location
makes a line-table row with **no line**, and a debugger stepping into one shows
no source — so the prologue is attributed to the `sub` header and the tail to
the last statement. This was caught by asserting no row has line 0, not by
reading the IR.

Stock gdb is used as an *oracle*, never as a dependency: if gdb cannot see our
lines, nothing we write will either.

```
Breakpoint 1, oe_user_main () at examples/loops.oir:24
24	  call print_text("-- fizzbuzz, counted --")
#0  oe_user_main () at examples/loops.oir:24
#1  0x0000000000400c36 in ECodeStart ()
#2  0x000000000040168c in main ()
```

The Windows cross-build carries DWARF in PE too, unchanged — so Phase 2's
symbol layer can read both from the start.

## What Phase 2 actually did

`debug/` is a new workspace member, `openepl-debug`. It loads a built program
with `object`, reads the line program with `gimli`, and indexes it both ways:
address → line, and line → the address a breakpoint goes at. It runs nothing;
there is no `ptrace` in it. Seven transitive dependencies, all MIT-compatible.

`openepl debug --dump-lines / --dump-subs / --resolve / --at` exercises it.

Two things the first attempt got wrong, both found by comparing against an
oracle rather than by reading the code:

- **Other people's compile units.** Every binary built here links glibc's
  `atexit.c`, *with its debug information*. Merging its rows into the table
  attributed `atexit.c`'s line 45 to the user's source. Units are now filtered
  by `DW_AT_producer`, which the backend writes — so the user-frame filter is
  explicit, as the critique demanded, rather than a side effect of where
  unwinding happens to stop.
- **`LineTablesOnly` emits no `DW_TAG_subprogram` DIEs.** Function extents
  come from the ELF symbol table instead, keyed on the `oe_user_` prefix. That
  is enough for "which function is this address in", and it is stripped by
  `--release` on exactly the same terms as the line table.

`--resolve loops.oir:24` returns `0x400557` — the same address gdb picks for
`break loops.oir:24`, arrived at independently.

## How Studio wires up (Phases 7 and 8)

Studio talks to `openepl debug` the same way it already talks to
`openepl lsp`: a subprocess on a pipe, speaking a JSON protocol with
`Content-Length` framing. `designer/dbgclient.h` is `lspclient.h` again —
same spawn, same non-blocking read loop, same `updated_`/`has_update()`
pattern for replies that land between frames.

**Studio never traces the program itself.** Debug-run is a different path from
Run, not a modified one: `run_app()` keeps its `fork`/`dup2`/`execl` for a
plain Run, and `debug_app()` spawns the adapter, which owns the debuggee. This
is not tidiness. `stop_app()` does a blocking `waitpid` on the UI thread
today, and under ptrace *every* breakpoint hit and *every* single step becomes
one of those stalls — the exact thing the non-blocking drain was written to
avoid. Putting the tracer in its own process sidesteps the problem entirely.
It also quietly fixes `kill(SIGTERM)`, which is wrong for a traced child: it
stops rather than dies.

**The stopped line is nearly free.** `refresh_highlight()` already picks a row
class from state — `main.cpp:2343` emits `<div class='badline'>` or `<div>`.
A stopped row is a third class and one CSS rule beside `main.cpp:977`.

**The gutter is a third sibling, not a layer.** The Code view is two stacked
elements (`main.cpp:1356`): `#codehl` paints colour underneath and a real
`<textarea id='fullcode'>` sits on top supplying caret, selection and
clipboard. A dot drawn inside `#codehl` receives no clicks — the press lands
on the textarea. So the gutter is a third element to the left of both, with
the other two shifted right by its width, built in the same loop and sharing
`theme::CODE_LINE_H` exactly. The file already warns that any typographic
difference between the layers shows up as text drifting away from its colour,
and the gutter inherits that discipline. Its scrollbar must be sized or
hidden: an unstyled RmlUi scrollbar eats clicks, which has broken Studio
twice.

**The cache key is the trap.** `refresh_highlight()` returns early when the
text, first line, row count, `marks` and horizontal scroll are all unchanged
(`main.cpp:2302-2311`). The `marks` string exists precisely because
diagnostics changing under unchanged text otherwise never repainted — and a
breakpoint toggled without editing is exactly that case. The breakpoint set
and the stopped line must join the key, or clicking the gutter silently does
nothing. This gets its own regression test; it is the kind of bug that passes
every unit test and fails on screen.

**Marker shapes carry meaning.** Solid dot for a breakpoint bound to an
address, hollow for one the engine could not bind, arrow for the stopped row.
A breakpoint that silently never fires is the worst thing a first version can
do, and the distinction is nearly free once a line table exists.

**New panes** join the existing bottom dock beside PROBLEMS and OUTPUT:
Variables, Call Stack, Watch — and Components, only when the program has a
form. Tables rather than a command prompt, which is what this product family's
users expect. The call stack shows OpenEPL frames by default with a "show
internal frames" toggle: a click handler's real stack is `main → ECodeStart →
oe_ui_run → oe_loop_run → ui_pump → Backend::ProcessEvents → [SDL] → [RmlUi
dispatch] → HandlerBridge::ProcessEvent → oe_evt_on_click_i32 →
oe_user_on_click`, and showing all twelve makes the pane useless. **Filtering
is by DWARF producer, not by symbol prefix** — the runtime's C is compiled in
the same build, so a prefix test would misclassify it.

**Hover a variable to see its value.** Studio already turns an x offset into a
column and has an identifier lexer in `highlight.h`. Hover sends an `evaluate`
with `context: "hover"` and renders the tip when the reply lands —
asynchronously, following the LSP client's `has_update()` pattern rather than
pumping on mouse-move, which would stutter the frame loop. This is the single
most-requested debugging behaviour in the 易语言 material.

**Keys** follow the family: F5 debug-run, F9 toggle breakpoint, F10 step over,
F11 step in, Shift+F11 step out, Shift+F5 stop. F5 is *debug*-run because that
is the expectation here; Studio already has plain Run on the toolbar.

**Headless testing** adds verbs to the existing script harness — `bp:<line>`,
`dbgrun`, `dbgstep`, `dbgnext`, `dbgout`, `dbgcont`, `dbgstop`, `waitstop`,
`frames`, `locals`, `watch:<name>`. `waitstop` blocks on the adapter's
`stopped` event through the same shape `waitdiag`/`waitdef`/`waitcomplete`
already use for asynchronous LSP replies, so there is no new machinery.
Fixtures are **console** programs, so the debuggee never opens a window.

The proof is a rendered frame, not a passing assertion:

```sh
OPENEPL_DESIGNER_DUMP=/tmp/stop.ppm \
OPENEPL_DESIGNER_SCRIPT='view:code;bp:12;dbgrun;waitstop;locals;dbgnext;waitstop' \
  designer/openepl-designer /tmp/fixture.oir target/release/openepl
```

— and then looking at the frame for the solid dot on 12, the tint on 13, and a
populated Variables pane. A Studio bug passes tests and fails on screen.

**Phase 8, the part no wrapper can reach.** Right-click a button on the form
and choose "break when clicked". Handlers are compiler-emitted functions bound
by pointer, with the surface event name in the component descriptor, so a
`(component, event) → symbol` table emitted beside the DWARF turns that into a
symbol lookup — set from the *form designer*, with no idea where the handler's
first line is. And a Components pane showing `button1.caption` as it is right
now, because the runtime's property accessors are already string-keyed by
name. Reading it needs no function-call injection: we link a plain data mirror
into debug builds and read it out of the stopped process. FpDebug took seven
years to get call injection; we do not need it, because we own the runtime.

## Decisions taken

- **Debug info is on by default; `--release` strips it.** One emission path,
  so Studio's Run is debuggable without a mode switch — what every other
  compiler does.
- **No 198-site edit.** `self.body` becomes a wrapper implementing
  `fmt::Write` that appends `, !dbg !N` to indented instruction lines when a
  location is set. Labels are left alone. The five sites that assemble whole
  function bodies with `format!` (`:243`, `:247`, `:264`, `:737`, `:3971`)
  bypass it and need a fallback location, or LLVM's verifier discards the
  module's debug info wholesale.
- **Fail the build on `ignoring invalid debug info`.** Verified: `-w` and
  `-Wno-everything` do not suppress it, and clang exits 0 while producing no
  `.debug_*` sections at all. Without the check, every emitter bug looks like
  "the debugger sees nothing".
- **CFI is the primary unwinder**, frame pointers an optimisation — not the
  reverse. A naive rbp walk silently drops `main` when stopped at `low_pc`.
  Always break at `prologue_end`, never `low_pc`.

## Traps the critique found, to fold in before writing Phase 1

Each is a silent-failure class — wrong without an error message.

- **CFA direction.** The stack grows down, so a callee's CFA is *smaller*.
  `stepIn` is `frame.cfa < stopped.cfa`; `stepOut` is `>`; `next` is `==`.
  Get it backwards and stepIn silently degenerates into continue.
- **Leaving the topmost user frame means continue, not stop.** A click
  handler sits under 8–12 non-user frames across three languages. RmlUi is
  built `Release` with no `-g`, so a return-address breakpoint there has
  nothing to map to; the runtime *will* have a line table, so a user pressing
  `next` would start stepping OpenEPL's own event loop.
- **`.eh_frame` per mapped module.** SDL2 links shared (`-lSDL2`; there is no
  `libSDL2.a`), so pausing an idle form app stops inside `libSDL2.so` and a
  single-module unwinder yields a one-frame stack.
- **Stop all threads on any thread's stop.** `PTRACE_O_TRACECLONE` makes
  siblings traced; it does not stop them. Reading locals while another thread
  mutates them is a torn value, and DAP's `allThreadsStopped: true` would be
  a lie.
- **Globals need `!DIGlobalVariableExpression` and a `globals:` field** on the
  compile unit, or module variables are unreadable however good the engine is.
- **Record locals are two different shapes.** A heap record's alloca holds a
  `ptr`, so its `DILocalVariable` type is a `DW_TAG_pointer_type` *to* the
  composite; a c-record is a flat `[N x i8]` alloca, so the bare composite is
  correct. One sentence covering both renders garbage for one of them.
- **`launch` is a three-state machine**, not two: build, then stream clang's
  output as `output` events, then trace. A DAP adapter that blocks silently
  through a multi-second compile looks hung.

## Not doing

Windows process control (the `Target` trait is the seam that keeps the door
open), optimised-code debugging, watchpoints, and edit-and-continue.
