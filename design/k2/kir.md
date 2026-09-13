# KIR — Kiln's middle IR

Status: **design, Phase 1**. KIR is the crate the backend consumes instead of
`kiln_ir::Module`. Both front ends — the 1.x lowerer (`k1-lower`, a thin shim
around today's parser/sema) and the K2 lowerer (`k2-lower`) — produce KIR; the
backend produces LLVM text from KIR and nothing else.

```
kiln_ir::Module ─┐                         ┌─▶ (1.x still ships)
  (1.x front)    ├─▶ *-lower ─▶ KIR ─▶ backend ─▶ .ll ─▶ clang
k2 AST ──────────┘                         └─▶ (K2 at the flip)
```

## Why KIR exists

The backend today reads `kiln_ir::Module` directly and re-derives, per call,
what a value's LLVM type is, which runtime symbol a command maps to, and how to
marshal each argument — pulling from `&Registry` as it goes. KIR moves every one
of those decisions to the front end, so the backend is a straight typed-tree →
text walk with no name resolution, no desugaring and no registry. That is what
lets two very different front ends share one emitter, and what makes the K2
features (generics, closures, methods, sized ints) expressible without touching
the backend once the port lands.

**Design rule:** if lowering a KIR node needs to look a name up in a side table
or choose between shapes, that decision belongs in the front end, not the
backend. KIR nodes are self-contained.

## Types are interned

The backend's ergonomics rest on `Ty: Copy` — `llvm_ty(t)`, `alloca(t)`,
`emit_ret_from_i64(t, …)` all take `Ty` by value. A tree type (for `Func<…>`,
`List<T>`, generic instances) would kill that. So KIR interns types:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyId(u32);          // Copy, cheap, passed by value everywhere

pub enum TyKind {
    // scalars — 1:1 with today's Ty
    Bool, I8, U8, I16, U16, I32, U32, I64, U64, Nint, Nuint,
    F32, F64, Char,
    Str,                       // `string`/`text`: ptr to NUL-terminated UTF-8
    Bytes,                     // mutable byte buffer (0-based)
    Ptr,                       // opaque interop pointer

    // aggregates
    Record(RecordId),          // reference record (value or C-layout — see def)
    Array(TyId),               // growable list; nests (Array(Array(I32)))
    Dict(TyId, TyId),          // key, value
    Set(TyId),
    Optional(TyId),            // T? — a value plus a "present" bit / null ptr
    Tuple(Box<[TyId]>),

    // functions
    Func { params: Box<[TyId]>, ret: TyId },   // a {fn ptr, env ptr} pair
    Void,
}

pub struct TyTable {           // owned by the KIR module
    kinds: Vec<TyKind>,        // TyId(n) -> kinds[n]
    intern: HashMap<TyKind, TyId>,
}
```

`TyTable` hands out `TyId`s and answers `kind(TyId) -> TyKind` and the derived
queries the backend needs (`llvm_ty`, `slot_tag`, `size`, `align`,
`is_reference`). Interning means equal types share an id, so `==` on `TyId` is
type equality. The old `Ty`/`Elem`/`CArray` split disappears: an element type is
just a `TyId`, and nesting is free.

## Module

```rust
pub struct Module {
    pub name: String,
    pub target: TargetInfo,          // ptr width + OS (unchanged from today)
    pub source: Option<PathBuf>,     // drives debug info; None = bare .ll
    pub types: TyTable,
    pub records: Vec<RecordDef>,     // both value-records and C-layout records
    pub globals: Vec<GlobalDef>,     // module-level vars; GC roots marked here
    pub funcs: Vec<Func>,            // every user sub, method, lambda body,
                                     //   handler thunk, attribute-generated fn
    pub forms: Vec<FormDef>,         // component build tables (see below)
    pub entry: Option<FuncId>,       // ECodeStart target; None for a library
    pub kind: ModuleKind,            // Console | Gui | SharedLib | StaticLib
}
```

Everything the backend currently reads out of `Registry` at lowering time is
pre-resolved into these vectors by the front end. The backend never sees a
`Registry`.

### RecordDef

```rust
pub struct RecordDef {
    pub id: RecordId,
    pub name: String,                // for debug info + mangling only
    pub fields: Vec<FieldDef>,       // { name, ty: TyId, // for C layout: }
    pub layout: Layout,              // Managed | C { size, align, offsets }
    pub equality: Equality,          // ByValue (record) | ByRef (class)
}
```

`Layout::C` carries the computed size/align/offsets so the backend does no
layout arithmetic — it already computes these in `c_record_size`/`c_field`;
that logic moves to the front end (or a shared `kir::layout` helper the front
end calls). `[Fixed(n)]` inline arrays and `[Packed]` are just field offsets
and a size the front end fills in.

## Functions

```rust
pub struct Func {
    pub id: FuncId,
    pub symbol: String,              // final LLVM symbol, already mangled
                                     //   (generic instances: K2Max$i32, etc.)
    pub params: Vec<Param>,          // { name, ty: TyId, span }
    pub ret: TyId,                   // Void for none
    pub conv: CallConv,              // Kiln | Cdecl | Stdcall | System
    pub locals: Vec<Local>,          // { name, ty, span, is_arg: Option<usize> }
    pub body: Vec<Stmt>,
    pub linkage: Linkage,            // Internal | Exported (library export)
    pub synthetic: bool,             // true => no debug info (thunks, wrappers)
}
```

There are no free "commands" here: a command is called, never defined, and its
definition lives in C. User subs, K2 methods (lowered to functions with an
explicit `this` first parameter), lifted lambdas and handler thunks are all just
`Func`s that differ only in how they were produced.

## Statements and expressions — no sugar

KIR statements are the small imperative set the backend already emits into:

```rust
pub enum Stmt {
    Let    { local: LocalId, value: Expr },        // store into a local's alloca
    Assign { place: Place, value: Expr },
    Expr(Expr),                                    // a call whose value is dropped
    If     { cond: Expr, then: Vec<Stmt>, els: Vec<Stmt> },
    Loop   { body: Vec<Stmt> },                    // plain loop; Break/Continue exit
    Break, Continue,
    Return(Option<Expr>),
    // defer is NOT a KIR node — the front end copies deferred statements to
    // every exit edge (return / break / continue / fall-through / `?`).
}

pub enum Expr {
    // literals & reads
    Int(i128, TyId), Float(f64, TyId), Bool(bool), Str(String), Null(TyId),
    Local(LocalId), Global(GlobalId), Field(Box<Expr>, usize /*index*/),
    Index(Box<Expr>, Box<Expr>),                   // list/dict/bytes element
    Slice(Box<Expr>, Box<Expr>, Box<Expr>),        // (1-based, inclusive) → helper

    // arithmetic / logic — typed, signedness explicit via operand TyId
    Bin(BinOp, Box<Expr>, Box<Expr>, TyId),        // Add..Rem, cmp, shifts, bitwise
    Not(Box<Expr>), Neg(Box<Expr>, TyId),
    Cast { value: Box<Expr>, to: TyId },           // truncate/extend/reinterpret

    // aggregates
    MakeRecord(RecordId, Vec<Expr>),
    MakeArray(TyId, Vec<Expr>),  MakeDict(TyId, TyId, Vec<(Expr, Expr)>),
    MakeTuple(Vec<Expr>),
    MakeOptional(TyId, Option<Box<Expr>>),         // Some(x) / None
    OptionalHasValue(Box<Expr>), OptionalGet(Box<Expr>),

    // closures
    MakeClosure { func: FuncId, env: Box<Expr> },  // {fn ptr, env record ptr}

    // the four call kinds
    Call(Call),
}

pub enum Call {
    // 1. a user/method/lambda-lifted function, by resolved symbol + conv
    Direct { func: FuncId, args: Vec<Expr> },
    // 2. a support-library command over the slot ABI — the front end has
    //    already resolved the runtime symbol and each arg/ret slot type
    Command { symbol: String, args: Vec<Expr>, arg_slots: Vec<SlotTy>, ret: TyId },
    // 3. an indirect typed call through a Func value (closure or fn pointer)
    Indirect { callee: Box<Expr>, args: Vec<Expr>, sig: TyId /*Func*/ },
    // 4. a foreign function: library, symbol, convention, C marshalling
    Dll { library: String, symbol: String, conv: CallConv,
          args: Vec<Expr>, arg_ctys: Vec<CTy>, ret_cty: CTy },
}
```

`Place` is the l-value subset (`Local`, `Global`, `Field`, `Index`) used by
`Assign` and by property setters.

**What the front end lowers away before KIR** (today these are backend methods —
`k1-lower` will do the same work producing the nodes above):

| 1.x / K2 surface | Lowers to |
|---|---|
| `otherwise` / `??` | `If (error-slot != 0) { fallback } else { tmp }` |
| `check` / `?` | `If (error-slot != 0) { Return err }` after the call |
| chained comparison `a<b<c` | `&&` of pairwise `Bin(cmp)` with each operand once |
| `in` / membership | a `Command` helper call or an `||` chain |
| list comprehension | `Loop` building an `Array` |
| `if`-expression, `switch`-expression | `If` chain into a temp local |
| `match` / `IfSome` | `If` on tag / `OptionalHasValue` + `OptionalGet` |
| `for each`, `for a..b` | `Loop` + a counter local (1-based bounds) |
| string interpolation | `Command` concat calls |
| `defer` | statements copied to every exit edge |

The runtime-helper operations that are **not** sugar stay as KIR nodes
(`Index`, `Slice`, `MakeArray`, `MakeDict`, `MakeRecord`, optional ops): they
are one op that lowers to one documented helper call, and both front ends want
them.

## Forms and components

The build tables the backend walks in `map_components`/`form_build` become data:

```rust
pub struct FormDef {
    pub name: String,
    pub root: Option<ComponentId>,        // the window; None for a bare component set
    pub components: Vec<ComponentDef>,    // creation order == handle order
}
pub struct ComponentDef {
    pub id: ComponentId,
    pub lib: Option<String>,              // owning library (ui, net, …)
    pub type_name: String,
    pub handle: u64,                      // compile-time constant, as today
    pub properties: Vec<(String, PropVal)>,
    pub handlers: Vec<(String /*event*/, HandlerRef)>,
}
pub enum HandlerRef {
    Sub(FuncId),                          // 1.x: a plain sub
    Closure { func: FuncId, env: GlobalId },   // K2: `Click += lambda` (ABI v5)
}
```

Handles stay compile-time integers. Property values are already-typed literals
or an `Expr` for the ones set at build time. ABI v5 (Phase 6) adds the `env`
pointer to `HandlerRef::Closure`; Phase 1 emits only `Sub`, matching today.

## Spans and debug info

Every `Func`, `Param`, `Local`, `Stmt` carries a `Span { line, col, end_col,
scope }`. `debug.rs` needs exactly `line`, `col` and the subprogram `scope`
(`describe_local`, `set_loc`, `prologue`), so KIR spans carry those and
`debug.rs` does not change in Phase 1. A `synthetic` func and any front-end-
generated statement carry no span (emit bare, as today).

## Phase 1 exit fixtures

Hand-written KIR (no parser yet) that must compile and run, proving KIR carries
what K2 needs and 1.x lacks:

1. **Closure with captured env** — a `MakeClosure` over an env record holding a
   `LocalId`; mutate the captured local *after* the closure is made and call it
   through `Indirect`; it must observe the new value (capture is **by
   reference**, per spec §8).
2. **Monomorphised generic** — two `Func`s `Max$i32`/`Max$f64` with mangled
   symbols and a `Direct` call to each.
3. **Method call** — a `Func` with an explicit `this: Record(..)` first param,
   called `Direct` with the receiver as arg 1.
4. **Unsigned arithmetic** — `Bin(Div, …, U32)` → `udiv`, `Bin(Lt, …, U16)` →
   `icmp ult`, `Bin(Shr, …, U32)` → `lshr`, on values above the signed range.

The 1.x suite passing through KIR proves the round-trip; these four prove the
reach.
