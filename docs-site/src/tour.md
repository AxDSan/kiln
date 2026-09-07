# A tour of the language

One program, grown a step at a time, meeting each part of the language where a
newcomer meets it. Every listing here is a whole module: copy one into a file
and `kiln run` it. The [Language guide](./language.md) is the same material
arranged as a reference.

## A value, and a line of output

```
module tour
target console

sub main
  let name: text = "Ada"
  let age: int = 36
  call print_text(name + " is " + int_to_text(age) + ".")
end
```

A type is written after a colon, or taken from the value when the value makes
it plain (see [Shorthands](./sugar.md#a-binding-can-take-its-type-from-its-value));
a `let` cannot be reassigned, a `var` can. Nothing converts on its own —
`int_to_text` is how a
number gets into a sentence, and leaving it out is a compile error rather than
a `36` that quietly became `"36"` somewhere.

## Positions count from 1

Everything that has a position — a character in text, an element of an array,
a row in a grid — is numbered from 1. The first character of `name` is at
position 1, and `substr` takes a start and a length:

```
module tour
target console

sub main
  let name: text = "Ada Lovelace"
  let space: int = find(name, " ")
  call print_text(substr(name, 1, space - 1))      # Ada
  call print_text(substr(name, space + 1, 8))      # Lovelace
  call print_int(find(name, "Byron"))              # 0
end
```

That last line is the reason for the choice. Because nothing sits at 0, `find`
can answer 0 for *not there*, and a position can never be confused with a
failure. A language that counts from 0 needs a `-1` for the same job, and a
loop from `0` to `count - 1`; here a loop runs `1` to the count.

## More than one value: arrays

An array is written `T[]`, built with `[...]`, and indexed with `[]`:

```
module tour
target console

sub main
  var scores: int[] = [72, 91, 58]
  scores = append(scores, 85)

  var total = 0
  for each s in scores
    total += s
  end
  call print_text("average: {total / count(scores)}")

  call sort(scores)
  call print_text("sorted: " + join(scores, ", "))
  call print_int(index_of(scores, 91))             # 4
  call print_int(index_of(scores, 100))            # 0 — not there
end
```

`append` hands back a new array rather than growing this one in place, so the
result is assigned back; `remove` and `sort` change the array they are given
and are called as statements. An empty list, `[]`, takes its element type from
the declaration, which is the only place it can get one.

## A subroutine that takes and gives

The average is worth naming. A parameter is declared the way a variable is,
and a return type goes after the parameter list:

```
module tour
target console

sub average(xs: int[]): int
  return 0 if count(xs) = 0
  var total = 0
  for each x in xs
    total += x
  end
  return total / count(xs)
end

sub main
  let scores: int[] = [72, 91, 58, 85]
  let nobody: int[] = []
  call print_int(average(scores))
  call print_int(average(nobody))
end
```

A subroutine is called exactly as a command is — as a statement with `call`,
or inside an expression when it returns a value — and it may be written below
the line that calls it. A parameter cannot be assigned to: the call site's
argument should still be what the subroutine is working with. An array crosses
the call as one reference, so passing a long one costs nothing.

## Several things about one thing: records

A score belongs to someone. A `record` names a group of fields, and is how a
subroutine gives back more than one value:

```
module tour
target console

record student
  name: text
  score: int
end

sub best(xs: student[]): student
  var top = xs[1]
  for i in 2..count(xs)
    if xs[i].score > top.score
      top = xs[i]
    end
  end
  return top
end

sub main
  let class = [student{name: "Ada", score: 91}, student{name: "Grace", score: 85}, student{name: "Alan", score: 72}]

  let winner = best(class)
  call print_text("{winner.name} scored {winner.score}")
end
```

Fields are given by name when a record is built and read with a dot. A record
is a reference, as an array is: `top = xs[i]` makes `top` another name for the
same student, not a copy, so a change through one name is seen through the
other.

## Found by name: dictionaries

When the question is *what did Ada score?* rather than *who came first?*, a
dictionary answers it directly. It is written `T{}`, keyed by text, and holds
one type of value:

```
module tour
target console

sub main
  var scores: int{} = {"Ada": 91, "Grace": 85}
  scores["Alan"] = 72

  call print_int(scores["Ada"])
  call print_int(dict_count(scores))

  for each name, score in scores
    call print_text("{name}: {score}")
  end

  if dict_has(scores, "Byron")
    call print_text("Byron sat the exam")
  else
    call print_text("no score for Byron")
  end
end
```

`for each` walks a dictionary in the order its keys were added, which is what
makes walking one reproducible. Asking for a key that is not there answers
the value type's sentinel — `0` here — which is why `dict_has` exists: it is
the only way to tell a missing entry from a stored `0`.

## When something goes wrong: the error slot

There are no exceptions. A command that can fail returns a sentinel — `0` for
a handle or a position, `-1` for a count, `""` for text, `false` for a yes/no
— and leaves the reason in the *error slot*, which `last_error_code()` and
`last_error_text()` read. Reading scores from a file that may not exist:

```
module tour
target console
use file

sub main
  let raw: text = file_read_text("scores.txt")
  if last_error_code() <> 0
    call print_text("could not read scores.txt: {last_error_text()}")
    return
  end

  let lines: text[] = split(trim(raw), "\n")
  var scores: int{} = {}
  for each line in lines
    let fields = split(line, ",")
    scores[fields[1]] = text_to_int(fields[2])
  end
  call print_text("{dict_count(scores)} scores read")
end
```

A command that succeeds clears the slot, and a command that cannot fail —
`concat`, arithmetic, `count` — never touches it, so a code left over from
earlier is never mistaken for a fresh failure and the check does not have to
sit on the very next line. The same rule is what makes `false` readable: a
`false` with code `0` is a genuine no, a `false` with a non-zero code is a
failure.

An index outside an array goes through the same slot: `lines[99]` in the
program above answers `""` and sets the code, rather than reading whatever
sits after the array.

## Where next

A program that reacts to something — a click, a tick, a request — is the
other half of the language: [Forms and events](./forms-and-events.md) is the
shape of a window, and [Components](./components.md) is what goes in one, and
beside one. Every command met above, and the rest, is in
[Commands](./reference-commands.md).
