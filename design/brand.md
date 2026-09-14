# Kiln — what it is, and how to say so

This file exists because an identity generated away from the product drifted
from it. A brand sheet produced by an image model described Kiln as *"a systems
language that treats compilation as craft"*. The artwork it came with was good
and is what ships; that sentence is wrong, and wrong in an expensive direction —
it recruits people who want Rust-class control over memory and lifetimes, who
will leave, and it repels the people Kiln is actually for.

So the words live here, in the tree, next to the thing they describe.

## What Kiln is

Kiln is a **RAD environment that compiles to native code**: you draw a form,
wire its events, and get one binary you can send to someone. English-first,
cross-platform, open source.

Kiln 2's language is C#-shaped — and compiles to a native binary with no .NET,
no interpreter and no runtime to install.

The lineage is Visual Basic 6 and Delphi — the tradition where the designer and
the source are the same artifact, where a double-click writes the handler, and
where "build" produces one file you can send to someone. The compiler is real
(LLVM, native code, no interpreter and no runtime to install), but the compiler
is not the pitch. **The pitch is that building a desktop app is quick again.**

## What the product says about itself

These are the words, and they are short because a slogan that needs a
paragraph is not one. Everything user-facing uses them:

| Where | Words |
|---|---|
| Splash | Draw the form · Wire the events · Ship one binary |
| Studio's status bar | Draw the form, wire the events, ship one binary |
| About | Draw a window, wire a handler, press Run. What comes out is one native binary with nothing to install beside it. |
| About, second line | An open, cross-platform heir to the VB6 and Delphi tradition: the form you draw is the source, and the compiler is real. |

**Do not ship a positioning note as a slogan.** "RAD is the identity ·
English-first · Cross-platform" was on the splash screen for a while. It is
three true things about the product and none of them is a sentence anyone
outside this repository would want to read; it describes the decision rather
than the thing decided. The rule: a user-facing line says what the product
*does for the reader*, in words they would use.

The same goes for lineage. "Built from EPL and BlackMoon concepts" was in the
About box: true, and meaningless to the person reading it. The VB6 and Delphi
comparison stays because people know what those were.

## Where this came from

Kiln began as an open implementation of Easy Programming Language (易语言,
EPL), and 1.x still is one — it is on the `1.x` branch and its documentation
still says so. Kiln 2 stops being that: the language is C#-shaped, and the
audience is people who think in C# and want a native binary without .NET.
What survives is the part that was never about EPL — the form you draw is the
source, and build produces one file you can send.

Until Kiln 2 becomes the default (plan Phase 10), public copy keeps
describing 1.x as shipped. At the flip, these replace the EPL lines:

| Room | Line |
| --- | --- |
| A line | Draw an app. Write it like C#. Ship one native binary. |
| A sentence | A C#-shaped language and form designer that compiles to one native binary — no runtime, no .NET, no metadata to reverse. |

Do not describe Kiln 2 as "C# without .NET" or a C# implementation: it is
not source-compatible with C#, indexes from 1, and has no exceptions or
inheritance. "C#-shaped" is the claim.

## What Kiln is not

Say none of these. Each one is a claim the product does not make and does not
want to defend:

- **A systems language.** No manual memory management, no borrow checker, no
  freestanding or kernel target. Memory is reclaimed by a collector, not by
  ownership. Anyone arriving expecting Zig or Rust has been mis-sold.
- **A general-purpose scripting language.** There is no REPL and the language
  is deliberately small. It exists to make GUI programs.
- **A transpiler, or a wrapper.** Kiln lowers to LLVM IR and links a native
  binary. It does not emit C, and it is not a binding layer over a UI toolkit.
- **A framework.** There is nothing to `import` into an existing project.

Also avoid "craft", "artisanal", and anything that makes compilation sound
like a lifestyle. The value is speed and directness, not romance.

## Saying it

The lockup line, in order of how much room there is:

| Room | Line |
| --- | --- |
| A word | Kiln |
| A line | Draw an app. Wire an event. Ship a native binary. |
| A sentence | An open, English-first, cross-platform implementation of Easy Programming Language: draw a form, wire its events, ship one native binary. |
| A paragraph | The README's opening. |

"Fire your program into binary" works as a companion line where the metaphor
has already been set up by the mark. It is not the primary line, because it
says how Kiln feels rather than what it does.

The name was chosen for its file extension as much as its meaning. `.bas`
belongs to Visual Basic and `.bsl` to 1C:Enterprise, so the short forms of the
earlier candidate were all taken; `.kiln` is free and needs no explanation. The
meaning happens to fit — soft material in, hard finished object out — which is
the compiler, not a philosophy of work.

## The marks

| File | What it is |
| --- | --- |
| `assets/kiln-wordmark.png` | The wordmark: amber-to-red gradient, rounded geometric sans, a hairline crack through `l` and `n` |
| `assets/kiln-icon.png` | The K as fired clay with a molten core, 256×256 |
| `assets/kiln-icon-64.png` | The same at 64×64, which is where it stops being legible |
| `assets/kiln-hero.png` | The photographic mark — cracked ceramic with the fire inside it — over the word and a tagline. The README's header and the handbook's, where there is room for it and the background is light |
| `assets/kiln-hero-mark.png` | The same mark cropped to the K alone, 600×600. The hero's baked-in word is dark type: on a dark page it reads as a rendering fault, so the landing page takes this and lets its own type say the name |
| `assets/kiln-mark-black.png` | The flat black K. What a *built app* ships as its icon, and what Studio wears — an app made with Kiln should not wear Kiln's fire |

Scale to fit, never to fill — the wordmark's aspect is fixed and stretching it
is the one thing a mark cannot survive. Both keep their transparency, so they
sit on light and dark equally.

Which mark where, because there are now five: the **hero** introduces the
product to someone who has not met it (README, handbook, landing page); the
**icon** is the product's own small identity (favicons, Studio's window); the
**black mark** belongs to what users build with it, not to Kiln itself; the
**wordmark** is for a wide strip beside text, like the handbook's sidebar.

The icon is detailed where the wordmark is flat. That is deliberate: the icon
is seen alone at small sizes and needs the texture to stay recognisable, and
the wordmark is seen beside text and would fight it.

## Palette

Kiln heat over ceramic neutrals.

| Token | Hex | Use |
| --- | --- | --- |
| Amber | `#D97706` | The primary accent — links, focus, the active thing |
| Orange | `#EA580C` | Hover and emphasis, the wordmark's midpoint |
| Ember | `#991B1B` | The wordmark's terminal, and error |
| Glow | `#FDBA74` | Tints and highlights on dark |
| Ink | `#0A0A0B` | Text on light, ground on dark |
| Slate | `#4B4B55` | Secondary text |
| Ash | `#9CA3AF` | Muted text and rules |
| Bone | `#FAFAFA` | Ground on light |
| Kiln green | `#16A34A` | Success only — never decoration |

Type is **Inter** for prose and **JetBrains Mono** for code, with the platform
stack behind both. Studio uses whatever the system provides; the site names
these.
