# Interop

Kiln talks to C in both directions: a program calls into native libraries,
and native code calls back into it. A shared library Kiln builds can go one
step further and carry a loader hook — a real `DllMain` on Windows, an ELF
constructor on Linux — so it runs the instant it is mapped into a process. This
page collects the pieces that reach across that boundary, starting with the raw
pointer everything else is built on and ending with a library that hooks a
function the moment it loads.

## Pointers and memory

A `ptr` holds a raw machine address: a 64-bit number that points at some bytes.
It is how a program hands a buffer, a struct, a handle or an out-parameter to a
C API, and how it reads one back. A `ptr` is deliberately opaque — it has no
length, no ownership, and no automatic conversion to or from `int64`. You move
bytes through it explicitly, or you do not move them at all.

`ptr_null()` is the zero address, and `ptr_is_null` tests for it:

```kiln
module nullcheck

sub main
  var p: ptr = ptr_null()
  if ptr_is_null(p)
    call print_text("nothing here yet")
  end
end
```

### Allocating a buffer

`mem_alloc` returns a block of raw bytes you own; `mem_free` releases it. These
are `malloc` and `free` — the block is yours to free, it is not swept up when
the program ends, and it is the block a C API can safely `free` or `realloc`
itself. `mem_zero` and `mem_copy` are `memset` and `memcpy`.

Write typed values into the block at a byte offset and read them back. Offsets
and sizes are `int64`, so a buffer may be larger than two gigabytes:

```kiln
module buffer

sub main
  var buf: ptr = mem_alloc(16)
  call mem_zero(buf, 16)

  call ptr_write_int(buf, 0, 42)
  call ptr_write_int64(buf, 8, 9000000000)

  call print_int(ptr_read_int(buf, 0))
  call print_int64(ptr_read_int64(buf, 8))

  call mem_free(buf)
end
```

The read and write families cover the widths a C API expects:

| Read | Write | Moves |
|------|-------|-------|
| `ptr_read_int` | `ptr_write_int` | a 32-bit int |
| `ptr_read_int64` | `ptr_write_int64` | a 64-bit int |
| `ptr_read_byte` | `ptr_write_byte` | one byte, as an int `0`..`255` |
| `ptr_read_double` | `ptr_write_double` | a 64-bit float |
| `ptr_read_ptr` | `ptr_write_ptr` | one pointer — for a pointer-to-pointer |

`ptr_offset(p, bytes)` makes a new pointer that many bytes past `p`, for when
it reads better than passing an offset to every call.

### Strings across the boundary

`ptr_of_text` hands back the `char*` behind a text, to pass to a C function that
takes a string. It is borrowed: valid only while that text is, and pointing at
read-only bytes when the text is a literal. `ptr_read_text` does the reverse,
copying a NUL-terminated C string at an address into an Kiln text you own:

```kiln
module strings

sub main
  let greeting: text = "hello, C"
  var borrowed: ptr = ptr_of_text(greeting)
  call print_text(ptr_read_text(borrowed))

  var owned: ptr = mem_alloc(32)
  call ptr_write_text(owned, 0, "Kiln")
  call print_text(ptr_read_text(owned))
  call mem_free(owned)
end
```

`ptr_read_text` is the one read that is defined at a null address: it answers
the empty text rather than dereferencing, because the result is copied out
anyway. Every other read at a bad address faults, exactly as the same C would.

### The escape hatch

When you already hold an address as a number — one a C API returned through an
out-parameter, say — `ptr_from_int` turns it into a `ptr`, and `ptr_to_int`
turns a `ptr` back into an `int64`. This is the only bridge between the two, and
it is spelled out on purpose: an address is not an integer you can accidentally
do arithmetic on.

```kiln
module escape

sub main
  var block: ptr = mem_alloc(8)
  call ptr_write_int(block, 0, 7)

  var address: int64 = ptr_to_int(block)
  var same: ptr = ptr_from_int(address)

  call print_int(ptr_read_int(same, 0))
  call mem_free(block)
end
```

Two `ptr` values that hold the same address compare equal with `=`, so
`same = block` above is true.

## Calling a DLL

A `dll` declaration names a function that lives in a shared library — a `.dll`
on Windows, a `.so` on Linux, a `.dylib` on macOS — and makes it callable like
any subroutine. It is how a program reaches the Win32 API, a Lua C API, Detours,
or any C export the language does not wrap.

The declaration sits at module level, beside `sub`:

```kiln
module messagebox

dll MessageBoxA(handle: ptr, text: text, caption: text, kind: int): int from "user32"

sub main
  let clicked: int = MessageBoxA(ptr_null(), "Built with Kiln.", "Hello", 0)
  call print_int(clicked)
end
```

`MessageBoxA` is now called exactly as a subroutine is — `call MessageBoxA(...)`
for its effect, or `MessageBoxA(...)` in an expression for its result — and the
call is checked for arity and type at build time.

### The shape of a declaration

```text
dll NAME(param: type, ...): return-type from "library" as "symbol" convention
```

- The parameter list and the `: return-type` are the same as a subroutine's. A
  `dll` with no return type is call-only, like a `sub` without one.
- `from "library"` says where the symbol lives. A bare name is decorated for the
  platform — `from "mathdll"` looks for `libmathdll.so` (then `mathdll.so`) on
  Linux, `mathdll.dll` on Windows, `libmathdll.dylib` on macOS — and is searched
  beside the program first, then on the system path. A name with an extension or
  a slash (`"./plugins/audio.so"`, `"user32.dll"`) is used exactly as written.
- `as "symbol"` overrides the exported name, for when the symbol a library
  exports is not what the program wants to call it by. It is optional; without
  it the declaration name is the symbol name.
- `convention` — one of `stdcall`, `cdecl` or `system`, last on the line — names
  the C calling convention. It is optional and, on every target Kiln builds,
  a no-op; [Calling conventions](#calling-conventions) below says why, and why
  `system` is the one to write on a Win32 declaration anyway.

The types that cross the boundary are `int`, `int64`, `double`, `bool`, `text`
and `ptr`. A `text` is passed as a C `char *` and a returned `char *` is copied
into a managed text; a `ptr` is passed straight through. A parameter may also be
a [C-struct record](#c-struct-records), which passes a pointer to a real struct
— the way a C API that takes a `RECT *` or a `MSG *` is reached. An Kiln
array or dictionary, and a plain (non-`is c`) record, are runtime-owned objects
with no by-value C shape — pass a `ptr` to bytes you laid out instead.

### A worked example

Given a small C library `mathdll` with `int add_ints(int, int)`,
`const char *banner(void)`, `void bump(int *)` and `int times_ten(int)`, a
program calls each the way its signature reads. `bump` takes a pointer the callee
writes through, so the program hands it a `ptr` to a cell it allocated:

```kiln
module mathdll

dll add_ints(a: int, b: int): int from "mathdll"
dll banner(): text from "mathdll"
dll bump(cell: ptr) from "mathdll"
dll tentimes(x: int): int from "mathdll" as "times_ten"

sub main
  call print_int(add_ints(40, 2))
  call print_text(banner())

  let cell: ptr = mem_alloc(4)
  call ptr_write_int(cell, 0, 41)
  call bump(cell)
  call print_int(ptr_read_int(cell, 0))
  call mem_free(cell)

  call print_int(tentimes(5))
end
```

### Loading is lazy

A library is opened at the first call to one of its functions, not at start-up.
A program may declare a `dll` from a library that is not present and still build
and run — it reaches out only when it actually makes the call. When that call
comes and the library or the symbol cannot be found, the program stops with a
message naming both, so a foreign call that cannot be made is a visible failure
rather than a silent zero.

## Calling a function pointer

A `dll` line names a symbol the linker resolves before the program ever runs.
Some addresses are not known then. A plug-in is opened while the program is
running and asked for its entry points by name; a COM object hands back a table
of pointers and every method is one slot in it; a C library takes a callback and
gives one back. In each case what the program is holding is a `ptr` — and
`call through` is how it calls one:

```text
call through <ptr-expression>(args...) [: return-type] [convention]
```

The call site *is* the declaration. There is no name to look up and no `dll`
line to check against, so the parentheses and the `: type` supply the C
signature that a declaration would otherwise have carried: the argument types
are whatever the argument expressions are, and `: type` is what C returns.
Leave the `: type` off and it is a C `void` call, which is a statement:

```text
let sum: int = call through add(10, 20): int    # a value
call through log_line(message)                  # an effect
```

Everything else is the `dll` path, unchanged. The types that may cross are the
same six — `int`, `int64`, `double`, `bool`, `text`, `ptr` — plus a
[c-record](#c-struct-records), which passes as a pointer to its flat storage
exactly as a c-record `dll` parameter does. A `text` argument marshals to the
`char *` behind it, a returned `char *` is copied into a managed text, and a
returned C `int` typed `bool` normalises to `true`/`false`. A trailing
`stdcall`, `cdecl` or `system` is accepted and means what it means on a `dll`
line — see [Calling conventions](#calling-conventions).

The callee must be a `ptr`. A number holding an address is not one: put it
through `ptr_from_int` first, which is the same explicitness `ptr` asks for
everywhere else.

### A plug-in, loaded and called

The POSIX loader is itself a C library, so it is reached with `dll` lines and
nothing else is needed. Given a plug-in `libplug.so` built from

```c
int add(int a, int b) { return a + b; }
const char *name(void) { return "plug"; }
void bump(int *cell) { *cell += 1; }
```

a program opens it, asks for three addresses, and calls all three. Not one of
them is declared anywhere:

```kiln
module plugin

dll dlopen(path: text, mode: int): ptr from "libdl.so.2"
dll dlsym(handle: ptr, symbol: text): ptr from "libdl.so.2"
dll dlclose(handle: ptr): int from "libdl.so.2"

const RTLD_NOW = 2

sub main
  let lib: ptr = dlopen("./libplug.so", RTLD_NOW)
  if ptr_is_null(lib)
    call print_text("no plug-in today")
    return
  end

  let add_fn: ptr = dlsym(lib, "add")
  call print_int(call through add_fn(5, 10): int)      # 15

  let name_fn: ptr = dlsym(lib, "name")
  call print_text(call through name_fn(): text)        # plug

  # No return type, so it is a statement: the plug-in writes through the cell.
  let cell: ptr = mem_alloc(4)
  call ptr_write_int(cell, 0, 41)
  call through (dlsym(lib, "bump"))(cell)
  call print_int(ptr_read_int(cell, 0))                # 42
  call mem_free(cell)

  call print_int(dlclose(lib))
end
```

On Windows the two loader calls come from the [`win` kit](./win-kit.md) instead,
and nothing else about the program changes:

```text
module plugin
use win

sub main
  let lib: ptr = LoadLibraryA("plug.dll")
  let add_fn: ptr = GetProcAddress(lib, "add")
  call print_int(call through add_fn(5, 10): int system)
  if FreeLibrary(lib)
    call print_text("unloaded")
  end
end
```

### Parentheses around the callee

`call through (dlsym(lib, "bump"))(cell)` above has the callee in parentheses
because a parenthesis straight after the callee is the argument list. A bare
name — and a field or an array slot reached from one — needs none. Anything
with a call in it does:

```text
call through fp(1, 2): int                        # a variable
call through api.draw(1, 2): int                  # a c-record field
call through vt.fn[3](obj): int                   # a slot of an inline ptr[4]
call through (ptr_read_ptr(vtable, 24))(obj)      # any expression
```

A table of function pointers lives in a c-record's inline `ptr[N]`, as `vt.fn`
above does — an Kiln list holds no `ptr`, so that and `ptr_read_ptr` at a
counted offset are the two ways to index one.

That last line is the whole of a COM method call. A COM object is a pointer to a
pointer to a table of function pointers, so the method at slot 4 is
`ptr_read_ptr` twice and a `call through` with the object as the first argument
— the `this` C++ passes invisibly and Kiln passes by hand.

### What is not checked

An indirect call is checked for the two things that can be known: the callee is
a `ptr`, and every argument has a shape C can be handed. Nothing checks the
signature *against the function*, because at the point of the call there is no
function to check against — only an address. Get the argument count, a width, or
the return type wrong and the result is the same as getting a C prototype wrong:
whatever the machine does. Write the site against the header the export came
from.

A widthless literal is the one to watch. An argument's type comes from the
expression, so `0` is a 32-bit `int`; a C parameter that is 64 bits wants
`int_to_int64(0)` — or a `ptr_null()` where the parameter is a pointer.

### Flags, masks and hex

A C header's constants are hexadecimal and its flags are combined with `|`.
Both are written directly: `0x8000_0000` is a literal, and `band`, `bor`,
`bxor`, `bnot`, `shl`, `shr` and `ushr` are the operators. A bit pattern takes
the width of what it meets, so one constant serves an `int` mask and an
`int64` parameter alike. See [Bitwise operators and hex
literals](./language.md#bitwise-operators-and-hex-literals).

```text
const WS_VISIBLE = 0x1000_0000
const WS_POPUP   = 0x8000_0000

var style: int = WS_VISIBLE bor WS_POPUP
if style band WS_VISIBLE <> 0
  call print_text("visible")
end
var low: int64 = wparam band 0xFFFF         # LOWORD
var high: int64 = wparam ushr 16 band 0xFFFF
```

The two belong together, because neither does much alone: a bit decides which
address to call, and the flag word handed to the function at that address is
built by combining constants. Two runnable programs put both to work and check
themselves as they go.
[`examples/dll/dispatch.kiln`](https://github.com/axdsan/kiln/tree/main/examples/dll)
opens a plug-in it never declares, fetches five same-shaped exports into a
`ptr[5]`, lets a request word choose which of them to call, and holds C's `&`,
`|`, `^`, `<<` and `>>` against Kiln's own operators.
[`examples/win/flags.kiln`](https://github.com/axdsan/kiln/tree/main/examples/win)
asks `GetProcAddress` for `GetCurrentProcessId` and calls the address it gets,
checking the answer against the same function reached as a declared import —
one process has one id — and hands `VirtualAlloc` and `OpenProcess` the words
`MEM_COMMIT bor MEM_RESERVE` and
`PROCESS_QUERY_INFORMATION bor PROCESS_VM_READ`.

## C-struct records

Most real C APIs do not take a handful of scalars — they take a *struct*. A
`RECT`, a `POINT`, a `MSG`, a `STARTUPINFO`: a block of bytes with a fixed
layout the callee reads and writes by field. A `record` marked `is c` is exactly
that block. It has a C memory layout — natural alignment, the padding a C
compiler inserts — so a `dll` can be handed a pointer to a real struct instead
of a buffer packed by hand with `ptr_write_int` at offsets counted by eye.

```text
record NAME is c
  field: type
  ...
end
```

The `is c` after the name is the whole of the marker. Without it a `record` is
unchanged: a reference to a runtime-owned object, built with `NAME(field: ...)`
and passed by handle. With it the record is a **value** whose storage is a flat
struct on the stack, and the two do not mix — a c-record is for crossing to C,
a plain record for everything else.

A c-record field is one of the C-representable scalars — `int`, `int64`,
`double`, `bool`, `text` (a `char *`), `ptr` — or one of the widths that mean
something only in a layout, or another c-record, or a fixed array. The whole
set, which is what a transcription of a C header is written against:

| Field type | In C | Reads and writes as |
|---|---|---|
| `int` | `int32_t` | `int` |
| `int64` | `int64_t` | `int64` |
| `int16`, `word` | `uint16_t` (a Win32 `WORD`) | `int`, `0`..`65535` |
| `byte` | `uint8_t` | `int`, `0`..`255` |
| `double` | `double` | `double` |
| `float` | `float` | `double` |
| `bool` | `int32_t` (a Win32 `BOOL`) | `bool` |
| `text` | `char *` | `text` |
| `ptr` | `void *` | `ptr` |
| another `is c` record | that struct, by value | its own fields |
| `T[N]` | `T a[N]` | one element at a time |

A `bool` occupies a C `int` (four bytes), matching how a C API declares a `BOOL`
field. `int16` and `word` are two spellings of one 16-bit field — write whichever
reads better beside the header being transcribed.

### A struct across the boundary

A c-record is declared as a local with `var` and starts zeroed; its fields are
read and written by name; `address of` it is the pointer a C API expects; and
`size of` it is the struct's `sizeof`, a compile-time constant. Given a C
library `geo` with a `Point` and a function that moves one:

```c
typedef struct { int x; int y; } Point;
void move_point(Point *p, int dx, int dy) { p->x += dx; p->y += dy; }
```

the Kiln side declares the record with the same layout and the function with
a `Point` parameter — a c-record parameter means the C prototype takes a pointer
to that struct, and the record is passed as that pointer automatically:

```kiln
module geometry

record Point is c
  x: int
  y: int
end

dll move_point(p: Point, dx: int, dy: int) from "geo"

sub main
  var here: Point          # a zeroed c-record local
  here.x = 3
  here.y = 4
  call move_point(here, 10, 20)   # C mutates the struct through the pointer
  call print_int(here.x)          # 13
  call print_int(here.y)          # 24

  let bytes: int64 = size of Point
  call print_int64(bytes)         # 8 — two ints, no padding
end
```

`move_point` writes through the pointer, and the change is visible on the next
line: `here` is one struct, in one place, that Kiln and C both hold.

### Passing the pointer two ways

A `dll` parameter typed as the c-record — `move_point(p: Point, ...)` above —
takes the pointer for you. The other way is to type the parameter `ptr` and pass
`address of` the record yourself, which is what a Win32 signature reads like when
the API is declared in terms of the handle and the out-struct:

```kiln
module window
target sharedlib

record RECT is c
  left: int
  top: int
  right: int
  bottom: int
end

dll GetWindowRect(window: ptr, rect: ptr): bool from "user32"

sub width_of(window: ptr): int
  var box: RECT
  let ok: bool = GetWindowRect(window, address of box)
  if not ok
    return 0
  end
  return box.right - box.left
end
```

Both forms hand C the same address — the flat storage of the record. Use the
typed parameter when you write the declaration, the `address of` form when the
declaration is a transcription of a C header that already says `ptr`.

### Layout, padding, and `size of`

The layout is the target's C ABI — the same on x86-64 Linux and x64 Windows for
these scalar fields. A field sits at the next offset aligned to its own width,
and the struct is padded at the end to its widest member, so a record of a
`byte`, an `int`, a `byte` and an `int64`:

```kiln
module layout

record Mixed is c
  a: byte
  b: int
  c: byte
  d: int64
end

sub main
  call print_int64(size of Mixed)   # 24: a@0, b@4, c@8, d@16, tail-padded
end
```

is 24 bytes, not 14 — `b` is pushed to offset 4, `d` to offset 16, exactly as C
lays it out. `size of Mixed` reports that number, and it is the number to pass to
`mem_alloc` or to a C API that wants the size of the struct it is being given. A
`byte` field reads back as an `int` in `0`..`255`, the same convention
`ptr_read_byte` uses; storing an `int` into one keeps its low byte.

A `text` field is a `char *`. Reading one copies the C string into a managed
text you own — a NULL field reads as the empty text — so the value outlives the
struct. Writing one stores the borrowed pointer behind an Kiln text: it is
valid only while that text is, the same bargain `ptr_of_text` makes, so keep the
text alive as long as the struct is in use.

### Narrow numbers: `int16`, `word` and `float`

A C struct is full of members narrower than the language's own numbers. A Win32
`WNDCLASSEXA` has two `WORD`s in the middle of it; a `STARTUPINFOA` has three.
A c-record spells that width `int16`, or `word` where the header being
transcribed says `WORD` — one type under two names, so a declaration can read
like the C it came from.

A `word` field is *read and written as an `int`*: reading widens **unsigned**,
so `0xFFFF` in the struct is `65535` and not `-1`, and writing keeps the low 16
bits. That is the same bargain `byte` makes, and it is the right one because the
field this exists for is a `WORD`, not a `SHORT`.

`float` is the same idea one type over. Kiln has a single floating type,
`double`, so a `float` field is read and written as a `double` and the narrowing
happens at the store: what sits in the struct is a real 4-byte IEEE `float`,
which is what a C API that declares one reads back.

```kiln
module widths

record WndClass is c
  style: int
  cls_extra: int16
  wnd_extra: word
  name: text
end

record Sample is c
  gain: float
end

sub main
  var wc: WndClass
  wc.cls_extra = 65535
  call print_int(wc.cls_extra)      # 65535 — widened unsigned
  call print_int64(size of WndClass) # 16: style@0, the two WORDs@4 and @6, name@8

  var s: Sample
  s.gain = 1.5
  call print_double(s.gain)         # 1.5, stored as a 4-byte float
  call print_int64(size of Sample)  # 4
end
```

### A record inside a record

A C struct holds another *by value* all the time — a `MSG` ends with a `POINT`,
a `PAINTSTRUCT` holds a `RECT`. A c-record field whose type is another `is c`
record is exactly that: the nested struct is laid inline, at its own alignment,
and it costs the outer struct its bytes and nothing else. There is no pointer,
and no second object.

Reach through it with another `.`, as deep as the nesting goes, and take
`address of` it for the pointer to the nested struct alone:

```kiln
module nested

record Point is c
  x: int
  y: int
end

record Msg is c
  hwnd: ptr
  message: int
  wparam: int64
  lparam: int64
  time: int
  pt: Point
end

dll GetMessageA(msg: Msg, window: ptr, first: int, last: int): int from "user32" system

sub main
  var msg: Msg
  msg.pt.x = 11          # a nested field is written through the path
  call print_int(msg.pt.x)

  call print_int64(size of Msg)      # 48 — the same number `sizeof(MSG)` is

  var here: ptr = address of msg.pt  # a pointer to the POINT alone
  call print_int64(ptr_to_int(here) - ptr_to_int(address of msg))   # 36
end
```

A nested field is also a value a `dll` can be handed: a parameter declared as
the nested record takes the address of *that member*, not of the whole struct,
so `call ClientToScreen(window, msg.pt)` reaches C exactly as `&msg.pt` would.

The nested type must itself be `is c`. A plain record is a reference to a
runtime-owned object, and a struct cannot hold one of those by value; a field
like that is a build error that says so. A record that contains itself, directly
or through an array of itself, is refused for the same reason it always was —
it would have no size.

Only the *parts* of a nested record are assignable: `msg.pt.x = 11` is a write,
`msg.pt = somewhere` is not, because there is no value on the right that a whole
block of struct could be filled from. Copy the bytes through `address of` and
`mem_copy` when that is what you mean.

### A fixed array inside a record

`rgb: byte[32]` is C's `BYTE rgb[32]`: thirty-two bytes laid end to end inside
the struct, not a pointer to a runtime array. The element type is any c-record
field type — including another `is c` record — and the count is a literal,
because `size of` and every offset after the field are compile-time numbers.

Elements count **from 1**, like everything else in Kiln, so `r.rgb[1]` is the
first byte and `r.rgb[32]` is the last. `address of r.rgb` is a pointer to the
first element, which is where C's own `&r.rgb` points, so a `memset` or a
`memcpy` reaches the member and nothing around it:

```kiln
module inline_array

record Paint is c
  erase: bool
  rgb: byte[32]
end

sub main
  var ps: Paint
  ps.rgb[1] = 200
  ps.rgb[32] = 7
  call print_int(ps.rgb[1])         # 200 — a byte element reads back unsigned
  call print_int64(size of Paint)   # 36: erase@0, rgb@4

  call mem_zero(address of ps.rgb, 32)   # the member, and nothing around it
  call print_int(ps.rgb[1])         # 0
end
```

An index the compiler can see — a literal, or a `const` that stands for one — is
checked when the program is built: `r.rgb[33]` on a `byte[32]` is a compile
error naming the count, and so is `r.rgb[SLOT]` where `SLOT` is 33, because the
count is part of the type and nothing has to run to know it is wrong.

A **computed** index is a plain address calculation with **no bounds check** —
the same bargain every other `ptr` operation makes. An index a loop drives past
the end reads or writes the bytes beside the array, exactly as the equivalent C
does.

`address of` an array field is the first element. For a later one, offset that
pointer by the element width — `ptr_offset(address of r.rgb, 5)` is `&r.rgb[6]`
— which is the same arithmetic the C would do and keeps the one spelling of
`address of` honest about what it names.

The array as a whole is not a value: assign one element at a time, or move the
bytes through `address of`.

## Callbacks: passing a sub to C

`dll` lets a program call C. `address of` is the other direction: it hands C the
address of a subroutine, so C can call back into Kiln. This is what a hook
detour, a `CreateThread` ThreadProc, an `EnumWindows` callback or a Lua C
function all need — "here is my function, you call it".

```text
address of NAME
```

The result is a `ptr`: a real function pointer a C API invokes with the C
calling convention. The named subroutine must have a C-representable signature —
every parameter and the return in `int`, `int64`, `double`, `bool`, `text` or
`ptr`, or no return at all. A sub that takes or returns an array, a dictionary
or a record has no address you can hand across, and `address of` on it is a
compile error that names the sub and the type that does not fit. A `bool` is
C's `int`; a callback shaped like a Win32 `WNDPROC` uses `int64` for the
pointer-width `WPARAM`, `LPARAM` and `LRESULT`.

Given a C library `cb` that calls back through a function pointer:

```c
int apply(int (*fn)(int, int), int a, int b) { return fn(a, b); }
void each(void (*fn)(int), int n) { for (int i = 1; i <= n; i++) fn(i); }
```

a program declares each as a `dll` taking a `ptr` where C takes the function
pointer, and passes `address of` a matching sub:

```kiln
module callbacks

dll apply(fn: ptr, a: int, b: int): int from "cb"
dll each(fn: ptr, n: int) from "cb"

sub summer(a: int, b: int): int
  return a + b
end

sub announce(n: int)
  call print_int(n)
end

sub main
  var add: ptr = address of summer
  call print_int(apply(add, 40, 2))

  call each(address of announce, 3)
end
```

`apply` calls `summer` for its result and the program prints `42`; `each` calls
`announce` with 1, 2 and 3 from inside its own loop, and the program prints that
sequence. C is driving Kiln code in both.

The subroutine runs on whatever thread and stack C calls it from, with C's
calling convention, and it makes no assumption about an event loop — a ThreadProc
handed to `CreateThread` runs on the new thread, and coordinating that with the
rest of the program is the program's own affair. A `text` parameter arrives as
the C `char *` the caller passed, read for the duration of the call; a `text` the
callback returns is storage the Kiln runtime owns and frees.

A callback sub may carry the same optional convention marker a `dll` does —
`sub wndproc(hwnd: ptr, msg: int, wparam: int64, lparam: int64): int64 system` —
to document the convention C calls it with. It changes nothing on any target
today, for the reason [Calling conventions](#calling-conventions) gives next.

## Calling conventions

A C calling convention is the contract for a call: which registers or stack
slots carry the arguments, who pops them afterwards, how the return comes back.
`cdecl` and `stdcall` are two such contracts — on 32-bit x86 they differ in who
cleans the stack, and calling a `stdcall` function as `cdecl` corrupts it. A
`dll` declaration and a callback sub may name one, last on the line:

```kiln
module conventions

dll MessageBoxA(handle: ptr, text: text, caption: text, kind: int): int from "user32" system
dll SetWindowsHookExA(id: int, hook: ptr, hmod: ptr, thread: int): ptr from "user32" system

sub keyboard_hook(code: int, wparam: int64, lparam: int64): int64 system
  return 0
end

sub main
  call print_text("declared with a convention; built the same as without one")
end
```

`system` is the one to reach for. It means *the platform's own convention for
its system APIs* — `stdcall` on 32-bit Windows, `cdecl` everywhere else — so a
Win32 declaration written `from "user32" system` stays correct no matter the
target. `stdcall` and `cdecl` name a specific convention outright, for a library
that documents one.

On every target Kiln builds today the marker is a **no-op**. The three
targets — x86-64 Linux, x64 Windows, 64-bit macOS — are all 64-bit, and a 64-bit
target has a *single* C calling convention: `cdecl`, `stdcall` and `system` all
resolve to the same one, and the compiler emits identical code whether a
declaration names a convention or leaves it off. The marker is accepted, checked
against the set of three, and carried through the toolchain, but it does not
change a single instruction.

It is worth writing regardless, for two reasons. It documents intent — a reader
of `from "user32" system` sees a Win32 call for what it is — and it is the fact a
future 32-bit backend would need, where the conventions diverge and the marker
would decide how the call is made. A declaration transcribed correctly today is
one that keeps working if that target ever lands. An unrecognised word in the
slot — `fastcall`, `pascal`, anything but the three — is a build error that names
it, so a typo is caught rather than silently ignored.

## Declaration kits

A `dll` line, an `is c` record and the `const` a C API is written in terms of
are the same in every program that reaches that API. `use` lets a *kit* carry
them, so a program says `use win` and has `MessageBoxA`, `RECT` and `MB_OK`
without transcribing a single one.

A kit is the directory [Kits](./kits.md) describes. A kit that ships
declarations puts them in a `.kdecl` file beside its `lib.json` — a small kit in
one named for the kit, `<name>.kdecl`. The file is a run of declarations with no
`module` header — `dll`, `record` and `const`, and nothing else. A `sub`, a
`form`, a component or a module variable does not belong in one and is refused,
because a declaration bundle *declares*; it does not define or build.

```text
# win.kdecl — the bundle `use win` brings in
dll MessageBoxA(handle: ptr, text: text, caption: text, kind: int): int from "user32" system
dll GetLastError(): int from "kernel32" system

record RECT is c
  left: int
  top: int
  right: int
  bottom: int
end

const MB_OK = 0
const MB_YESNO = 4
const WM_DESTROY = 2
```

`use win` finds the kit exactly as `use net` does — a `kits/` beside the
project first, then `~/.kiln/kits/`, then the bundled `libs/` — and merges
its declarations into the program as if they had been typed there. From then on
`MessageBoxA` is called like any `dll`, `RECT` is a c-record like any other, and
`MB_OK` is the number `0` everywhere a `0` could go:

```text
module hello
use win

sub main
  let clicked: int = MessageBoxA(ptr_null(), "Built with a kit.", "Hello", MB_YESNO)
  call print_int(clicked)
end
```

A kit can ship declarations, C-implemented commands, or both: a `.kdecl` beside a
`<name>_libinfo.c` contributes to the one registry from both halves.
`kiln commands --use <name>` lists a kit's `dll:`, `crecord:` and `const:`
lines beside its `command:` lines, so Studio's completion and the reference see
them; `kiln kits` reports the bundle a kit carries.

### A bundle across several files

One file per kit stops reading well the moment a kit is large. A Win32 kit wraps
half a dozen system libraries, and a thousand declarations in one `win.kdecl` is a
file nobody can find anything in. So a kit may carry **as many `.kdecl` files as it
likes**, and every one in the kit directory is merged into a single bundle:

```text
kits/win/
  lib.json
  user32.kdecl      # windows, messages, MessageBoxA
  kernel32.kdecl    # handles, modules, GetLastError
  gdi32.kdecl       # drawing
```

Order across files does not matter any more than order within one does: the
merged bundle is registered whole before any cross-reference is checked, so a
`dll` in `user32.kdecl` may take a `RECT` declared in `gdi32.kdecl`. What the files
share is one namespace — `dll`, `record` and `const` names all land in the same
registry — so declaring one name in two files is a kit-authoring error naming
both, caught the moment the kit is used or listed.

`use` is unchanged: a program says `use win` and gets everything from every
file. So is `kiln kits`, which lists the whole merged bundle.

### Constants

A `const` names a literal — an integer, a double, a text or a bool — and stands
for it everywhere a literal is allowed: a `dll` argument, a comparison, a `let`.
It is module-level, and a program writes its own the same way a kit does:

```kiln
module flags

const RETRIES = 3
const GREETING = "ready"

sub main
  var left: int = RETRIES
  while left > 0
    left -= 1
  end
  call print_text(GREETING)
end
```

A constant is a single literal, nothing more — not another constant, not an
expression — so its type is fixed where it is written and a reference to it
costs nothing at run time. An `int` constant folds into an `int64` where one is
wanted, the same widening a bare number gets, so `mem_alloc(SIZE)` reads the way
it should.

### Platform-only kits

A kit that wraps a platform's own API works only on that platform: `MessageBoxA`
lives in a Windows `user32`, and there is no Linux library to resolve it
against. Such a kit says so in its `lib.json`:

```json
{ "display": "Win32", "platforms": ["windows"] }
```

Building a program that `use`s it for another operating system is a compile
error that names the kit and the OS it needs, rather than a wall of linker
errors at the end:

```text
$ kiln build hello.kiln --os linux
kiln: kit `win` supports windows — it cannot be built for linux. Build with `--os windows`.
```

Listing the kit's contents is still allowed anywhere — `kiln commands --use
win` and the language server complete a Win32 declaration on a Linux machine, so
the documentation and the editor work even where a build cannot. A kit with no
`platforms` key is portable and builds everywhere, which is why the bundled
libraries and a kit like `demoffi` — a small C library a program loads through
its own `dll` lines — run on Linux and Windows alike.

## A library the loader runs: DllMain

A `sharedlib` normally has no start-up moment. It exports its subroutines and
waits; a host calls `<module>_init` once to set up module variables (see
[Build targets](./build-targets.md)) and then calls whatever it needs. Nothing
runs on its own, because a library should not run your code before the host is
ready for it.

A library that is *loaded for effect* — injected into a process, or brought in
with `LoadLibrary`/`dlopen` for what it does rather than what it exports — has no
host willing to make that first call. It has to run the instant it is mapped.
Two specially-named subroutines give it that moment:

```text
sub dll_attach   # runs when the library is mapped into a process
sub dll_detach   # runs when it is unmapped
```

Each takes no parameters and returns nothing — the loader calls them with
neither. Define one or both in a `target sharedlib` module and the compiler
wires it to the platform's loader entry:

| | `dll_attach` | `dll_detach` |
| --- | --- | --- |
| **Windows** | `DllMain`, `DLL_PROCESS_ATTACH` | `DllMain`, `DLL_PROCESS_DETACH` |
| **Linux** | `__attribute__((constructor))` | `__attribute__((destructor))` |

`<module>_init` runs first, before `dll_attach`, so any module variable the hook
touches is already set up. A sharedlib that defines neither hook is unchanged:
it gets no loader entry, and the host calls `<module>_init` itself as before.

`dll_attach` runs under the OS loader — on Windows, while the loader lock is
held. That is the right place to install a hook or record that the library
loaded, and the wrong place for anything slow or anything that loads another
library: work heavier than a few assignments belongs on a thread the hook
spawns (through a `dll` to `CreateThread` or `pthread_create`), which runs once
the loader has let go.

### A worked hook

The three pieces above compose into a real, self-contained hook: a library that,
the moment it loads, redirects a function another part of the program is calling
— in the same process, with nothing patched by hand.

The function being hooked lives in a small C library, `hookrt`. It dispatches
every call through a slot, so redirecting the slot redirects every future call;
installing a detour keeps the old target as a trampoline the detour can still
reach:

```c
static int base_target(int x) { return x * 2; }        /* the original */
static int (*slot)(int) = base_target;                 /* what call() dispatches to */
static int (*trampoline)(int) = base_target;           /* the saved original */

int  hookrt_call(int x)     { return slot(x); }        /* the application's call site */
int  hookrt_original(int x) { return trampoline(x); }  /* the detour reaches the original */
void hookrt_install(void *detour) {                    /* redirect the slot */
    trampoline = slot;
    slot = (int (*)(int))detour;
}
```

The Kiln library declares the two functions it needs as `dll`, writes the
detour as an ordinary subroutine, and installs it from `dll_attach` with
`address of`:

```kiln
module hook
target sharedlib

dll hookrt_original(x: int): int from "hookrt"
dll hookrt_install(detour: ptr) from "hookrt"

sub detour(x: int): int
  return hookrt_original(x) + 1
end

sub dll_attach
  call hookrt_install(address of detour)
end
```

A host program links `hookrt` and calls `hookrt_call` directly — it is the
application whose function gets hooked. It calls once, loads the Kiln library,
and calls again:

```c
extern int hookrt_call(int x);

#ifdef _WIN32
#include <windows.h>
static int load_hook(void) { return LoadLibraryA("hook.dll") != NULL; }
#else
#include <dlfcn.h>
static int load_hook(void) { return dlopen("./libhook.so", RTLD_NOW | RTLD_LOCAL) != NULL; }
#endif

int main(void) {
    printf("before %d\n", hookrt_call(10));   /* 20: the original, x*2 */
    load_hook();                              /* fires dll_attach, installs the detour */
    printf("after %d\n", hookrt_call(10));    /* 21: the detour, original + 1 */
    return 0;
}
```

Loading the library is the whole of it: `dll_attach` runs under the loader,
installs the detour into the one loaded `hookrt`, and the host's next call —
made through the same slot — lands in the Kiln `detour`, which reaches the
original through the trampoline and adds one. The program prints `before 20`
then `after 21`, on Linux through the constructor and on Windows through
`DllMain`. The complete, buildable example is in
[`examples/hook/`](https://github.com/axdsan/kiln/tree/main/examples/hook).
