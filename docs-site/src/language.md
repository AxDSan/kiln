# Language guide

A whole program is one module. Files are UTF-8 and use the extension `.kiln`;
`#` starts a comment that runs to the end of the line. This page is the
reference; [A tour of the language](./tour.md) is the same material met in the
order a newcomer meets it.

```
# A module is a compilation unit.
module hello
target console

sub main
  call print_text("Hello.")
end
```

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
value](./sugar.md#a-binding-can-take-its-type-from-its-value):

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
[String interpolation](./sugar.md#string-interpolation).

Two other spellings exist for the two places that one is awkward: a **block**
between three quotes, whose newlines are the newlines themselves, and a **raw**
literal, `r"..."`, whose backslashes are backslashes. Both are the same text
value written a different way — see [Blocks and raw
text](./sugar.md#blocks-and-raw-text).

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
`a..b`](./sugar.md#taking-a-run-ab) and [Saying less at the
call](./sugar.md#saying-less-at-the-call).

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
call](./sugar.md#saying-less-at-the-call).

## Documenting a subroutine

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

## When a command fails

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
position 0. [A tour of the language](./tour.md) walks through this with a
program that grows.

Two shorthands write the two things a program does with a failure — carry on
with something else, or give up and pass it back — without spelling that `if`
out each time. [`otherwise`](./sugar.md#a-value-to-fall-back-on-otherwise)
supplies the value the failed call did not, and
[`check`](./sugar.md#passing-a-failure-back-check) returns from the subroutine
the moment a call fails. A local may also be typed
[`T?`](./sugar.md#a-value-that-may-not-be-there-t), which is the checker
refusing to let a missing value be read as though it were there.

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

## Shorthands and sugar

A shorthand is a spelling the compiler rewrites into something on this page
before the type checker ever sees it, so each obeys exactly the rules its
longhand does and carries no second meaning. They have a chapter of their own,
[Shorthands and sugar](./sugar.md), where each is set beside what it stands
for:

- **Values and text** — [compound assignment and the operator
  words](./sugar.md#assignment-and-operator-shorthands), [blocks and raw
  text](./sugar.md#blocks-and-raw-text), [string
  interpolation](./sugar.md#string-interpolation), and
  [`xs[a..b]`](./sugar.md#taking-a-run-ab) for a run of a collection.
- **Choosing and repeating** — [`if c then a else
  b`](./sugar.md#choosing-a-value) where a value goes,
  [`match`](./sugar.md#choosing-among-values-match) for the chain that tests one
  value, the [range and `for each`
  loops](./sugar.md#iteration-ranges-and-for-each), [`repeat N
  times`](./sugar.md#repeating-a-fixed-number-of-times), and [a list built by a
  loop](./sugar.md#building-a-list-with-a-loop-written-as-a-value).
- **Calling** — [the dot call](./sugar.md#calling-with-a-dot), and the four
  shorthands that let a call [say less](./sugar.md#saying-less-at-the-call): a
  `let` without a type, a parameter default, a named argument, and a record
  literal.
- **Failing and finishing** —
  [`otherwise`](./sugar.md#a-value-to-fall-back-on-otherwise),
  [`check`](./sugar.md#passing-a-failure-back-check), the optional
  [`T?`](./sugar.md#a-value-that-may-not-be-there-t), and
  [`defer`](./sugar.md#cleaning-up-on-the-way-out-defer).
- **Naming and checking** — [`enum`](./sugar.md#named-numbers-enum) for a run of
  named whole numbers, and [`assert`](./sugar.md#checking-as-you-go-assert) for
  what a debug build should stop on.

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

The same choice written where a *value* goes is [`if c then a else
b`](./sugar.md#choosing-a-value), and a chain that tests one value against
several is written once with
[`match`](./sugar.md#choosing-among-values-match). Both are shorthands for the
block above.

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

Three shorthands sit on top of this counting loop: the range loop
`for i in 1..n` and `for each x in xs`, which walk a collection without
spelling the index, and [`repeat N
times`](./sugar.md#repeating-a-fixed-number-of-times), which hides the counter
when nothing uses it. All are pure sugar over the `for` above; see [Shorthands
and sugar](./sugar.md#iteration-ranges-and-for-each).

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
[Calling with a dot](./sugar.md#calling-with-a-dot).
