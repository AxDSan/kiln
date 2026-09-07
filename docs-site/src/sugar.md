# Shorthands and sugar

A shorthand is a convenience and nothing more. Every one of them
**desugars**: the compiler rewrites it into something the language already had
— a `concat`, an indexed loop, an `if` chain, an `x = x + 1` — before the type
checker ever sees it. So each obeys exactly the rules its longhand does and
carries no second meaning, and nothing you can write with one is a thing the
language could not already do.

That is the whole bargain, and it is why this chapter can be read in any order:
each section says what its shorthand is written as, and what it is rewritten
into. Two of them ask something of the compiler beyond the rewrite, and say so
where they are described — the optional `T?`, which the checker refuses to let
you read as though the value were there, and `assert`, whose expansion is
nothing at all in a release build.

The language they are shorthands *for* is the [Language guide](./language.md).

## Assignment and operator shorthands

Each shorthand below rewrites to a plain `=`, `concat` or comparison you could
have typed by hand, so it obeys the ordinary type rules and carries no second
meaning.

### Compound assignment

`target OP= value` changes a target in place; it is `target = target OP value`.

| written | means |
| --- | --- |
| `x += e` | `x = x + e` |
| `x -= e` | `x = x - e` |
| `x *= e` | `x = x * e` |
| `x /= e` | `x = x / e` |
| `x mod= e` | `x = x % e` (remainder) |
| `s &= e` | `s = concat(s, e)` (join text) |

The target is any place a plain `=` accepts — a variable, a component property,
an array element, a field inside a c-record:

```
module m
sub main
  var total: int = 0
  total += 10
  total *= 3
  call print_int(total)          # 30

  var greeting: text = "hello"
  greeting &= ", world"
  call print_text(greeting)      # hello, world
end
```

`&=` joins text and only text; `+=` on numbers is arithmetic. The type rules are
the ordinary ones, because the desugar reuses the ordinary `+`, `concat` and `=`.

### increment and decrement

`increment x` is `x = x + 1`; `decrement x` is `x = x - 1`. They are statements,
not expressions — there is no `y = increment x`. Both are soft keywords: a
variable named `increment` still works, because the statement is recognised only
when a target name follows the word.

```
module m
sub main
  var n: int = 0
  increment n
  increment n
  decrement n
  call print_int(n)              # 1
end
```

### Joining and repeating text

`+` between two texts joins them, the same as `concat`. `text * count` repeats a
text, the same as `repeat` — the text goes on the left.

```
module m
sub main
  call print_text("ab" + "cd")   # abcd
  call print_text("=" * 10)      # ==========
end
```

`+` never turns a number into its text: `"n=" + 5` is an error, not `"n=5"`.
Build a mixed message with `concat("n=", int_to_text(5))` instead. Keeping `+`
from ever guessing is deliberate — turning values into text is a job of its own.

### Chained comparison

`1 <= x <= 12` means `1 <= x and x <= 12`, the reading from mathematics. Any two
comparisons that share a middle value chain this way, and the middle is
evaluated **once** — so a call in the middle runs a single time:

```
module m
sub main
  var x: int = 5
  if 0 <= x <= 9
    call print_text("one digit")
  end
end
```

Three comparisons in a row have no single meaning and stay an error; write the
rest with `and`.

### Membership: in and not in

`in` asks whether a value is present; `not in` is its negation. What it lowers to
depends on the right-hand side:

| written | means |
| --- | --- |
| `e in xs` (array) | `index_of(xs, e) <> 0` |
| `k in d` (dictionary) | `dict_has(d, k)` |
| `sub in text` | `find(text, sub) <> 0` |

Positions count from 1, so 0 means "absent". `in` is a soft keyword; a variable
named `in` is untouched.

```
module m
sub main
  var days: text[] = ["mon", "tue", "wed"]
  if "tue" in days
    call print_text("found tuesday")
  end
  if "fun" not in days
    call print_text("no fun today")
  end
  if "ell" in "hello"
    call print_text("a substring")
  end
end
```

### One-line if

A single simple statement — a call, an assignment, a `return`, or an
`increment` — may carry a trailing `if`. `STMT if COND` is exactly
`if COND` / `STMT` / `end`:

```
module m
sub classify(n: int): text
  return "negative" if n < 0
  return "zero" if n = 0
  return "positive"
end
sub main
  call print_text(classify(0-3))
  call print_text(classify(7))
end
```

The block `if` — with a body and an optional `else` — is unchanged.

### Underscores in numbers

A long number may be grouped with underscores, between digits: `1_000_000`,
`0.000_1`. This already worked for hex and binary (`0xDEAD_BEEF`).

### Trailing commas

A single trailing comma is tolerated wherever a comma-separated list is written
— argument and parameter lists, array and dictionary literals, record fields —
so a line can be reordered or extended without minding the last comma:

```
module m
sub main
  var xs: int[] = [1, 2, 3,]
  var ages: int{} = {"ann": 30, "bo": 41,}
  call print_int(count(xs),)       # 3
  call print_int(ages["ann"],)     # 30
end
```

## Blocks and raw text

Text is written between quotes, with `\n`, `\t`, `\\`, `\"` and `\0` for the
characters that cannot be typed. Two other spellings exist for the two places
that one is awkward.

**Three quotes** open a block, whose newlines are the newlines themselves:

```
module block

sub main
  let name: text = "world"
  let letter: text = """
Dear {name},

  You have mail.
"""
  call print_text(letter)
end
```

A block is an ordinary text literal in every other way — its escapes still
escape and its `{...}` holes still fire — so it is the same value a `\n`-laden
one-liner would have been, written the way it comes out. One newline directly
after the opening `"""` is dropped, so a block may start on the line below its
delimiter; nothing else is, so the indentation inside a block is part of the
text. A single `"` inside a block is just a quote — only three in a row close
it.

**An `r` before the quote** makes a raw literal, whose backslashes are
backslashes:

```
module raw

sub main
  call print_text(r"C:\logs\today.txt")
  call print_text(r"\d+\s*(\w+)")
  # No holes in a raw literal: these braces are braces.
  call print_text(r"{not a hole}")
end
```

Nothing inside a raw literal is interpreted: there are no escapes and no
interpolation, which is exactly what a Windows path and a regular expression
need. `r"""..."""` is the raw form of a block, for a pattern or a path that
runs over lines. A one-line `r"..."` ends at its first `"`, so text with a
quote in it wants the block form.

`r` is a prefix, not a word: it means a raw literal only when a quote follows
it immediately, so a variable or a subroutine named `r` is untouched.

## String interpolation

A text literal may carry **holes** — `{` an expression `}` — and each hole is
replaced, when the program runs, by that expression turned into text:

```
module interp
sub greet(name: text, unread: int): text
  return "Hi {name}, you have {unread} messages."
end
sub main
  call print_text(greet("Ada", 3))   # Hi Ada, you have 3 messages.
end
```

A hole holds a whole expression, not just a name — arithmetic, a field, a call:

```
module holes
record cart
  items: int
  total: double
end
sub main
  let c: cart = cart(items: 3, total: 19.5)
  call print_text("{c.items} items, {c.total} each side of tax")
  call print_text("subtotal ~ {c.items * 10}")
end
```

The grammar of one hole is:

```text
hole   = "{" expression "}"
```

The expression is the ordinary expression language, parsed up to the `}` that
closes the hole; braces **inside** it (a dictionary literal) balance, so the
hole ends at the matching `}`, not the first one. A hole may not itself contain
a text literal — the quote would close the surrounding string first.

Each hole is turned to text by its type, the same conversion a component
property assignment already performs:

| Hole type | Becomes |
|-----------|---------|
| `text` | itself |
| `int` | `int_to_text` |
| `int64` | `int64_to_text` |
| `double` | `double_to_text` |
| `bool` | the word `true` or `false` |

A type with no text form — a `ptr`, an array, a record, a dictionary — is a
build error that names the hole; convert it yourself first.

A brace you mean literally is doubled: `{{` is one `{` and `}}` is one `}`.

```
module braces
sub main
  call print_text("a set is written {{ }} in maths")   # a set is written { } in maths
end
```

Interpolation is **pure sugar**: the literal desugars to the `concat` chain an
author could have written by hand, one call per join, with the right
`*_to_text` around each hole. `"Row {i} of {n}"` is exactly

```text
concat(concat(concat("Row ", int_to_text(i)), " of "), int_to_text(n))
```

so a literal with no holes is unchanged — one string, no `concat` — and a lone
`"{x}"` is just the one conversion. Because `{` opens a hole in every ordinary
text literal, braces in a string (JSON, a CSS rule) must be doubled — or the
literal written as a
[raw one](#blocks-and-raw-text), `r"..."`, where a brace is a brace and
nothing is interpreted at all. A [block literal](#blocks-and-raw-text),
`"""..."""`, is an ordinary literal in this respect: its holes fire, and its
braces double.

A colon inside a hole is **reserved** for a formatting spec that does not exist
yet, so `"{n:04}"` is a clear error rather than a mis-parse — it is not part of
the language today.

## Taking a run: `a..b`

`xs[a..b]` is the run of a collection from position `a` to position `b`,
**including both**, counting from 1 like everything else. It works on text, on
a list, and on a byte-set:

```
module slices

sub main
  let s: text = "Hello, world"
  call print_text(s[1..5])            # Hello
  call print_text(s[8..])             # world — to the end
  call print_text(s[..5])             # Hello — from the start
  call print_text(s[3..3])            # l — both ends included, so one character

  let xs: int[] = [10, 20, 30, 40, 50]
  let mid: int[] = xs[2..4]           # a list of 20, 30, 40
  call print_int(count(mid))

  let b: bytes = bytes_from_text("ABCDEF")
  call print_text(text_from_bytes(b[2..4]))   # BCD
end
```

Leaving out a bound means the collection's own end — `xs[a..]` runs to the
last position, `xs[..b]` starts at the first, and `xs[..]` is a copy. Each
bound is worked out once, so a call in one runs a single time.

It is a shorthand for the command the thing being sliced already answers to:
`substr` for text, `slice` for a list, `bytes_slice` for a byte-set. Each takes
a start and a **count**, which is why the shorthand is `to - from + 1` of them.
Text is measured in characters rather than bytes, so slicing accented text
never splits one.

**Bounds outside the collection are trimmed, not refused.** A start below 1
reads from 1, an end past the last position stops there, and an end before the
start is empty — the bargain `substr` has always made, kept by all three so
that `s[a..b]` and `xs[a..b]` cannot disagree about the same numbers. A slice
is where a program asks how much is there; failing would mean writing the
bounds check the slice was meant to be. A single position (`xs[3]`) is the
strict one and still reports an out-of-range index.

## Choosing a value

An [`if`](./language.md#choosing) chooses between two *statements*. The same
choice, written where a *value* goes:

```
module plural

sub summary(n: int): text
  let word: text = if n = 1 then "item" else "items"
  return "{n} {word}"
end

sub main
  call print_text(summary(1))
  call print_text(summary(4))
  # `else if` chains, exactly as the block form does.
  let size: text = if n_of(4) > 100 then "big" else if n_of(4) > 10 then "medium" else "small"
  call print_text(size)
end

sub n_of(k: int): int
  return k * 12
end
```

`then` is what tells the two forms apart: a statement that begins with `if` is
always the block form, and the value form cannot be written without a `then`
on the same line.

Both arms must have one type, and that type is the type of the whole
expression — `if c then 1 else "two"` is a compile error naming both. Exactly
one arm is evaluated, so a call in the arm not taken never runs. The `else` is
not optional: a value has to come from somewhere on every path.

## Choosing among values: `match`

An `if` chain that tests one value against several is written once:

```
module traffic

sub advice(light: text): text
  match light
  when "green": return "go"
  when "amber", "red": return "stop"
  else: return "the light is broken"
  end
end

sub main
  call print_text(advice("green"))
  call print_text(advice("amber"))
  call print_text(advice("blue"))
end
```

Each `when` lists one or more values, separated by commas, and matches if the
tested value equals **any** of them. An arm is either the rest of the line, as
above, or a block on the lines below it:

```
module tally

sub main
  var score: int = 0
  for n in 1..4
    match n
    when 1, 2:
      score += 10
      score += 1
    when 3:
      score += 100
    else:
      score = 0
    end
  end
  call print_int(score)
end
```

The values are ordinary expressions, compared with `=` — so a `when` of the
wrong type is the same compile error `light = 3` would be. The tested value is
evaluated **once**, however many arms there are, so `match next_line()` reads
one line and not one per `when`.

`else` is optional; a `match` that matches nothing does nothing, exactly as an
`if` with no `else` does. `break` and `continue` inside an arm belong to
whatever loop encloses the `match`, because a `match` is a branch and not a
loop.

This is a comparison, not pattern matching: there is no binding a name out of
the value and no matching on shape. `match` is the if-chain it replaces,
written the way it is read.

## Iteration: ranges and for-each

Two shorthands sit on top of the counting `for`. Both are pure convenience:
each one turns into the counting [`for`](./language.md#repeating), counting from
one, so everything the `for` guarantees — the once-only bounds, the immutable
loop binding, `break` and `continue` — is true of them too.

A **range loop** writes the bounds with `..` instead of `= … to …`. It counts
inclusively from the first bound to the second, and `step` works exactly as it
does on the counting `for` (a negative step counts down).

```
for i in 1..10
  call print_int(i)        # 1, 2, … 10
end

for i in 10..1 step -1
  call print_int(i)        # 10, 9, … 1
end
```

`for i in A..B` is the same loop as `for i = A to B`; the bounds count with
`int`.

**`for each`** walks a collection and binds its elements, so you never spell the
index yourself. It reads an array's items, a byte-set's bytes (each as an
`int`), a text's characters (each as a one-character text), and a dictionary's
keys:

```
for each name in names          # an array of text
  call print_text(name)
end

for each b in payload           # a byte-set; b is an int, 0..255
  call print_int(b)
end
```

Over a dictionary, two bindings read the key and the value together; one binding
reads the key alone:

```
for each key, count in tallies  # an int{}
  call print_text("{key}: {count}")
end

for each key in tallies
  call print_text(key)
end
```

Add `at` to bind the 1-based position alongside the element:

```
for each line at n in lines
  call print_text("{n}: {line}")
end
```

The element, the value and the index bindings are fresh and immutable inside the
loop, just like a `for` counter, and the collection is read once before the
first turn. `break` and `continue` behave as they do in any loop.

## Repeating a fixed number of times

`repeat N times` is the loop whose counter nobody uses:

```
module drum

sub main
  repeat 3 times
    call print_text("tick")
  end
end
```

It is the counting loop with its variable hidden — the count is read once
before the first turn, `break` and `continue` behave, and `repeat 0 times` runs
the body no times at all. Reach for it when the body does not care which turn
it is on; when it does, name the counter with a `for`.

## Building a list with a loop, written as a value

A list made by walking another one is four lines that are always the same four
lines: an empty list, a loop, an `append`, the name at the end. Written as a
value it is one:

```
module lists

sub main
  let xs: int[] = [1, 2, 3, 4, 5]
  let doubled: int[] = [n * 2 for each n in xs]
  let evens: int[] = [n for each n in xs where mod_int(n, 2) = 0]
  call print_text(join([int_to_text(d) for each d in doubled], ", "))
  call print_text(join([int_to_text(e) for each e in evens], ", "))
end
```

The header after the expression is the `for each` header, word for word — the
same four collections, the same `, value` for a dictionary's value, the same
`at i` for the position, counting from 1:

```
module lists

sub main
  let marked: text[] = ["{i}:{c}" for each c at i in "abc"]
  call print_text(join(marked, " "))
  let ages: int{} = {"ann": 30, "bob": 24}
  let grown: text[] = [k for each k, v in ages where v >= 25]
  call print_text(join(grown, " "))
end
```

`where` is optional; when it is there the element is appended only if the test
is true. The list holds whatever the expression to the left of `for` produces,
so `[int_to_text(n) for each n in xs]` is a `text[]` built from an `int[]`.

And that is all it is: the loop, written where the list is wanted. It runs once
per element, in order, over one snapshot of the collection — because it *is*
the `for each` loop, not a second one that behaves nearly the same.

## Calling with a dot

`x.f(a)` is `f(x, a)` — the same call, spelled left to right, with the value on
the left becoming the first argument. Nothing else changes: it is the same
command, checked the same way, and the two spellings compile to the same thing.

```
module chained

sub main
  let raw: text = "  Ada Lovelace  "
  call print_text(raw.trim().uppercase())
  call print_text(uppercase(trim(raw)))
  # A second argument follows the receiver: `s.find(x)` is `find(s, x)`.
  call print_int(raw.find("Ada"))
  call raw.trim()
end
```

It exists for the chain. `trim(uppercase(replace(s, "-", " ")))` is read from
the inside out and edited from both ends; `s.replace("-", " ").uppercase().trim()`
is read in the order it happens.

The dot is doing two jobs, and the parentheses are what separate them: **a
`.name` followed by `(` is a call, and a `.name` without one is a property or
field read.** So `greeting.text` still reads the label's text, and
`greeting.text.uppercase()` reads it and then shouts it.

## Saying less at the call

Four shorthands share one idea: the compiler already knows something you would
otherwise have to spell out — the type of a value, the position of an
argument, the fields you are not changing — so it lets you leave it unwritten.
Each is rewritten into what you would have typed, and nothing you can write
with one is a thing the language could not already do.

### A binding can take its type from its value

`let` and `var` accept a type, and do not require one when the value says what
it is:

```
module measured

sub twice(n: int): int
  return n * 2
end

sub main
  let words: text[] = ["alpha", "beta", "gamma"]
  let n = count(words)
  let greeting = concat("hello, ", words[1])
  let plenty = n > 2
  var running = twice(n)
  running += 1
  call print_int(n)
  call print_text(greeting)
  call print_int(if plenty then 1 else 0)
  call print_int(running)
end
```

The type is read off the initializer and nothing else — the value's own type,
not what it is later used as — so it is exactly the type you would have
written. `n` is an `int` and stays one; passing it where a `text` is wanted is
the same mistake it would have been with the annotation in place.

Write the type when the value cannot supply one. `let xs = []` says nothing
about what the list holds, and `var total` says nothing at all; both are
refused, naming the binding, and `let xs: text[] = []` is the fix. Write it
too when it is the point: a module-level `var` always declares its type,
because a reader of the file is not necessarily reading the initializer.

### A parameter can have a default

A parameter may end with `= value`, which is what a call that leaves it out
gets:

```
module connecting

sub connect(host: text, port: int = 80, timeout: int = 5000): text
  return "{host}:{port}, giving up after {timeout}ms"
end

sub main
  call print_text(connect("example.com"))
  call print_text(connect("example.com", 8080))
  call print_text(connect("example.com", 8080, 250))
end
```

Only the *last* parameters may have one. A default in the middle would make
`connect("a", 250)` mean different things depending on where the reader started
counting, and there is no spelling that says which — so it is refused where it
is written.

The default is an expression, and it is evaluated **at the call**, once per
call that needs it. That is what keeps it a shorthand: `connect("a")` is
`connect("a", 80, 5000)`, written out by the compiler.

It also means a default may not read a *name*. A parameter and a local do not
exist where the call is written, and a module variable there could be shadowed
by a local of the caller's — so a default is built from literals, constants and
calls, and one that names a variable is refused where it is declared. A call is
fine, and is made afresh for each call that needs it: `sub log(at: int =
now_ms())` timestamps each call, not the declaration.

A `dll` cannot declare a default. It names a function someone else wrote, and
that function has no opinion about what a missing argument means; write a
`sub` around it that does.

### An argument can name its parameter

Any argument may be written `name: value`, and goes to the parameter of that
name whatever order it is in:

```
module named

sub window(title: text, width: int = 640, height: int = 480, resizable: bool = true): text
  return "{title} {width}x{height} resizable={if resizable then 1 else 0}"
end

sub main
  call print_text(window(title: "Editor", height: 900, width: 1200))
  call print_text(window("Console", resizable: false))
  call print_text(window(title: "Splash"))
end
```

This is what a wide signature is for: `window("Splash", 640, 480, true)` is
four values whose meaning is their position, and `window(title: "Splash")` is
one whose meaning is written down. Parameters the call does not mention take
their defaults, so the two shorthands compose.

The rules are the ones you would guess. A name that is not a parameter is
refused, and the message lists the ones that are. A parameter given twice —
once by position and once by name — is refused. Positional arguments come
first: once an argument names its parameter, the ones after it must too, since
"the next one" no longer has an answer.

Foreign functions take named arguments as well, because a `dll` declares its
parameter names the same way a `sub` does:

```
module ffi

dll MessageBoxA(handle: ptr, text: text, caption: text, kind: int): int from "user32" system

sub main
  call MessageBoxA(handle: ptr_null(), caption: "Saved", text: "All done.", kind: 0)
end
```

(That is the Windows API, so it runs on Windows; what it shows is the shape.
A four-argument C function whose third argument is the caption is exactly the
call that is easiest to get wrong by position.)

Library commands do not. Their metadata carries types, not names, so a call to
one is positional and a named argument is refused rather than guessed at.

### A record can be written with braces, and copied with `...`

`point(x: 1, y: 2)` has a second spelling, `point{x: 1, y: 2}`, which reads as
a value rather than a call and may run over several lines. Both are the same
record, and every field must still be given:

```
module records

record point
  x: int
  y: int
  label: text
end

sub main
  let origin = point{
    x: 0,
    y: 0,
    label: "origin",
  }
  let moved = point{...origin, x: 3, label: "moved"}
  call print_text("{origin.label} {origin.x},{origin.y}")
  call print_text("{moved.label} {moved.x},{moved.y}")
end
```

`...base` inside the braces is an **update**: every field you do not name is
copied from `base`. It is rewritten into the literal with each field spelled
out — `point(x: 3, y: origin.y, label: "moved")` — which is why the original is
untouched: an update makes a new record, it does not write into the old one.
(`..base` is the same thing; the extra dot is there because `...` is how most
people write it.)

What follows `...` has to be a name, or a field or element path from one, since
it is read once for each field it fills in. A call there would be a call per
field, so it is refused with that reason.

A c-record — one declared `record R is c`, with a fixed C memory layout — has
no value form to copy, so it has no update. It does accept a literal in the one
place a literal makes sense, which is where it is declared:

```
module cstruct

record rect is c
  left: int
  top: int
  right: int
  bottom: int
end

sub main
  var r: rect = rect{left: 10, right: 200}
  call print_int(r.left)
  call print_int(r.top)
  call print_int(r.right)
end
```

That is the declaration and its field writes, written once: `var r: rect`
followed by `r.left = 10` and `r.right = 200`. The fields the literal leaves
out keep the zero a c-record declaration already gives them.

## A value to fall back on: `otherwise`

`EXPR otherwise FALLBACK` is the value of `EXPR`, unless the call in it failed,
in which case it is `FALLBACK`.

```
module fallback
use file

sub main
  let notes: text = file_read_text("notes.txt") otherwise "(no notes yet)"
  call print_text(notes)
end
```

That is exactly the [program that reads the error slot by
hand](./language.md#when-a-command-fails), with the `if` written for you:
`EXPR` runs into a temporary, and then the value is
`if last_error_code() <> 0 then FALLBACK else <that temporary>`. So `EXPR` runs
once, the fallback runs only when it failed, and both sides must have one type.

It does **not** clear the error slot — `last_error_code()` still reports what
went wrong afterwards, which is what lets a program fall back *and* log why.
For the same reason an expression carries one `otherwise` and not two: a second
would test a slot the first fallback never cleared, and take the last arm every
time. It is only meaningful after a command that can fail; after one that
cannot, the code it reads is whatever an earlier call left there.

## Passing a failure back: `check`

`check` runs a call and, if it failed, returns from the subroutine
immediately.

```
module propagate
use file

sub greeting_from(path: text): text
  let name: text = check file_read_text(path)
  return "Hello, {name}!"
end

sub copy(from: text, to: text): bool
  let body: text = check file_read_text(from)
  check file_write_text(to, body)
  return true
end

sub main
  call print_text(greeting_from("name.txt"))
  call print_text(if copy("name.txt", "name.bak") then "copied" else "could not copy")
end
```

Each `check` expands to the binding (or the call) followed by
`if last_error_code() <> 0 <return> end`. The value that early `return` carries
is the sentinel a failing command of that type already returns — `0` for a
number, `""` for text, `false` for a yes/no, `ptr_null()` for a pointer,
nothing at all in a subroutine that returns nothing — so a caller sees the same
failure it would have seen from the call itself, with the reason still in the
slot. A subroutine returning a list, a dictionary or a record has no such
sentinel, so `check` is refused there and the `if` is written out.

`check` leads a statement or a `let`/`var` initializer, and a one-line `if`
cannot be attached to it: the suffix would guard the call and leave the
propagation running regardless, which is a wrong answer rather than an error.

## A value that may not be there: `T?`

Some questions have no answer. A key that is not in a dictionary, a setting
nobody wrote, a line at the end of a file — the command has to say "nothing",
and the program has to be made to notice.

Write a `?` after the type and you have said that the value may be absent:

```
module maybe

sub main
  let ages: int{} = {"ann": 30, "bob": 24}
  let ann: int? = dict_get(ages, "ann")
  let zed: int? = dict_get(ages, "zed")
  call print_int(ann otherwise 0)
  call print_int(zed otherwise 0)
end
```

An `int?` is not an `int`, and the checker will not let you use one as though it
were:

```
call print_int(ann)
```

```
command `print_int` argument 1 expects int, got int? — a value that may be
absent. Supply the missing one with `... otherwise <int>`, or open it with
`if some ... as value`
```

That refusal is the whole feature. There are two ways past it, and each leaves
an ordinary value behind.

**`otherwise` supplies the one that is not there.** You met it above as the
fallback for a failed call; on an optional it reads the optional's own answer
rather than the error slot, so it is still right long after the call that
failed.

**`if some ... as` opens the one that is.** The name it binds is an ordinary
local — a plain `int`, not an `int?` — so the body can use it anywhere a value
goes:

```
module maybe

sub main
  let ages: int{} = {"ann": 30}
  let ann: int? = dict_get(ages, "ann")
  if some ann as years
    call print_text("ann is {years} this year")
  else
    call print_text("ann is not in the book")
  end
end
```

`none` is the optional that holds nothing:

```
module maybe

sub main
  var found: text? = none
  let names: text[] = ["ann", "bob"]
  for each n in names
    if n = "bob"
      found = n
    end
  end
  call print_text(found otherwise "(nobody)")
end
```

An optional is a **local's** type, and only a local's. It is a value with a
hidden truth beside it saying whether the value is there — two things, where a
parameter, a return type, a list element or a record field has room for one. So
`sub f(v: text?)` is refused where it is written, and a subroutine that may
have no answer returns the answer plus a sentinel it documents, exactly as it
did before. Unwrap at the edge, and pass a `T`.

The truth beside the value is *this* line's verdict. `let n: int? = f()` is
absent when `f` failed and present otherwise, and a command that cannot fail
never produces an absent one — an earlier failure elsewhere in the program does
not leak into it.

## Cleaning up on the way out: `defer`

Something opened has to be closed, and the closing belongs next to the opening
— not eight lines below, repeated once per way out.

```
module cleanup

var log: text = ""

sub note(what: text)
  log = log + what + " "
end

sub attempt(n: int): int
  call note("open")
  defer call note("close")
  if n = 1
    return 10
  end
  call note("work")
  return 20
end

sub main
  call print_int(attempt(1))
  call print_text(log)
end
```

```
10
open close
```

`defer STMT` runs `STMT` when the block it was written in is left — **whichever
way it is left**: falling off the end, a `return` below it, a `break` or a
`continue` out of a loop body. Several defers in one block unwind in reverse
order of declaration, because the second one was set up while the first one's
cleanup was already standing.

The value a `return` carries is computed **before** the cleanup runs, which is
what makes the pattern safe:

```
  let f: int = file_open(path, "r")
  defer call file_close(f)
  return file_read_line(f)
```

The read happens while the handle is still open. Without that rule the pairing
would be a trap rather than a convenience.

A `defer` belongs to its own block, so one inside a loop body runs on every
turn:

```
module cleanup

sub main
  var i: int = 0
  while i < 3
    i += 1
    defer call print_text("turn {i} done")
    if i = 2
      continue
    end
    call print_text("turn {i} work")
  end
end
```

It takes **one simple statement** — a call, an assignment, a property write.
A block has an end of its own, and "the end of the block" is the whole of what a
`defer` means, so `defer if ... end` and `defer while ... end` are refused with
that reason. So are `defer return` and `defer break`, which would leave from
inside the cleanup, and `defer let`, which would bind a name nothing could read.

There is no run-time list of pending calls behind any of this: the statement is
copied to each exit of the block, and what runs is the program you could have
written by hand with the closing spelled out four times.

## Named numbers: `enum`

A run of related whole numbers, each with a name:

```
module levels

enum severity
  info, warning
  error
end

sub label(s: severity): text
  match s
  when severity.info: return "info"
  when severity.warning: return "warning"
  else: return "error"
  end
end

sub main
  call print_int(severity.info)      # 1
  call print_int(severity.error)     # 3
  call print_text(label(severity.warning))
end
```

The members are numbered from **1**, in declaration order, like every other
position in Kiln. They may be written one per line or several to a line,
separated by commas.

A member is reached only through the enum's name — `severity.info`, never a
bare `info` — so an enum adds no names to the module and two enums may each
have a `red`. Writing a member that does not exist is a compile error that
lists the ones that do.

An enum is a **name for `int`s**, not a type of its own: `severity` written as
a parameter or field type means `int`, so a subroutine declared
`sub label(s: severity)` accepts `severity.info` and accepts a plain `2`, and
every rule about ints — arithmetic, comparison, crossing to C in a `dll`
declaration — applies unchanged.

## Checking as you go: `assert`

`assert` states something that must be true, and stops the program when it is
not:

```
module withdraw

sub take(balance: int, amount: int): int
  assert amount > 0, "an amount must be positive"
  assert amount <= balance
  return balance - amount
end

sub main
  call print_int(take(100, 30))
end
```

A failing assertion prints its message to standard error and exits with a
failing status, so a script that runs the program can tell. With no message of
its own it quotes the condition as you wrote it — `assertion failed:
amount <= balance` — which is usually the message you would have typed.

**A release build compiles asserts out entirely.** `kiln build --release`
emits no check, no branch and no message for one: an `assert` costs a debug
build a comparison and costs a release build nothing. So an `assert` is for
stating what you believe, not for validating input a user typed — check that
with an `if`, which is there in both builds.

## The two tours

`examples/sugar_tour.kiln` puts the operator and iteration shorthands into one
short program — compound assignment, text `+`, interpolation, a range loop,
`for each` over a dictionary, `in`, and a one-line `if`.
`examples/sugar09_tour.kiln` does the same for the rest: block and raw text, a slice, the dot call, an
inferred `let`, the value `if`, `enum`, `match`, `repeat`, `assert`, a
parameter default, a named argument, a record literal and an update, an
optional opened with `otherwise` and with `if some`, a list built by a loop,
`check`, and `defer`. Both print a fixed transcript, and both are built and
run — on Linux and cross-built for Windows — by the test suite, so a shorthand
that regresses fails there.
