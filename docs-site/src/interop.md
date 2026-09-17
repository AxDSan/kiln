# Interop

Kiln talks to C in both directions: a program calls into native libraries,
and native code calls back into it. A shared library Kiln builds can go one
step further and carry a loader hook — a real `DllMain` on Windows, an ELF
constructor on Linux — so it runs the instant it is mapped into a process. This
page collects the pieces that reach across that boundary, starting with the raw
pointer everything else is built on and ending with a library that hooks a
function the moment it loads.

## Pointers and memory

A `Ptr` holds a raw machine address: a 64-bit number that points at some bytes.
It is how a program hands a buffer, a struct, a handle or an out-parameter to a
C API, and how it reads one back. A `Ptr` is deliberately opaque — it has no
length, no ownership, and no automatic conversion to or from `long`. You move
bytes through it explicitly, or you do not move them at all.

`PtrNull()` is the zero address, and `PtrIsNull` tests for it:

```k2
namespace NullCheck;

public static class P
{
    public static void Main()
    {
        Ptr p = PtrNull();
        if (PtrIsNull(p))
            Console.WriteLine("nothing here yet");
    }
}
```

### Allocating a buffer

`MemAlloc` returns a block of raw bytes you own; `MemFree` releases it. These
are `malloc` and `free` — the block is yours to free, the collector never sweeps
it, and it is the block a C API can safely `free` or `realloc` itself. `MemZero`
and `MemCopy` are `memset` and `memcpy`.

Write typed values into the block at a byte offset and read them back. Offsets
and sizes are `long`, so a buffer may be larger than two gigabytes:

```k2
namespace Buffer;

public static class P
{
    public static void Main()
    {
        Ptr buf = MemAlloc(16);
        MemZero(buf, 16);

        PtrWriteInt(buf, 0, 42);
        PtrWriteInt64(buf, 8, 9000000000);

        Console.WriteLine($"{PtrReadInt(buf, 0)} {PtrReadInt64(buf, 8)}");

        MemFree(buf);
    }
}
```

The read and write families cover the widths a C API expects:

| Read | Write | Moves |
|------|-------|-------|
| `PtrReadInt` | `PtrWriteInt` | a 32-bit int |
| `PtrReadInt64` | `PtrWriteInt64` | a 64-bit int |
| `PtrReadByte` | `PtrWriteByte` | one byte, as an int `0`..`255` |
| `PtrReadDouble` | `PtrWriteDouble` | a 64-bit float |
| `PtrReadPtr` | `PtrWritePtr` | one pointer — for a pointer-to-pointer |

`PtrOffset(p, bytes)` makes a new pointer that many bytes past `p`, for when it
reads better than passing an offset to every call.

### Strings across the boundary

`PtrOfText` hands back the `char *` behind a string, to pass to a C function that
takes one. It is borrowed: valid only while that string is, and pointing at
read-only bytes when the string is a literal. `PtrReadText` does the reverse,
copying a NUL-terminated C string at an address into a Kiln string you own:

```k2
namespace Strings;

public static class P
{
    public static void Main()
    {
        var greeting = "hello, C";
        Ptr borrowed = PtrOfText(greeting);
        Console.WriteLine(PtrReadText(borrowed));

        Ptr owned = MemAlloc(32);
        PtrWriteText(owned, 0, "Kiln");
        Console.WriteLine(PtrReadText(owned));
        MemFree(owned);
    }
}
```

`PtrReadText` is the one read that is defined at a null address: it answers the
empty string rather than dereferencing, because the result is copied out anyway.
Every other read at a bad address faults, exactly as the same C would.

### The escape hatch

When you already hold an address as a number — one a C API returned through an
out-parameter, say — `PtrFromInt` turns it into a `Ptr`, and `PtrToInt` turns a
`Ptr` back into a `long`. This is the only bridge between the two, and it is
spelled out on purpose: an address is not an integer you can accidentally do
arithmetic on.

```k2
namespace Escape;

public static class P
{
    public static void Main()
    {
        Ptr block = MemAlloc(8);
        PtrWriteInt(block, 0, 7);

        long address = PtrToInt(block);
        Ptr same = PtrFromInt(address);

        Console.WriteLine($"{PtrReadInt(same, 0)} {same == block}");
        MemFree(block);
    }
}
```

## Calling a DLL

A `[Dll]` method names a function that lives in a shared library — a `.dll` on
Windows, a `.so` on Linux, a `.dylib` on macOS — and makes it callable like any
method. It is how a program reaches the Win32 API, a Lua C API, Detours, or any
C export the language does not wrap.

```
namespace MessageBox;

public static class P
{
    [Dll("user32", Convention = CallConv.System)]
    public static extern int MessageBoxA(Ptr handle, string text, string caption, int kind);

    public static void Main()
    {
        int clicked = MessageBoxA(PtrNull(), "Built with Kiln.", "Hello", 0);
        Console.WriteLine($"{clicked}");
    }
}
```

`MessageBoxA` is called exactly as a method is, and the call is checked for
arity and type at build time.

### The shape of a declaration

```
[Dll("library", Entry = "symbol", Convention = CallConv.System)]
public static extern ReturnType Name(Type param, ...);
```

- The parameter list and the return type are a method's. A `void` return is
  called for its effect.
- `"library"` says where the symbol lives. A bare name is decorated for the
  platform — `"mathdll"` looks for `libmathdll.so` (then `mathdll.so`) on Linux,
  `mathdll.dll` on Windows, `libmathdll.dylib` on macOS — and is searched beside
  the program first, then on the system path. A name with an extension or a
  slash (`"./plugins/audio.so"`, `"user32.dll"`) is used exactly as written.
- `Entry = "symbol"` is the exported name, when it is not what the program wants
  to call the method. Without it the method's name is the symbol.
- `Convention` names the C calling convention: `CallConv.Cdecl`,
  `CallConv.StdCall` or `CallConv.System`. It is optional;
  [Calling conventions](#calling-conventions) below says when it matters.

The types that cross the boundary are `int`, `long`, `double`, `bool`, `string`
and `Ptr`. A `string` is passed as a C `char *` and a returned `char *` is copied
into a managed string; a `Ptr` is passed straight through. A parameter may also
be a [C-layout record](#c-layout-records), which passes a pointer to a real
struct — the way a C API that takes a `RECT *` or a `MSG *` is reached. A
`List<T>`, a dictionary or a class is a runtime-owned object with no C shape —
pass a `Ptr` to bytes you laid out instead.

### A worked example

Given a small C library `mathdll` with `int add_ints(int, int)`,
`const char *banner(void)`, `void bump(int *)` and `int times_ten(int)`, a
program calls each the way its signature reads. `bump` takes a pointer the callee
writes through, so the program hands it a `Ptr` to a cell it allocated:

```
namespace MathDll;

public static class P
{
    [Dll("mathdll", Entry = "add_ints")]
    public static extern int AddInts(int a, int b);

    [Dll("mathdll", Entry = "banner")]
    public static extern string Banner();

    [Dll("mathdll", Entry = "bump")]
    public static extern void Bump(Ptr cell);

    [Dll("mathdll", Entry = "times_ten")]
    public static extern int TimesTen(int x);

    public static void Main()
    {
        Console.WriteLine($"{AddInts(40, 2)}");
        Console.WriteLine(Banner());

        Ptr cell = MemAlloc(4);
        PtrWriteInt(cell, 0, 41);
        Bump(cell);
        Console.WriteLine($"{PtrReadInt(cell, 0)}");
        MemFree(cell);

        Console.WriteLine($"{TimesTen(5)}");
    }
}
```

`examples/dll/mathdll.kiln` is this program with `long`, `double` and `bool`
crossing as well.

### Loading is lazy

A library is opened at the first call to one of its functions, not at start-up.
A program may declare a function from a library that is not present and still
build and run — it reaches out only when it actually makes the call. When that
call comes and the library or the symbol cannot be found, the program stops with
a message naming both, so a foreign call that cannot be made is a visible failure
rather than a silent zero.

## Calling a function pointer

A `[Dll]` method names a symbol the linker resolves before the program ever
runs. Some addresses are not known then. A plug-in is opened while the program
is running and asked for its entry points by name; a COM object hands back a
table of pointers and every method is one slot in it; a C library takes a
callback and gives one back. In each case what the program is holding is a
`Ptr` — and a function-pointer type is how it calls one:

```
delegate* unmanaged<int, int, int>             // int (*)(int, int)
delegate* unmanaged<string>                    // const char *(*)(void)
delegate* unmanaged<Ptr, void>                 // void (*)(void *)
delegate* unmanaged[Stdcall]<int>              // a stdcall int (*)(void)
```

The last type argument is the return type; the ones before it are the
parameters. A `Ptr` cast to one of these is a value that can be called, stored
in a variable, or held in a C-layout record's field:

```
var add = (delegate* unmanaged<int, int, int>)Dlsym(lib, "add");
Console.WriteLine($"{add(5, 10)}");
```

The cast *is* the declaration. There is no name to look up and nothing to check
it against, so the type supplies the C signature a `[Dll]` method would
otherwise have carried. The types that may cross are the same — `int`, `long`,
`double`, `bool`, `string`, `Ptr` — plus a C-layout record, which passes as a
pointer to its storage. A returned `char *` is copied into a managed string, and
a returned C `int` typed `bool` normalises to `true`/`false`.

### A plug-in, loaded and called

The POSIX loader is itself a C library, so it is reached with `[Dll]` methods and
nothing else is needed. Given a plug-in `libplug.so` built from

```c
int add(int a, int b) { return a + b; }
const char *name(void) { return "plug"; }
void bump(int *cell) { *cell += 1; }
```

a program opens it, asks for three addresses, and calls all three. Not one of
them is declared anywhere:

```
namespace Plugin;

public static class P
{
    [Dll("libdl.so.2", Entry = "dlopen")]
    public static extern Ptr Dlopen(string path, int mode);

    [Dll("libdl.so.2", Entry = "dlsym")]
    public static extern Ptr Dlsym(Ptr handle, string symbol);

    [Dll("libdl.so.2", Entry = "dlclose")]
    public static extern int Dlclose(Ptr handle);

    const int RTLD_NOW = 2;

    public static void Main()
    {
        Ptr lib = Dlopen("./libplug.so", RTLD_NOW);
        if (PtrIsNull(lib))
        {
            Console.WriteLine("no plug-in today");
            return;
        }

        var add = (delegate* unmanaged<int, int, int>)Dlsym(lib, "add");
        Console.WriteLine($"{add(5, 10)}");                  // 15

        var name = (delegate* unmanaged<string>)Dlsym(lib, "name");
        Console.WriteLine(name());                            // plug

        // No return value, so it is a statement: the plug-in writes through the cell.
        Ptr cell = MemAlloc(4);
        PtrWriteInt(cell, 0, 41);
        ((delegate* unmanaged<Ptr, void>)Dlsym(lib, "bump"))(cell);
        Console.WriteLine($"{PtrReadInt(cell, 0)}");          // 42
        MemFree(cell);

        Console.WriteLine($"{Dlclose(lib)}");
    }
}
```

On Windows the two loader calls come from the [`win` kit](./win-kit.md) instead,
and nothing else about the program changes:

```
using Kiln.Win;
...
Ptr lib = LoadLibraryA("plug.dll");
var add = (delegate* unmanaged[Stdcall]<int, int, int>)GetProcAddress(lib, "add");
Console.WriteLine($"{add(5, 10)}");
```

### Tables of pointers

A table of function pointers lives in a C-layout record's inline array, or is
read out of memory with `PtrReadPtr` at a counted offset:

```
[CLayout]
public record Slots(delegate* unmanaged<int, int, int>[4] Fn);

var vt = new Slots();
vt.Fn[4] = add;                                          // positions count from 1
Console.WriteLine($"{vt.Fn[4](2, 3)}");

var method = (delegate* unmanaged<Ptr, int>)PtrReadPtr(vtable, 24);
method(obj);
```

That last pair is the whole of a COM method call. A COM object is a pointer to a
pointer to a table of function pointers, so the method at slot 4 is `PtrReadPtr`
twice and a call with the object as the first argument — the `this` C++ passes
invisibly and Kiln passes by hand. `examples/dll/plug.kiln` and
`examples/dll/dispatch.kiln` run all of this.

### What is not checked

A call through a function pointer is checked for what can be known: every
argument has a shape C can be handed, and matches the type the cast named.
Nothing checks the signature *against the function*, because at the point of the
call there is no function to check against — only an address. Get the argument
count, a width, or the return type wrong and the result is the same as getting a
C prototype wrong: whatever the machine does. Write the cast against the header
the export came from.

### Flags, masks and hex

A C header's constants are hexadecimal and its flags are combined with `|`. Both
are written directly: `0x8000_0000` is a literal, and `|`, `&`, `^`, `~`, `<<`,
`>>` and `>>>` (which shifts zeroes in whatever the sign) are the operators.

```
const int WS_VISIBLE = 0x1000_0000;

int style = WS_VISIBLE | 0x0080_0000;
if ((style & WS_VISIBLE) != 0)
    Console.WriteLine("visible");
long low = wparam & 0xFFFF;                   // LOWORD
long high = (wparam >>> 16) & 0xFFFF;         // HIWORD
```

The two belong together, because neither does much alone: a bit decides which
address to call, and the flag word handed to the function at that address is
built by combining constants.
[`examples/dll/dispatch.kiln`](https://github.com/axdsan/kiln/tree/main/examples/dll)
opens a plug-in it never declares, fetches five same-shaped exports into a
table, lets a request word choose which of them to call, and holds C's `&`, `|`,
`^`, `<<` and `>>` against Kiln's own operators.
[`examples/win/flags.kiln`](https://github.com/axdsan/kiln/tree/main/examples/win)
asks `GetProcAddress` for `GetCurrentProcessId` and calls the address it gets,
checking the answer against the same function reached as a declared import —
one process has one id.

## C-layout records

Most real C APIs do not take a handful of scalars — they take a *struct*. A
`RECT`, a `POINT`, a `MSG`, a `STARTUPINFO`: a block of bytes with a fixed
layout the callee reads and writes by field. A record marked `[CLayout]` is
exactly that block. It has a C memory layout — natural alignment, the padding a C
compiler inserts — so a `[Dll]` function can be handed a pointer to a real struct
instead of a buffer packed by hand at offsets counted by eye.

```
[CLayout]
public record Point(int X, int Y);
```

`[Packed]` is the other layout: no padding at all, which is what a wire format
wants rather than a C API — see [the language guide](./kiln-2.md#reaching-c-and-bytes).

A C-layout record's fields are the C-representable types — `int`, `long`,
`double`, `bool`, `string` (a `char *`), `Ptr` — the widths that mean something
only in a layout, another C-layout record, a fixed array, or a function pointer.
The whole set, which is what a transcription of a C header is written against:

| Field type | In C | Reads and writes as |
|---|---|---|
| `int` / `uint` | `int32_t` / `uint32_t` | itself |
| `long` / `ulong` | `int64_t` / `uint64_t` | itself |
| `short` / `ushort` | `int16_t` / `uint16_t` (a Win32 `WORD`) | itself |
| `sbyte` / `byte` | `int8_t` / `uint8_t` | itself |
| `double` | `double` | `double` |
| `float` | `float` | `float` — a real 4-byte IEEE value |
| `bool` | `int32_t` (a Win32 `BOOL`) | `bool` |
| `string` | `char *` | `string` |
| `Ptr` | `void *` | `Ptr` |
| another `[CLayout]` record | that struct, by value | its own fields |
| `T[N]` | `T a[N]` | one element at a time |
| `delegate* unmanaged<...>` | a function pointer | a callable value |

A `bool` occupies a C `int` (four bytes), matching how a C API declares a `BOOL`
field.

### A struct across the boundary

A C-layout record starts zeroed; its fields are read and written by name; it
converts to a `Ptr` to its first byte where one is wanted; and `Point.Size` is
the struct's `sizeof`, a compile-time constant. Given a C library `geo` with a
`Point` and a function that moves one:

```c
typedef struct { int x; int y; } Point;
void move_point(Point *p, int dx, int dy) { p->x += dx; p->y += dy; }
```

the Kiln side declares the record with the same layout and the function with a
`Point` parameter — a record parameter means the C prototype takes a pointer to
that struct, and the record is passed as that pointer automatically:

```
namespace Geometry;

[CLayout]
public record Point(int X, int Y);

public static class P
{
    [Dll("geo", Entry = "move_point")]
    public static extern void MovePoint(Point p, int dx, int dy);

    public static void Main()
    {
        var here = new Point();         // zeroed
        here.X = 3;
        here.Y = 4;
        MovePoint(here, 10, 20);        // C mutates the struct through the pointer
        Console.WriteLine($"{here.X} {here.Y}");   // 13 24
        Console.WriteLine($"{Point.Size}");         // 8 — two ints, no padding
    }
}
```

`MovePoint` writes through the pointer, and the change is visible on the next
line: `here` is one struct, in one place, that Kiln and C both hold.

### Passing the pointer two ways

A parameter typed as the record — `MovePoint(Point p, ...)` above — takes the
pointer for you. The other way is to type the parameter `Ptr` and pass the
record, which converts: that is what a Win32 signature reads like when it is
transcribed with a pointer, often because `NULL` is also a normal argument.

```
[CLayout]
public record RECT(int Left, int Top, int Right, int Bottom);

[Dll("user32", Convention = CallConv.System)]
public static extern bool GetWindowRect(Ptr window, Ptr rect);

static int WidthOf(Ptr window)
{
    var box = new RECT();
    if (!GetWindowRect(window, box))
        return 0;
    return box.Right - box.Left;
}
```

Both forms hand C the same address — the record's own storage.

### Layout, padding, and `Size`

The layout is the target's C ABI. A field sits at the next offset aligned to its
own width, and the struct is padded at the end to its widest member, so a record
of a `byte`, an `int`, a `byte` and a `long`:

```k2
namespace Layout;

[CLayout]
public record Mixed(byte A, int B, byte C, long D);

public static class P
{
    public static void Main()
    {
        Console.WriteLine($"{Mixed.Size}");              // 24: A@0, B@4, C@8, D@16
        Console.WriteLine($"{Mixed.OffsetOf("D")}");     // 16
    }
}
```

is 24 bytes, not 14 — `B` is pushed to offset 4, `D` to offset 16, exactly as C
lays it out. `Mixed.Size` reports that number, and it is the number to pass to
`MemAlloc` or to a C API that wants the size of the struct it is being given;
`Mixed.OffsetOf("D")` answers one field's offset.

A `string` field is a `char *`. Reading one copies the C string into a managed
string you own — a NULL field reads as the empty string — so the value outlives
the struct. Writing one stores the borrowed pointer behind a Kiln string: it is
valid only while that string is, the same bargain `PtrOfText` makes, so keep the
string alive as long as the struct is in use.

### Narrow numbers

A C struct is full of members narrower than a program's own numbers. A Win32
`WNDCLASSEXA` has two `WORD`s in the middle of it; a `STARTUPINFOA` has three.
They are `ushort` fields, read and written as the 16-bit unsigned numbers they
are, and a `float` field is a real 4-byte `float` — which is what a C API that
declares one reads back.

```k2
namespace Widths;

[CLayout]
public record WndClass(int Style, ushort ClsExtra, ushort WndExtra, string Name);

[CLayout]
public record Sample(float Gain);

public static class P
{
    public static void Main()
    {
        var wc = new WndClass();
        wc.ClsExtra = 65535;
        Console.WriteLine($"{wc.ClsExtra} {WndClass.Size}");   // 65535 16

        var s = new Sample();
        s.Gain = 1.5f;
        Console.WriteLine($"{s.Gain} {Sample.Size}");          // 1.5 4
    }
}
```

### A record inside a record

A C struct holds another *by value* all the time — a `MSG` ends with a `POINT`,
a `PAINTSTRUCT` holds a `RECT`. A field whose type is another `[CLayout]` record
is exactly that: the nested struct is laid inline, at its own alignment, and it
costs the outer struct its bytes and nothing else. There is no pointer, and no
second object.

```k2
namespace Nested;

[CLayout]
public record Point(int X, int Y);

[CLayout]
public record Msg(Ptr Hwnd, int Message, long Wparam, long Lparam, int Time, Point Pt);

public static class P
{
    public static void Main()
    {
        var msg = new Msg();
        msg.Pt.X = 11;                       // a nested field is written through the path
        Console.WriteLine($"{msg.Pt.X}");
        Console.WriteLine($"{Msg.Size}");   // 48 — the same number `sizeof(MSG)` is

        Ptr whole = msg;
        Ptr pt = msg.Pt;                     // a pointer to the POINT alone
        Console.WriteLine($"{PtrToInt(pt) - PtrToInt(whole)}");   // 36
    }
}
```

A nested field is also a value a `[Dll]` function can be handed: a parameter
declared as the nested record takes the address of *that member*, not of the
whole struct, so `ClientToScreen(window, msg.Pt)` reaches C exactly as
`&msg.Pt` would.

The nested type must itself be `[CLayout]`. A class or a plain record is a
reference to a runtime-owned object, and a struct cannot hold one of those by
value; a field like that is a build error that says so.

### A fixed array inside a record

`byte[32] Rgb` is C's `BYTE rgb[32]`: thirty-two bytes laid end to end inside the
struct, not a pointer to a list. The element type is any field type above —
including another `[CLayout]` record or a function pointer — and the count is a
literal, because `Size` and every offset after the field are compile-time
numbers.

Elements count **from 1**, like everything else in Kiln, so `r.Rgb[1]` is the
first byte and `r.Rgb[32]` is the last. The field converts to a pointer to its
first element, which is where C's own `r.rgb` points, so a `MemZero` or a
`MemCopy` reaches the member and nothing around it:

```k2
namespace InlineArray;

[CLayout]
public record Paint(bool Erase, byte[32] Rgb);

public static class P
{
    public static void Main()
    {
        var ps = new Paint();
        ps.Rgb[1] = 200;
        ps.Rgb[32] = 7;
        Console.WriteLine($"{ps.Rgb[1]} {Paint.Size}");   // 200 36

        MemZero(ps.Rgb, 32);                                // the member, and nothing around it
        Console.WriteLine($"{ps.Rgb[1]}");                 // 0
    }
}
```

An index the compiler can see — a literal, or a constant — is checked when the
program is built: `r.Rgb[33]` on a `byte[32]` is a compile error naming the
count. A **computed** index is a plain address calculation with **no bounds
check** — the same bargain every other `Ptr` operation makes.

## Callbacks: passing a method to C

A `[Dll]` method lets a program call C. The other direction is a method used
where a `Ptr` is wanted: it hands C the method's address, so C can call back into
Kiln. This is what a hook detour, a `CreateThread` ThreadProc, an `EnumWindows`
callback or a Lua C function all need — "here is my function, you call it".

The method must have a C-representable signature — every parameter and the
return in `int`, `long`, `double`, `bool`, `string` or `Ptr`, or `void`. A method
that takes or returns a list, a dictionary or a class has no address you can hand
across, and using it as a `Ptr` is a compile error naming the method and the type
that does not fit.

Given a C library `cb` that calls back through a function pointer:

```c
int apply(int (*fn)(int, int), int a, int b) { return fn(a, b); }
void each(void (*fn)(int), int n) { for (int i = 1; i <= n; i++) fn(i); }
```

a program declares each with a `Ptr` where C takes the function pointer, and
passes a matching method:

```
namespace Callbacks;

public static class P
{
    [Dll("cb", Entry = "apply")]
    public static extern int Apply(Ptr fn, int a, int b);

    [Dll("cb", Entry = "each")]
    public static extern void Each(Ptr fn, int n);

    static int Summer(int a, int b) => a + b;

    static void Announce(int n) => Console.WriteLine($"{n}");

    public static void Main()
    {
        Ptr add = Summer;
        Console.WriteLine($"{Apply(add, 40, 2)}");    // 42
        Each(Announce, 3);                              // 1 2 3, from inside C's loop
    }
}
```

The method runs on whatever thread and stack C calls it from, and it makes no
assumption about an event loop — a ThreadProc handed to `CreateThread` runs on
the new thread, and coordinating that with the rest of the program is the
program's own affair. A `string` parameter arrives as the C `char *` the caller
passed, read for the duration of the call; a `string` the callback returns is
storage the Kiln runtime owns and frees.

A callback is called with the C convention. On 32-bit Windows a Win32 callback —
a WNDPROC, a ThreadProc — is `stdcall`, and a method has no way yet to say so;
see [Limitations](./limitations.md).

## Calling conventions

A C calling convention is the contract for a call: which registers or stack
slots carry the arguments, who pops them afterwards, how the return comes back.
`cdecl` and `stdcall` are two such contracts — on 32-bit x86 they differ in who
cleans the stack, and calling a `stdcall` function as `cdecl` corrupts it.

```
[Dll("user32", Convention = CallConv.System)]
public static extern int MessageBoxA(Ptr handle, string text, string caption, int kind);
```

`CallConv.System` is the one to reach for. It means *the platform's own
convention for its system APIs* — `stdcall` on 32-bit Windows, `cdecl`
everywhere else — so a Win32 declaration written with it stays correct whichever
way the program is built. `CallConv.StdCall` and `CallConv.Cdecl` name a specific
convention outright, for a library that documents one. A function pointer names
one the same way: `delegate* unmanaged[Stdcall]<...>`.

The marker changes code on exactly one target: 32-bit Windows (`--os windows
--arch x86`), where `System` and `StdCall` emit `stdcall` calls. x86-64 Linux and
x64 Windows have a single C calling convention, so there all three produce the
same instructions. It is worth writing regardless: a reader sees a Win32 call for
what it is, and the same source builds correctly for x86.

## Declaration kits

A foreign function, a C-layout record and the constants a C API is written in
terms of are the same in every program that reaches that API. A *kit* can carry
them, so a program says `using Kiln.Win;` and has `MessageBoxA`, `RECT` and
`MB_OK` without transcribing a single one.

A kit is the directory [Kits](./kits.md) describes. A kit that ships
declarations puts them in `.kdecl` files beside its `lib.json`. The file format
is older than Kiln 2 and keeps its own compact syntax — one `dll` line per
function, `record … is c` blocks, and `const` lines — and nothing else: a
declaration bundle *declares*; it does not define or build.

```text
# win.kdecl — the bundle `using Kiln.Win;` brings in
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

`using Kiln.Win;` finds the kit exactly as `using Kiln.Net;` finds a library — a
`kits/` beside the project first, then `~/.kiln/kits/`, then the bundled `libs/`
— and converts its declarations into the program as if they had been written
there:

| In the `.kdecl` | In the program |
| --- | --- |
| `dll MessageBoxA(…) … system` | `MessageBoxA(…)`, a `[Dll]` method with `CallConv.System` |
| `record RECT is c` with `left: int` | `RECT`, a `[CLayout]` record with field `Left` |
| `const MB_OK = 0` | `MB_OK` |
| `ptr`, `text`, `int64`, `word` | `Ptr`, `string`, `long`, `ushort` |

A name written the way C writes one — `MB_OK`, `MEMORY_BASIC_INFORMATION` —
keeps that spelling, so it matches the API's documentation; a record's fields
are spelled the Kiln way.

```
namespace Hello;

using Kiln.Win;

public static class P
{
    public static void Main()
    {
        int clicked = MessageBoxA(PtrNull(), "Built with a kit.", "Hello", MB_YESNO);
        Console.WriteLine($"{clicked}");
    }
}
```

A kit can ship declarations, C-implemented commands, or both: a `.kdecl` beside a
`<name>_libinfo.c` contributes to the one registry from both halves.
`kiln commands --use <name>` lists a kit's `dll:`, `crecord:` and `const:` lines
beside its `command:` lines, so Studio's completion and the reference see them;
`kiln kits` reports the bundle a kit carries.

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
function in `user32.kdecl` may take a `RECT` declared in `gdi32.kdecl`. What the
files share is one namespace, so declaring one name in two files is a
kit-authoring error naming both, caught the moment the kit is used or listed.

### Constants

A program writes its own constants the C# way, and they fold into the code that
uses them:

```k2
namespace Flags;

public static class P
{
    const int RETRIES = 3;
    const string GREETING = "ready";

    public static void Main()
    {
        int left = RETRIES;
        while (left > 0)
            left = left - 1;
        Console.WriteLine(GREETING);
    }
}
```

### Platform-only kits

A kit that wraps a platform's own API works only on that platform: `MessageBoxA`
lives in a Windows `user32`, and there is no Linux library to resolve it
against. Such a kit says so in its `lib.json`:

```json
{ "display": "Win32", "platforms": ["windows"] }
```

Building a program that uses it for another operating system is a compile error
that names the kit and the OS it needs, rather than a wall of linker errors at
the end:

```text
$ kiln build hello.kiln --os linux
kiln: kit `win` supports windows — it cannot be built for linux. Build with `--os windows`.
```

Listing the kit's contents is still allowed anywhere — `kiln commands --use
win` and the language server complete a Win32 declaration on a Linux machine, so
the documentation and the editor work even where a build cannot. A kit with no
`platforms` key is portable and builds everywhere.

## A library the loader runs: DllMain

A `sharedlib` normally has no start-up moment. It exports its methods and waits;
a host calls `<Namespace>_init` once to set up static fields (see
[Build targets](./build-targets.md)) and then calls whatever it needs. Nothing
runs on its own, because a library should not run your code before the host is
ready for it.

A library that is *loaded for effect* — injected into a process, or brought in
with `LoadLibrary`/`dlopen` for what it does rather than what it exports — has no
host willing to make that first call. It has to run the instant it is mapped.
Two specially-named methods give it that moment:

```
public static void DllAttach() { ... }   // runs when the library is mapped into a process
public static void DllDetach() { ... }   // runs when it is unmapped
```

Each takes no parameters and returns nothing — the loader calls them with
neither. Define one or both in a library built with `--target sharedlib` and the
compiler wires it to the platform's loader entry:

| | `DllAttach` | `DllDetach` |
| --- | --- | --- |
| **Windows** | `DllMain`, `DLL_PROCESS_ATTACH` | `DllMain`, `DLL_PROCESS_DETACH` |
| **Linux** | `__attribute__((constructor))` | `__attribute__((destructor))` |

`<Namespace>_init` runs first, before `DllAttach`, so any static field the hook
touches is already set up. A library that defines neither hook gets no loader
entry, and the host calls `<Namespace>_init` itself.

`DllAttach` runs under the OS loader — on Windows, while the loader lock is held.
That is the right place to install a hook or record that the library loaded, and
the wrong place for anything slow or anything that loads another library: work
heavier than a few assignments belongs on a thread the hook spawns (through
`CreateThread` or `pthread_create`), which runs once the loader has let go.

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

The Kiln library declares the two functions it needs, writes the detour as an
ordinary method, and installs it from `DllAttach`:

```
namespace Hook;

public static class Hook
{
    [Dll("hookrt", Entry = "hookrt_original")]
    public static extern int Original(int x);

    [Dll("hookrt", Entry = "hookrt_install")]
    public static extern void Install(Ptr detour);

    public static int Detour(int x) => Original(x) + 1;

    public static void DllAttach()
    {
        Install(Detour);
    }
}
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
    load_hook();                              /* fires DllAttach, installs the detour */
    printf("after %d\n", hookrt_call(10));    /* 21: the detour, original + 1 */
    return 0;
}
```

Loading the library is the whole of it: `DllAttach` runs under the loader,
installs the detour into the one loaded `hookrt`, and the host's next call — made
through the same slot — lands in the Kiln `Detour`, which reaches the original
through the trampoline and adds one. The program prints `before 20` then
`after 21`, on Linux through the constructor and on Windows through `DllMain`.
The complete, buildable example is in
[`examples/hook/`](https://github.com/axdsan/kiln/tree/main/examples/hook).
