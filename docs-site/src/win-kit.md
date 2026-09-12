# The `win` kit

`use win` is the Win32 API. One line at the top of a module, and a program has
`CreateWindowExA`, `WNDCLASSEXA`, `WM_PAINT`, `ReadProcessMemory`,
`RegOpenKeyExA` and 1,300 more names, without transcribing a single
declaration.

```text
module hello
use win

sub main
  call MessageBoxA(ptr_null(), "Built with Kiln.", "Hello", MB_OK)
end
```

It is a [declaration kit](./interop.md#declaration-kits): a directory of `.kdecl`
files holding `dll` lines, `is c` records and `const` numbers and nothing else.
The kit does not ship code — there is nothing to link, nothing to install, and
no wrapper between a program and the API. `CreateWindowExA` in a Kiln
program is `CreateWindowExA` in `user32.dll`, called with the arguments the
documentation lists, in the order it lists them.

## What is in it

Five files, split the way the API is:

| File | Library | What it binds |
| --- | --- | --- |
| `user32.kdecl` | `user32.dll` | windows, messages, dialogs, input, hooks, menus, the clipboard |
| `gdi32.kdecl` | `gdi32.dll` | device contexts, pens, brushes, fonts, bitmaps, regions, drawing |
| `kernel32_proc.kdecl` | `kernel32.dll` | processes, threads, tool-help snapshots, synchronisation |
| `kernel32_mem.kdecl` | `kernel32.dll` | virtual memory, heaps, modules, files, mappings |
| `advapi32.kdecl` | `advapi32.dll` | the registry, access tokens, privileges |

The split is for reading, not for using: every file in a kit directory is
merged into one bundle, so `use win` is the whole of asking for all five. A
name belongs to exactly one file — `RECT` is declared once, in `gdi32.kdecl`, and
`user32.kdecl` uses it without redeclaring it.

`kiln commands --use win` lists the lot, which is also where Studio's
completion and the language server get it from:

```sh
kiln commands --use win | grep CreateWindow
```

## Platform gating

The kit's `lib.json` says `"platforms": ["windows"]`, so a program that uses it
builds for Windows and refuses anything else, by name, before the linker is
reached:

```text
kiln: kit `win` supports windows — it cannot be built for linux.
Build with `--os windows`.
```

Listing the kit still works everywhere. `kiln commands --use win` answers on
Linux, so completion and the reference are available on a machine that cannot
build for Windows — which, since the toolchain runs on Linux and cross-builds,
is the machine most of this gets written on.

```sh
kiln build app.kiln --os windows -o app.exe
```

## The two examples

`examples/win/` holds six programs. None of them contains a `dll` or a `record`
line: if the kit were short of a declaration they need, they would not build.
(They do declare a few of their own `const`s — a window class name, a frame
budget — which is a program naming its own values, not a gap in the kit.) Two
are worth reading first.

### A window

`examples/win/window.kiln` is the program every Win32 book opens with — register
a class, create a window, pump messages, handle them in a window procedure —
and it is the one that proves the callback direction works.

The WNDPROC is a subroutine, and the class carries its address:

```text
sub wndproc(hwnd: ptr, msg: int, wparam: int64, lparam: int64): int64 system
  if msg = WM_PAINT
    var ps: PAINTSTRUCT
    let dc: ptr = BeginPaint(hwnd, ps)
    call TextOutA(dc, 10, 10, "Built with Kiln.", 19)
    call EndPaint(hwnd, ps)
    return int_to_int64(0)
  end
  if msg = WM_DESTROY
    call PostQuitMessage(0)
    return int_to_int64(0)
  end
  return DefWindowProcA(hwnd, msg, wparam, lparam)
end
```

`WPARAM`, `LPARAM` and `LRESULT` are pointer-width, so the kit spells them
`intptr` — 64-bit on a 64-bit target, 32-bit on `--arch x86`; the
message is a `UINT`, so it is an `int`; the `HWND` is a `ptr`. `address of
wndproc` is the function pointer the class stores, and Windows calls it — not
Kiln.

The class is a `WNDCLASSEXA`, a c-record that starts zeroed, which is exactly
what the API wants of every field a program does not set:

```text
var wc: WNDCLASSEXA
wc.cb_size = int64_to_int(size of WNDCLASSEXA)
wc.style = CS_HREDRAW bor CS_VREDRAW             # redraw on either resize
wc.wnd_proc = address of wndproc
wc.instance = GetModuleHandleNull(ptr_null())
wc.cursor = LoadCursorA(ptr_null(), ptr_from_int(int_to_int64(IDC_ARROW)))
wc.background = ptr_from_int(int_to_int64(COLOR_WINDOW + 1))
wc.class_name = "KilnWindowClass"
let atom: int = RegisterClassExA(wc)
```

The example ends by itself: it pumps with `PeekMessageA` rather than blocking
in `GetMessageA`, destroys its own window after a fixed number of turns, and
leaves through the `WM_QUIT` that `PostQuitMessage` posts — the real exit path,
so a test can run it to completion.

### Reading a process's memory

`examples/win/meminfo.kiln` is a console program, and it is the one that proves
the process and memory halves are bound to the real thing. It opens itself with
`OpenProcess`, allocates a page with `VirtualAlloc`, writes a known value into
it, and then reads that value back *through kernel32* rather than off the
pointer:

```text
let access: int = PROCESS_VM_READ bor PROCESS_VM_WRITE bor PROCESS_QUERY_INFORMATION
let process: ptr = OpenProcess(access, false, pid)
let read_ok: bool = ReadProcessMemory(process, page, into, 4, moved)
```

`lpNumberOfBytesRead` is a `SIZE_T *`, so `moved` is an eight-byte cell read
back with `ptr_read_int64` — four would overwrite the byte after it. Then
`VirtualQuery` fills a `MEMORY_BASIC_INFORMATION` and the program checks the
state and the protection Windows reports for the page it just made, which is
the first thing a wrong struct layout gets wrong.

The other four: `registry.kiln` creates a key under `HKEY_CURRENT_USER`, writes
a `REG_DWORD` and a `REG_SZ`, reads both back and deletes the key again;
`spawn.kiln` starts a thread whose ThreadProc is a Kiln subroutine and a
child process through the `STARTUPINFOA` / `PROCESS_INFORMATION` pair;
`flags.kiln` calls an address `GetProcAddress` handed back and reads the kit's
constants a bit at a time; `msgbox.kiln` is the four-line one at the top of this
page.

## How the declarations are spelled

A transcription has to decide how each Win32 type crosses, and the kit decides
it the same way everywhere.

| Win32 | In the kit | Why |
| --- | --- | --- |
| `HWND`, `HANDLE`, `HDC`, `HKEY`, `HMODULE`, any pointer | `ptr` | all pointer-width |
| `DWORD`, `UINT`, `LONG`, `int`, `ATOM` | `int` | 32 bits |
| `BOOL` | `bool` | a C `int`; any non-zero is true |
| `WPARAM`, `LPARAM`, `LRESULT`, `SIZE_T`, `ULONG_PTR` | `intptr` | pointer-width: `int64` on x64, `int` on x86 |
| `LPCSTR` the API always wants | `text` | crosses as the `char *` it is |
| `LPCSTR`/`LPSTR` that is routinely `NULL`, or an out buffer | `ptr` | a `text` has no `NULL` |
| a struct the callee fills | the c-record itself | the pointer is taken for you |
| `WORD` inside a struct | `word` | a 16-bit field, read as an `int` `0`..`65535` |

Two consequences worth knowing before writing against it.

**There is no `NULL` for a `text`.** Where a Win32 function is normally called
with a null string, the kit binds a `ptr`-taking sibling under a second name
with `as` pointing at the same export — `GetModuleHandleNull(ptr_null())` is
`GetModuleHandleA(NULL)`. Where both a string and `NULL` are ordinary, the
parameter is a `ptr` and a program passes `ptr_of_text(...)`.

**A struct parameter is the record, not its address.** `RegisterClassExA(wc)`
hands C `&wc`: a `dll` parameter typed as a c-record passes the record's
pointer automatically. Where the declaration says `ptr` instead — because
`NULL` is a normal argument there — pass `address of` the record yourself.

## `...A`, not `...W`

Every entry point that takes or answers a string is bound under its ANSI name:
`MessageBoxA`, `CreateWindowExA`, `RegQueryValueExA`. That is not a shortcut —
it is the only spelling that works. A Kiln `text` is a NUL-terminated byte
string, which is exactly the `char *` an `...A` entry point takes. The `...W`
entries take UTF-16, and there is no `text` that is UTF-16, so binding them
would hand Windows bytes it would read as the wrong encoding.

The practical cost is characters outside the process's ANSI code page: a window
title or a registry value in Japanese, on a machine whose code page is not
Japanese, will not survive the round trip. A UTF-16 text type is what would fix
it, and the kit is written so the `...W` half can be added beside the `...A`
half rather than instead of it.

## Constants are still spelled in decimal

The kit was transcribed before the language had a hexadecimal literal, so every
constant in the `.kdecl` files is written as the decimal number it is, with the
hex a C header would show in the comment beside it:

```text
const PAGE_READWRITE = 4                   # 0x04
const MEM_COMMIT = 4096                    # 0x00001000
const WS_OVERLAPPEDWINDOW = 13565952       # 0x00CF0000
```

Those are the same numbers either way, so nothing is wrong — the spelling is
simply older than the language. **A program that uses the kit is under no such
constraint**: `0x00CF_0000` is a number like any other, and flags combine and
are tested with the [bitwise
operators](./language.md#bitwise-operators-and-hex-literals).

```text
var style: int = WS_VISIBLE bor WS_POPUP        # combine
if style band WS_BORDER <> 0                    # test one bit
var low: int64 = wparam band 0xFFFF             # LOWORD
```

`examples/win/flags.kiln` does that against the kit itself and checks every
answer: `MEM_COMMIT bor MEM_RESERVE` is shown to be the same word as the
pre-combined `MEM_COMMIT_RESERVE`, `VirtualAlloc` and `OpenProcess` are handed
words built with `bor` rather than pre-combined ones, and
`WS_OVERLAPPEDWINDOW` is asked which of its bits are set.

The one place the old spelling shows through is a constant above `0x7FFF_FFFF`.
Written as decimal `2147483648` it is a *number*, so it types `int64`;
written as `0x8000_0000` it is a *bit pattern*, so it is an `int` on its own
and an `int64` where one is wanted. The `HKEY_*` constants in `advapi32.kdecl`
are the decimal kind, and `RegOpenKeyExA` takes their `ptr` through
`ptr_from_int`, which wants an `int64` — so they work as written. Rewriting one
to hex changes its bare type, which is a thing to do deliberately rather than
by search and replace.

## What it does not reach

- **A struct with a union or a bitfield** has no c-record. `BITMAPFILEHEADER`
  is `#pragma pack(2)` — 14 bytes where natural alignment gives 16 — so it is
  deliberately absent rather than present and wrong. Lay those out by hand with
  `mem_alloc` and `ptr_write_*` at counted offsets.
- **`...W` entry points**, for the reason above.
- **COM, as declarations.** The mechanism is there —
  [`call through`](./interop.md#calling-a-function-pointer) calls the function
  pointer a vtable slot holds, which is what every COM method call is — but
  the kit binds nothing for it. `ole32` is absent, so `CoInitializeEx` and
  `CoCreateInstance` are not declared, and `IUnknown`, the `HRESULT`
  conventions and the `this` argument are written out by hand.
- **A GUI-subsystem image.** A program written against `use win` alone builds
  for the console subsystem, so on a real Windows desktop it has a console
  window beside the one it made. `--target gui` is Kiln's own UI stack
  rather than a subsystem switch, and it refuses a module with no `form`, so
  there is currently no way to ask for the GUI subsystem and nothing else.
- **Structured exception handling**, `__try`/`__except`: there is no way to
  install a handler frame from Kiln.
- **Four libraries, and no more.** The kit is user32, gdi32, kernel32 and
  advapi32. Not in it: `comctl32` (the common controls — list views, tree
  views, `InitCommonControlsEx`), `comdlg32` (`GetOpenFileNameA` and the rest
  of the common dialogs), `shell32` (`ShellExecuteA`, the known folders),
  `psapi` (`EnumProcessModules`), `ws2_32` (sockets — Kiln's own `net` kit
  is the portable answer), `winmm`, `ole32`, the CryptoAPI, and the service
  control manager. Also absent from kernel32 itself: the console API
  (`GetStdHandle`, `WriteConsoleA`, `AllocConsole`), the high-resolution
  timers (`QueryPerformanceCounter`), and the debug loop
  (`WaitForDebugEvent`, `GetThreadContext` — `CONTEXT` is 1,232 bytes of
  unions and 16-byte alignment, and has no c-record).

  A `dll` line written by hand still reaches every one of those: the kit is a
  convenience, not a wall. `use win` plus a couple of local declarations is
  the normal way to use a library the kit has not covered yet.

## How it is tested

Every declaration in the kit is checked against real Windows rather than
against a header: the examples are cross-built with mingw and run under wine,
where a wrong struct offset, a `DWORD` declared as an `int64`, or an entry point
spelled the way the documentation prints it rather than the way the DLL exports
it all fail immediately — none of which a build catches.

wine runs with the display turned off, so nothing reaches the screen. A window
is still created and its messages are still delivered — `window.kiln`'s WNDPROC
is called back with `WM_PAINT` and `WM_DESTROY` under test, and that is checked
— but there is no framebuffer to read, so `TextOutA` is proved to have been
called and to have returned, not to have drawn the right pixels. A drawn
Windows window has not been looked at.

```sh
cargo test --release --test win_kit
```
