# Language guide

This is the whole language, in the order you meet it: a module, then the
values it holds, then the ways it chooses, repeats and calls, then what
happens when something fails. Start at the top and read down and nothing will
refer forward to something you have not seen; come back later and each
section stands on its own.

A whole program is one module. Files are UTF-8 and use the extension `.kiln`,
and `#` starts a comment that runs to the end of the line.

```
# A module is a compilation unit.
module hello
target console

sub main
  call print_text("Hello.")
end
```

Copy that into `hello.kiln` and `kiln run hello.kiln` prints it. Every listing
on this page is a whole module for the same reason: you can run each one, and
the build checks every one of them, so a sample that stops being true stops
the build rather than misleading a reader.

Some of what follows is **sugar** — a spelling the compiler rewrites into
another one on this page before the type checker ever sees it. Those are
marked where they appear, and each says what it is rewritten into, because
that is the whole of what it means: a shorthand obeys exactly the rules its
longhand does and carries no second meaning. Two ask something of the compiler
beyond the rewrite and say so: the optional `T?`, which the checker refuses to
let you read as though the value were there, and `assert`, whose expansion is
nothing at all in a release build.

`examples/sugar_tour.kiln` and `examples/sugar09_tour.kiln` put the shorthands
into two short programs that print a fixed transcript, built and run — on
Linux and cross-built for Windows — by the test suite.

## Modules

```
module <name>
target <kind>      # optional
use <library>      # optional, repeatable
```

`target` and `use` come first, then the module's contents. Leave `target` out
and Kiln infers it — a module with a form is a windowed program, anything
else is a console one. See [Build targets](./build-targets.md).

`use ui` brings in the visual components; without it, `form` and the visual
component types are not defined. `timer` needs no library — it is part of the
core runtime, and a console program can declare one.

### Units: one program, several files

A program is still one module. What a **unit** adds is a way to assemble it
from more than one file. A file that starts `unit <name>` holds subroutines,
records, constants, `dll` declarations and module variables — and nothing that
runs: no `main`, no form, no `target`. The program pulls it in with the same
`use` it uses for a library, and `<name>.kiln` beside the program is what
resolves.

```text
# maths.kiln
unit maths
use system

const TWO = 2

sub twice(n: int): int
  return n * TWO
end
```

```text
# app.kiln
module app
use maths

sub main
  call print_int(twice(21))     # 42; `use system` came in with the unit
end
```

`use maths` finds `maths.kiln` beside `app.kiln` because its header says
`unit`; a name with no such file beside it is a library, exactly as before. A
unit may `use` other units and libraries, and whatever it uses reaches the
program. Everything a unit declares is visible to the program by its plain
name — there is no prefix — so one name declared in two files is an error that
names both, rather than a guess about which one you meant. `kiln build` and
`kiln run` take the program, never the unit; `kiln inspect` lists each
resolved unit beside its `use:` line.

## Values that change, and values that do not

`let` binds a value that stays put. `var` binds one you intend to reassign.

```
let limit: int = 10       # reassigning this is an error
var total: int = 0
total = total + 5
```

The distinction is enforced: assigning to a `let` is a compile error that says
so, and names which fix to apply.

A `var` at module level is visible to every subroutine in the file — which is
how an event handler and the rest of the program share state:

```
module counters
target gui
use ui

var hits: int = 0

form main_window
  title = "Counter"
  width = 320
  height = 160

  button tap
    text = "Tap me"
    left = 40
    top = 40
    width = 160
    height = 40
    on click: on_tap
  end
end

sub on_tap
  hits += 1
  tap.text = "tapped {hits}"
end
```

A module variable's initializer may call commands but may not read another
module variable — order-dependent start-up is a source of bugs that is easier
to forbid than to explain.

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

### Increment and decrement

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

## Types

| Type | Holds |
| --- | --- |
| `int` | a whole number |
| `int64` | a wider whole number |
| `double` | a number with a fractional part |
| `text` | UTF-8 text |
| `bool` | `true` or `false` |
| `T[]` | a list of `T` |
| `T{}` | a dictionary of `T`, found by text key |
| `bytes` | a run of raw bytes |
| a record | a group of named fields you declare yourself |

Types are written after a colon, or taken from the value when the value makes
the type plain — see [A binding can take its type from its
value](#a-binding-can-take-its-type-from-its-value):

```
let count: int = 3
let ratio: double = 1.5
let name: text = "Kiln"
let ready: bool = true
```

There is no implicit conversion between them. Convert explicitly:

```
let n: int = 42
call print_text("answer: " + int_to_text(n))
```

### Writing text

Text is written between quotes, with `\n`, `\t`, `\\`, `\"` and `\0` for the
characters that cannot be typed, and `{...}` holes for values — see
[String interpolation](#string-interpolation).

Two other spellings exist for the two places that one is awkward: a **block**
between three quotes, whose newlines are the newlines themselves, and a **raw**
literal, `r"..."`, whose backslashes are backslashes. Both are the same text
value written a different way — see [Blocks and raw
text](#blocks-and-raw-text).

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

### String interpolation

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

### Blocks and raw text

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

### Underscores in numbers

A long number may be grouped with underscores, between digits: `1_000_000`,
`0.000_1`. This already worked for hex and binary (`0xDEAD_BEEF`).

## Groups of values

A list is written with `[]`, a dictionary with `{}`, and both count from 1 —
as does every other indexed thing in the language, so a loop runs `1` to the
count with no `- 1` anywhere.

```
var names: text[] = ["Ada", "Grace"]
names = append(names, "Alan")
call print_text(names[1])              # Ada

var ages: int{} = {"Ada": 36}
ages["Grace"] = 45
call print_int(ages["Ada"])
```

A dictionary holds one type of value. Asking for a key that is not there
answers that type's sentinel — `0`, `""`, `false` — and leaves the reason in
the error slot, so `dict_has` is what separates a missing key from a stored
`0`.

A `record` names a group of related values, and is the way a subroutine gives
back more than one thing:

```
record point
  x: int
  y: int
end

sub midpoint(a: point, b: point): point
  return point(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
end
```

Fields are given by name when a record is built, and read and written with a
dot: `p.x`, `p.x = 5`. A record is a **reference**, exactly as a list is — two
names for one record are two names for the same fields — so passing one to a
subroutine does not copy it.

A collection also answers to `xs[a..b]` for a run of it, and a record has a
second spelling, `point{x: 1, y: 2}`, that reads as a value rather than a call.
Both are shorthands for what is above — see [Taking a run:
`a..b`](#taking-a-run-ab) and [Saying less at the
call](#saying-less-at-the-call).

### Taking a run: `a..b`

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

## Expressions

Arithmetic is `+ - * / %` with the usual precedence, and parentheses group.
`%` is the remainder, and a leading `-` negates.

```
let x: int = 2 + 3 * 4        # 14
let y: int = (2 + 3) * 4      # 20
let r: int = 17 % 5           # 2
let below: int = -40          # negation, on literals and on expressions
```

Both sides of an arithmetic operator must be the same type: there is no
implicit conversion, so `d + 1` where `d` is a `double` is an error. Write
`d + int_to_double(1)`.

Dividing an integer by zero stops the program with a message on stderr rather
than killing it silently.

`+` on two `text` values joins them — the same thing the `concat` command
does, spelled so that building a sentence does not nest.

```
call print_text("Hello, " + who + " — you are " + int_to_text(age) + " today.")
```

Comparisons produce a `bool`: `=` `<>` `<` `<=` `>` `>=`. Combine them with
`and`, `or` and `not`.

```
let in_range: bool = count >= 1 and count <= 10
let missing: bool = not found
```

`=` compares. Assignment is a statement, never an expression, so `if x = 5`
tests whether `x` is five — it cannot assign by accident.

A command that returns a value can be used anywhere a value fits:

```
let longest: int = max_int(length(first), length(second))
```

See [Commands](./reference-commands.md) for the full list.

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

### Membership: `in` and `not in`

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

## Bitwise operators and hex literals

A flag word, a mask, a packed pair of 16-bit halves — these are values whose
*bits* matter rather than their size. Kiln writes them the way their
documentation does, and operates on them with words.

### Writing a bit pattern

A number may be written in hexadecimal with `0x` or in binary with `0b`, and
`_` may be put anywhere in the digits to group them.

```
let mask: int = 0xFF          # 255
let bits: int = 0b1010        # 10
let magic: int = 0xDEAD_BEEF
```

**A hex or binary literal is a bit pattern, and how wide it is comes from
where it lands.** On its own, a pattern of 32 bits or fewer is an `int`
holding exactly those bits — so `0x8000_0000` is `-2147483648` and
`0xFFFF_FFFF` is `-1`, which is what a mask written for a 32-bit word means.
A pattern that needs more than 32 bits is an `int64` holding exactly those
bits. Where an `int64` is wanted, the same pattern is those bits in 64,
gaining zeros rather than a sign:

```
const HKEY_CURRENT_USER = 0x8000_0001

let as_int: int = HKEY_CURRENT_USER        # -2147483647
var as_wide: int64 = HKEY_CURRENT_USER     # 2147483649
```

Both readings are the same 32 bits; which one a program gets is decided by
the type it is being read into, so a `DWORD` constant is right in an `int`
mask and right again in an `int64` parameter without being written twice.

How wide the pattern is comes from its *value*, not from how many digits were
typed: leading zeros change nothing, so `0x0000_0000_DEAD_BEEF` is the same
pattern as `0xDEAD_BEEF`. The way to the 64-bit reading is the destination —
`var v: int64 = 0xDEAD_BEEF` is 3735928559.

A leading `-` says the pattern was meant as a magnitude after all, so `-0x10`
is simply `-16`. More than 64 bits, a digit the base does not have, or no
digits at all is an error at the literal.

Decimal is unchanged: `2147483648` is an `int64`, because it is a *number*
and that number does not fit an `int`.

### The operators

| Written | Does | On |
| --- | --- | --- |
| `a band b` | bits set in both | `int`, `int64` |
| `a bor b` | bits set in either | `int`, `int64` |
| `a bxor b` | bits set in one but not both | `int`, `int64` |
| `bnot a` | every bit flipped | `int`, `int64` |
| `a shl n` | shift left by `n` | `int`, `int64` |
| `a shr n` | shift right, keeping the sign | `int`, `int64` |
| `a ushr n` | shift right, filling with zeros | `int`, `int64` |

They are defined on `int` and `int64` and on nothing else. A `double`'s bits
are an IEEE encoding, so `and`-ing two of them is never what was meant;
`bool`, `text` and `ptr` have no bits a program addresses one at a time. Any
of those is an error naming the operator and the side it was on.

`bnot` is the bitwise partner of `not`: `not` answers a truth value, `bnot`
flips bits.

```
let combined: int = WS_VISIBLE bor WS_POPUP     # flags together
let cleared: int = flags band bnot WS_BORDER    # one flag taken away
let low: int64 = wparam band 0xFFFF             # the low half of a WPARAM
let high: int64 = wparam ushr 16 band 0xFFFF    # the high half
```

`shr` keeps the sign and `ushr` does not: `-16 shr 2` is `-4`, and
`-16 ushr 2` is `1073741820`. A value used as a bit pattern rather than as a
number wants `ushr`.

### Precedence

Tightest at the top. A bitwise operator binds **looser** than a comparison,
which is what lets a flag test be written without parentheses — the one place
this table deliberately differs from C's.

| | |
| --- | --- |
| `xs[i]`, `r.field` | postfix |
| `-a`, `bnot a` | unary |
| `* / %` | |
| `+ -` | |
| `shl` `shr` `ushr` | |
| `band` | |
| `bxor` | |
| `bor` | |
| `= <> < <= > >=`, `in`, `not in` | comparison and membership |
| `not` | |
| `and` | |
| `or` | loosest |

```
if style band WS_VISIBLE <> 0        # (style band WS_VISIBLE) <> 0
let x: int = 1 shl 4 band 0xFF       # (1 shl 4) band 0xFF        -> 16
let y: int = 1 shl 2 + 2             # 1 shl (2 + 2)              -> 16
let z: int = 1 bor 6 band 4          # 1 bor (6 band 4)           -> 5
```

### The rules the checker holds you to

**Both sides of `band`, `bor` and `bxor` must be the same width.** A literal
takes the width of what it meets, so `wparam band 0xFFFF` works with an
`int64` `wparam`; an `int` *variable* does not, because that would be the
implicit conversion the language does not have. Write
`a band int_to_int64(b)`, and the message says so.

**A shift's count is a count**, not a second value: it may be an `int` or an
`int64` whatever the value's type is, and the result is the value's type. A
count written down must be within the value's width — `1 shl 32` on an `int`
is refused at build time, because there is no answer to give. A count only
known at run time is taken modulo that width.

**The infix operator words are soft keywords.** `band`, `bor`, `bxor`, `shl`,
`shr` and `ushr` mean the operator only where an operator can go — after a
complete value, where a name could never have appeared. A variable, a
parameter or a field named for one keeps working:

```
var band: int = 7
call print_int(band + 1)      # 8 — `band` here is a name
var shl: int = 7
call print_int(shl shl 2)     # 28 — a name, the operator, a number
```

`bnot` is the exception: it is a **reserved word**, like `not`. A prefix
operator cannot be soft — `bnot(x)` reads as the operator and as a call to
something named `bnot` equally well, and `bnot - 1` as a complement and as a
subtraction. Guessing there gives a wrong answer rather than an error, so the
word is refused as a name at the line that writes it.

## Choosing

```
if temperature > 30
  call print_text("hot")
else if temperature > 15
  call print_text("mild")
else
  call print_text("cold")
end
```

The condition must be a `bool`. There is no truthiness — an `int` is not a
condition, and saying so is a compile error rather than a surprise.

`else if` chains as far as you like, and `else` is optional. Written out as a
program:

```
module grading

sub grade(score: int): text
  if score >= 90
    return "A"
  else if score >= 80
    return "B"
  else if score >= 70
    return "C"
  else
    return "F"
  end
end

sub main
  call print_text(grade(95))    # A
  call print_text(grade(83))    # B
  call print_text(grade(40))    # F
end
```

A condition is any expression of type `bool`, so `and`, `or` and `not`
combine tests, and a command that answers a `bool` is a condition on its own:

```
module guard
use file

sub main
  let path: text = "notes.txt"
  if file_exists(path) and length(file_read_text(path)) > 0
    call print_text("there is something to read")
  end
  if not file_exists(path)
    call print_text("nothing there")
  end
end
```

The same choice written where a *value* goes is [`if c then a else
b`](#choosing-a-value), and a chain that tests one value against
several is written once with
[`match`](#choosing-among-values-match). Both are shorthands for the
block above.

### One-line `if`

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

### Choosing a value

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

### Choosing among values: `match`

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

## Repeating

`while` repeats for as long as its condition holds.

```
var i: int = 0
while i < 3
  call print_int(i)
  i = i + 1
end
```

`for` counts. The loop variable is an `int` that belongs to the loop and
cannot be assigned to inside it; `step` counts by something other than one,
and a negative step counts down.

```
for n = 1 to 10
  call print_int(n)
end

for n = 10 to 1 step -2
  call print_int(n)
end
```

The start and the limit are read **once**, before the first turn, so a loop
cannot be lengthened by its own body. `step` is a whole-number literal, which
is what lets the compiler know whether the loop counts up or down.

`break` leaves the innermost loop; `continue` goes straight to its next turn.
Both work in a `while` and in a `for`.

```
for n = 101 to 200
  if n % 7 <> 0
    continue
  end
  call print_int(n)     # the first multiple of 7 above 100
  break
end
```

Local variables are visible for the whole subroutine, so two loops in one
subroutine need two different loop-variable names.

The four ways of repeating, in one program — a `while`, a counting `for`, a
`for` with a `step`, and `break` and `continue`:

```
module looping

sub main
  # while: repeat until something stops being true
  var n: int = 1
  while n <= 3
    call print_int(n)                # 1 2 3
    n = n + 1
  end

  # for: count, inclusively, from one number to another
  var total: int = 0
  for i = 1 to 10
    total = total + i
  end
  call print_int(total)              # 55

  # step: by something other than one, and downwards when negative.
  # A second loop in the same subroutine needs a second name — a local is
  # visible for the whole body, not just its loop.
  for down = 10 to 1 step -3
    call print_int(down)             # 10 7 4 1
  end

  # continue skips a turn; break leaves the loop
  for k = 1 to 100
    if k % 3 <> 0
      continue
    end
    if k > 10
      break
    end
    call print_int(k)                # 3 6 9
  end
end
```

Three shorthands sit on top of this counting loop: the range loop
`for i in 1..n` and `for each x in xs`, which walk a collection without
spelling the index, and [`repeat N
times`](#repeating-a-fixed-number-of-times), which hides the counter
when nothing uses it. All are pure sugar over the `for` above; see [Shorthands
and sugar](#iteration-ranges-and-for-each).

### Ranges and `for each`

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

### Repeating a fixed number of times

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

### Building a list with a loop, written as a value

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

## Subroutines

```
sub name(parameter: type, ...): type
  ...
  return value
end
```

Both the parameter list and the return type are optional. A subroutine with
neither is an *entry point*: either `main`, or a subroutine bound to a
component's event. `main` is where a console program starts; in a windowed
program it runs before the window appears, if the module has one at all.

```
sub add(a: int, b: int): int
  return a + b
end
```

A parameter is declared the way a variable is — `name: type` — and is
immutable inside the body: assigning to one would make the call site's
argument a lie about what the subroutine is working with. Copy it into a `var`
if you need working state.

A subroutine that declares a return type must `return` a value of that type on
every path; a subroutine that declares none may `return` with no value to leave
early. An `if` without an `else` is not a complete path, and a `while` never
counts as one — it may not run at all.

You call a subroutine exactly as you call a command: as a statement with
`call`, or anywhere in an expression when it returns a value.

```
module greeting

sub shout(who: text): text
  return uppercase(who) + "!"
end

sub main
  call print_text(shout("ada"))
end
```

`call` has one other form. `call through <pointer>(args): type` calls a
function whose *address* a program is holding — what a plug-in loader or a COM
vtable hands back — rather than one it can name; the call site carries the
signature, because there is no declaration to carry it. It belongs with the
rest of the foreign-function machinery, so it is described in [Calling a
function pointer](./interop.md#calling-a-function-pointer).

Commands and subroutines share one namespace, so a subroutine may not take a
library command's name — that would silently change what every existing call
in the file means, and the compiler says so instead.

A subroutine may call itself. Recursion needs no forward declaration:

```
module fibonacci

sub fib(n: int): int
  if n < 2
    return n
  end
  return fib(n - 1) + fib(n - 2)
end

sub main
  call print_int(fib(15))
end
```

An entry point and an event handler are the one place the shape is fixed by
someone other than you. `main` takes nothing and returns nothing — the runtime
that calls it has nothing to hand over. A subroutine bound to an event takes
exactly what the event hands it, or nothing at all, and returns nothing: a
`timer`'s `tick` hands the tick count, so its handler is `sub on_tick(n: int)`
or plain `sub on_tick`, and `sub on_tick(s: text)` is a compile error that
shows the header to paste. See [Components](./components.md).

Four shorthands let a call say less than it used to: a `let` may leave out its
type, a parameter may carry a default, an argument may name its parameter, and
a record may be written with braces. Each is rewritten into the header or the
call you would have typed — see [Saying less at the
call](#saying-less-at-the-call).

### Calling with a dot

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

### Saying less at the call

Four shorthands share one idea: the compiler already knows something you would
otherwise have to spell out — the type of a value, the position of an
argument, the fields you are not changing — so it lets you leave it unwritten.
Each is rewritten into what you would have typed, and nothing you can write
with one is a thing the language could not already do.

#### A binding can take its type from its value

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

#### A parameter can have a default

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

#### An argument can name its parameter

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

#### A record can be written with braces, and copied with `...`

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

### Documenting a subroutine

A comment beginning with two hashes, directly above a `sub`, a `dll` or a
`record`, is that symbol's documentation. The editor shows it when you hover
the name, wherever the name is used:

```
module greet

## Greet someone by name.
## The name is not checked — an empty one greets nobody in particular.
sub hello(who: text)
  call print_text("Hello, {who}.")
end

sub main
  call hello("Ada")
end
```

A single `#` comment stays what it always was: a note to whoever is reading
that line, and invisible everywhere else. The difference is deliberate — `##`
says *what this is for*, and a signature can only ever say what it takes.

## When something fails

There are no exceptions. A command that can fail returns a sentinel — `0` for
a handle or a position, `-1` for a count or size, `""` for text, `false` for a
yes/no — and leaves the reason in the *error slot*, which `last_error_code()`
and `last_error_text()` read.

```
module missing
use file

sub main
  let notes: text = file_read_text("notes.txt")
  if last_error_code() <> 0
    call print_text("could not read notes.txt: {last_error_text()}")
    return
  end
  call print_text(notes)
end
```

A command that succeeds clears the slot, and a command that cannot fail never
touches it, so a code left over from earlier is never mistaken for a fresh
failure. That is what makes `false` and `0` readable: `false` with code `0`
is a genuine no, and `0` from `find` means *not there* — nothing sits at
position 0.

Two shorthands write the two things a program does with a failure — carry on
with something else, or give up and pass it back — without spelling that `if`
out each time. [`otherwise`](#a-value-to-fall-back-on-otherwise) supplies the
value the failed call did not, and [`check`](#passing-a-failure-back-check)
returns from the subroutine the moment a call fails. A local may also be typed
[`T?`](#a-value-that-may-not-be-there-t), which is the checker refusing to let
a missing value be read as though it were there. All four are below.

### A value to fall back on: `otherwise`

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

### Passing a failure back: `check`

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

### A value that may not be there: `T?`

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

### Cleaning up on the way out: `defer`

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

### Checking as you go: `assert`

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

## Components

A component's properties are read and written with a dot:

```
greeting.text = "Ready."
button_ok.width = 200
```

Which properties exist depends on the component; the compiler checks both the
name and the type, so a typo is an error at build time rather than a control
that silently does nothing. See [Forms and events](./forms-and-events.md) for
the shape of a form, and [Components](./components.md) for what the
components are and what their events hand a handler.

The dot has one more job, and the parentheses are what tell it from this one:
a `.name` followed by `(` is the call `f(x, a)`, written left to right. See
[Calling with a dot](#calling-with-a-dot).
