# Memory

You do not free anything in Kiln. There is no `free`, no `delete`, no
ownership annotation, and no lifetime to reason about. A string, a `List<T>`, a
class instance, a dictionary and a byte-set are all allocated by the runtime,
and the runtime reclaims each one once your program can no longer reach it.

```k2
namespace Summary;

public static class P
{
    static string Summarise(List<string> rows)
    {
        var text = "";
        foreach (var r in rows)
        {
            text = text + r + "\n";      // every one of these builds a new string
        }
        return text;
    }

    public static void Main()
    {
        Console.WriteLine(Summarise(["first", "second", "third"]));
    }
}
```

That loop makes one string per row and abandons all but the last. Nothing here
says so, and nothing needs to: the abandoned ones go away on their own.

## What "can no longer reach it" means

A value stays as long as some name still leads to it — a local in a method that
has not returned, a static field, a field of an object you can still reach, an
element of a list you can still reach. Two names for one list are two names for
the same list, and it lives until neither can be used.

```k2
namespace Reach;

public static class P
{
    static List<string> kept = new List<string>();

    static void Remember(string line)
    {
        kept.Add(line);                  // `kept` is a static field: this lives
    }

    static void Forget(string line)
    {
        var scratch = new List<string>();
        scratch.Add(line);               // `scratch` dies with the call
    }

    public static void Main()
    {
        Remember("stays");
        Forget("goes");
        Console.WriteLine($"{CollectGarbage()}");
        Console.WriteLine($"kept {kept.Count}: {kept[1]}");
    }
}
```

The consequence worth internalising: **memory grows when your program is
holding things, and only then.** A server whose memory climbs is a server that
is storing something — usually a static field nobody prunes. The collector
cannot tell "still wanted" from "forgotten but still reachable", and it does
not try.

## When it happens

As your program allocates. The runtime keeps a running count and collects when
enough has been allocated since the last time — at least a megabyte, and above
that, proportional to how much the program is holding. A program that allocates
almost nothing never collects at all.

Two commands let you watch and steer it:

| Command | Answers |
|---|---|
| `MemoryInUse()` | bytes of program data held right now |
| `CollectGarbage()` | collects immediately; answers the bytes reclaimed |

`CollectGarbage` is rarely needed. It earns its place when your program knows
something the runtime cannot: a level has finished loading, a request has been
answered, a frame has been drawn and the next one is not due for 14 ms.

```
// In a form: a timer's handler that collects where a pause does not show.
void OnFrame()
{
    DrawEverything();
    framesDrawn = framesDrawn + 1;
    if (framesDrawn > 600)          // once every ten seconds, say
    {
        CollectGarbage();
        framesDrawn = 0;
    }
}
```

## What it costs

Collection stops the program while it runs, and the pause grows with how much
work has happened since the last one. Measured on a loop building two million
short texts: a 2 ms pause about every megabyte, an 8 MB resident set, and a
total run *faster* than the same program with collection turned off — freed
memory is reused memory, and reused memory is in cache.

With a large live set the pause is longer, because everything reachable is
walked: a program holding 8 MB across a busy loop pauses for tens of
milliseconds. That is the honest number, and it is why
[Limitations](./limitations.md) lists incremental collection as unwritten work.

Three environment variables, for when a measurement is worth more than a guess:

| Variable | Effect |
|---|---|
| `KILN_GC_TRACE=1` | one line per collection: how long, how much freed, how much held |
| `KILN_GC_MIN_HEAP=<MB>` | raise the floor — fewer, longer pauses; less total overhead |
| `KILN_GC=0` | turn collection off entirely; memory is freed at exit, as it was before |

## The one rule for foreign code

The collector finds values by reading the machine stack, the processor's
registers and your static fields, and following what it finds. It does not
read memory owned by a C library. So if you hand a string or a byte-set to a
foreign function through a [`[Dll]` declaration](./interop.md) and that function
*keeps* the pointer past the call, the value must stay reachable from a Kiln
name for as long as it is kept:

```
// Wrong: nothing refers to the buffer after this line, so it may be reclaimed
// while the C library is still writing into it.
CRegisterBuffer(Bytes.Alloc(4096));

// Right: the buffer is held by a static field for as long as C holds it.
buffer = Bytes.Alloc(4096);
CRegisterBuffer(buffer);
```

A function that reads its argument and returns — which is nearly all of them —
needs nothing. The rule is only about a pointer that outlives the call.

Support libraries written against `abi/kiln_abi.h` follow the same rule and
have `kn_gc_root` for the case where they must keep one; the header says how.

## Library targets

`--target sharedlib` and `--target staticlib` produce code with no `main`, so
the runtime has no stack it can vouch for and does not collect. Memory there is
released when the host unloads the library, exactly as it was before the
collector existed.
