//! KIR — Kiln's typed, monomorphic middle IR.
//!
//! The seam the backend consumes instead of `kiln_ir::Module`. Both front ends
//! — the 1.x lowerer and the K2 lowerer — produce KIR; [`emit`] turns KIR into
//! textual LLVM IR. The design is `design/k2/kir.md`.
//!
//! KIR carries no syntax and no sugar: name resolution, desugaring, generic
//! monomorphisation and lambda lifting all happen in the front end, so the
//! backend is a straight typed-tree → text walk with no registry and no
//! choices to make. Types are interned ([`TyId`] is `Copy`) to keep the
//! backend's by-value type ergonomics while admitting function, collection and
//! generic-instance types the 1.x `Ty` could not express.

use std::collections::HashMap;

pub mod build;
pub mod debug;
pub mod emit;

// ─── Interned types ─────────────────────────────────────────────────────────

/// A handle into a module's [`TyTable`]. `Copy`, cheap, and — because the table
/// interns — equal ids mean equal types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TyId(pub u32);

/// A handle into a module's `records` vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RecordId(pub u32);

/// A handle into a module's `funcs` vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FuncId(pub u32);

/// A handle into a function's `locals` vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LocalId(pub u32);

/// A handle into a module's `globals` vector.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlobalId(pub u32);

/// The shape of a type. Interned in a [`TyTable`]; refer to one by [`TyId`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKind {
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    /// Pointer-width signed / unsigned integer (`nint` / `nuint`).
    Nint,
    Nuint,
    F32,
    F64,
    /// A Unicode scalar, 32 bits.
    Char,
    /// `string`/`text`: a pointer to NUL-terminated UTF-8; `null` is empty.
    Str,
    /// A mutable byte buffer (0-based offsets).
    Bytes,
    /// An opaque interop pointer.
    Ptr,
    /// A record — value or C-layout, per its [`RecordDef`].
    Record(RecordId),
    /// A growable list of an element type; nests freely.
    Array(TyId),
    /// A keyed collection: key, value.
    Dict(TyId, TyId),
    /// A set of an element type.
    Set(TyId),
    /// `T?`: a value plus a "present" bit.
    Optional(TyId),
    /// A tuple of element types.
    Tuple(Vec<TyId>),
    /// A function value: a `{fn ptr, env ptr}` pair.
    Func {
        params: Vec<TyId>,
        ret: TyId,
    },
    /// The address of a C function: a bare code pointer, called with no
    /// environment in the given convention. `delegate* unmanaged<…>` in K2.
    CFunc {
        params: Vec<TyId>,
        ret: TyId,
        conv: CallConv,
    },
    /// Storage held in place inside a C-layout record: `count` elements of
    /// `elem` (a C `T name[count]`), or — with `count` 0 — one C-layout
    /// record nested by value. Only a field has this type; reading the field
    /// yields the address of that storage.
    Inline {
        elem: TyId,
        count: u32,
    },
    /// No value.
    Void,
}

/// The type table a module owns. Interns [`TyKind`]s to [`TyId`]s and answers
/// the derived queries the backend needs.
#[derive(Clone, Debug)]
pub struct TyTable {
    kinds: Vec<TyKind>,
    intern: HashMap<TyKind, TyId>,
    ptr_bits: u32,
}

impl TyTable {
    pub fn new(ptr_bits: u32) -> TyTable {
        let mut t = TyTable {
            kinds: Vec::new(),
            intern: HashMap::new(),
            ptr_bits,
        };
        // Intern the scalars up front so common ids are stable and cheap.
        for k in [
            TyKind::Void,
            TyKind::Bool,
            TyKind::I8,
            TyKind::U8,
            TyKind::I16,
            TyKind::U16,
            TyKind::I32,
            TyKind::U32,
            TyKind::I64,
            TyKind::U64,
            TyKind::Nint,
            TyKind::Nuint,
            TyKind::F32,
            TyKind::F64,
            TyKind::Char,
            TyKind::Str,
            TyKind::Bytes,
            TyKind::Ptr,
        ] {
            t.intern(k);
        }
        t
    }

    /// Intern a kind, returning its id. Equal kinds return the same id.
    pub fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.intern.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.intern.insert(kind, id);
        id
    }

    pub fn kind(&self, id: TyId) -> &TyKind {
        &self.kinds[id.0 as usize]
    }

    /// The id of an already-interned kind, if present.
    pub fn find(&self, kind: &TyKind) -> Option<TyId> {
        self.intern.get(kind).copied()
    }

    // Stable ids for the pre-interned scalars, so callers need no lookup.
    pub const VOID: TyId = TyId(0);
    pub const BOOL: TyId = TyId(1);
    pub const I8: TyId = TyId(2);
    pub const U8: TyId = TyId(3);
    pub const I16: TyId = TyId(4);
    pub const U16: TyId = TyId(5);
    pub const I32: TyId = TyId(6);
    pub const U32: TyId = TyId(7);
    pub const I64: TyId = TyId(8);
    pub const U64: TyId = TyId(9);
    pub const NINT: TyId = TyId(10);
    pub const NUINT: TyId = TyId(11);
    pub const F32: TyId = TyId(12);
    pub const F64: TyId = TyId(13);
    pub const CHAR: TyId = TyId(14);
    pub const STR: TyId = TyId(15);
    pub const BYTES: TyId = TyId(16);
    pub const PTR: TyId = TyId(17);

    /// The LLVM type a value of this type is held in.
    pub fn llvm(&self, id: TyId) -> String {
        match self.kind(id) {
            TyKind::Bool => "i1".into(),
            TyKind::I8 | TyKind::U8 => "i8".into(),
            TyKind::I16 | TyKind::U16 => "i16".into(),
            TyKind::I32 | TyKind::U32 | TyKind::Char => "i32".into(),
            TyKind::I64 | TyKind::U64 => "i64".into(),
            TyKind::Nint | TyKind::Nuint => format!("i{}", self.ptr_bits),
            TyKind::F32 => "float".into(),
            TyKind::F64 => "double".into(),
            TyKind::Str | TyKind::Bytes | TyKind::Ptr => "ptr".into(),
            TyKind::Array(_) | TyKind::Dict(..) | TyKind::Set(_) => "ptr".into(),
            // A managed record is a pointer; a C-layout record is a pointer to
            // its named struct. Either way the value is `ptr`.
            TyKind::Record(_) => "ptr".into(),
            // T? is the value beside a 1-bit "present" flag.
            TyKind::Optional(t) => format!("{{ {}, i1 }}", self.llvm(*t)),
            TyKind::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(|e| self.llvm(*e)).collect();
                format!("{{ {} }}", parts.join(", "))
            }
            // A function value is a closure pair: code pointer + environment.
            TyKind::Func { .. } => "{ ptr, ptr }".into(),
            TyKind::CFunc { .. } | TyKind::Inline { .. } => "ptr".into(),
            TyKind::Void => "void".into(),
        }
    }

    /// The width of a pointer in bytes. A pointer-like value is one, and it is
    /// also the alignment an aggregate is laid out to, so the debug emitter
    /// needs it to measure the `{ value, present }` pair an optional is.
    pub fn ptr_bytes(&self) -> u64 {
        (self.ptr_bits / 8) as u64
    }

    /// Whether a value of this type occupies the slot's pointer union — the
    /// aggregates and the pointer-like scalars. Drives slot marshalling.
    pub fn is_pointer(&self, id: TyId) -> bool {
        matches!(
            self.kind(id),
            TyKind::Str
                | TyKind::Bytes
                | TyKind::Ptr
                | TyKind::Array(_)
                | TyKind::Dict(..)
                | TyKind::Set(_)
                | TyKind::Record(_)
                | TyKind::CFunc { .. }
                | TyKind::Inline { .. }
        )
    }

    /// The LLVM type of a C-layout field as it sits in its struct: a nested
    /// record or an inline array in place, anything else as its value type.
    pub fn llvm_in_place(&self, id: TyId, record_name: impl Fn(RecordId) -> String) -> String {
        match self.kind(id) {
            TyKind::Inline { elem, count } => {
                let e = match self.kind(*elem) {
                    TyKind::Record(rid) => format!("%rec.{}", record_name(*rid)),
                    _ => self.llvm(*elem),
                };
                if *count == 0 {
                    e
                } else {
                    format!("[{count} x {e}]")
                }
            }
            _ => self.llvm(id),
        }
    }

    pub fn is_float(&self, id: TyId) -> bool {
        matches!(self.kind(id), TyKind::F32 | TyKind::F64)
    }

    /// Whether arithmetic and comparison on this type are unsigned.
    pub fn is_unsigned(&self, id: TyId) -> bool {
        matches!(
            self.kind(id),
            TyKind::U8 | TyKind::U16 | TyKind::U32 | TyKind::U64 | TyKind::Nuint | TyKind::Char
        )
    }

    /// The `SDT_*` tag this type crosses the slot ABI as (`abi/kiln_abi.h`).
    /// Sized ints narrower than the ABI's set surface as their widened kin,
    /// exactly as the 1.x backend does.
    pub fn sdt_tag(&self, id: TyId) -> i32 {
        const ARRAY: i32 = 0x100;
        const DICT: i32 = 0x200;
        match self.kind(id) {
            TyKind::Bool => 8,
            TyKind::I8
            | TyKind::U8
            | TyKind::I16
            | TyKind::U16
            | TyKind::I32
            | TyKind::U32
            | TyKind::Char => 3, // KN_SDT_INT
            TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => 4, // KN_SDT_INT64
            TyKind::F32 | TyKind::F64 => 6,                                // KN_SDT_DOUBLE
            TyKind::Str => 9,
            TyKind::Bytes => 10,
            TyKind::Ptr | TyKind::CFunc { .. } | TyKind::Inline { .. } => 14,
            TyKind::Record(_) => 13,
            TyKind::Array(e) => ARRAY | self.sdt_tag(*e),
            TyKind::Dict(_, v) => DICT | self.sdt_tag(*v),
            TyKind::Set(e) => ARRAY | self.sdt_tag(*e),
            // Never crosses the ABI in these shapes; kept total.
            TyKind::Optional(t) => self.sdt_tag(*t),
            TyKind::Tuple(_) | TyKind::Func { .. } => 14,
            TyKind::Void => 0,
        }
    }

    /// The zero / null operand for a value of this type.
    pub fn zero(&self, id: TyId) -> String {
        match self.kind(id) {
            TyKind::F32 | TyKind::F64 => "0.000000e+00".into(),
            k if is_ptr_kind(k) => "null".into(),
            _ => "0".into(),
        }
    }
}

fn is_ptr_kind(k: &TyKind) -> bool {
    matches!(
        k,
        TyKind::Str
            | TyKind::Bytes
            | TyKind::Ptr
            | TyKind::Array(_)
            | TyKind::Dict(..)
            | TyKind::Set(_)
            | TyKind::Record(_)
            | TyKind::CFunc { .. }
            | TyKind::Inline { .. }
    )
}

// ─── Module ─────────────────────────────────────────────────────────────────

/// Where a module's heap comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Allocator {
    /// libc `malloc`/`realloc`. Self-contained, and never freed.
    #[default]
    Libc,
    /// The Kiln collector, through `kn_notify`. Traced and reclaimed.
    Runtime,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModuleKind {
    Console,
    Gui,
    SharedLib,
    StaticLib,
}

/// The machine a module is emitted for: pointer width and OS. Mirrors the
/// backend's `TargetInfo` — only these two facts vary in the IR text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Target {
    pub ptr_bits: u32,
    pub windows: bool,
}

impl Target {
    pub const X86_64_LINUX: Target = Target {
        ptr_bits: 64,
        windows: false,
    };
    pub const X86_WINDOWS: Target = Target {
        ptr_bits: 32,
        windows: true,
    };
}

pub struct Module {
    pub name: String,
    pub target: Target,
    pub types: TyTable,
    pub records: Vec<RecordDef>,
    pub globals: Vec<GlobalDef>,
    pub funcs: Vec<Func>,
    pub entry: Option<FuncId>,
    pub kind: ModuleKind,
    /// The file this module was compiled from. `None` emits no debug
    /// information at all, exactly as the 1.x backend does.
    pub source: Option<String>,
    /// Where allocations come from. A record, a string and a collection all
    /// allocate; with the runtime linked they should be collectable.
    pub allocator: Allocator,
    /// Libraries a program names through `[Dll]` that are not linked into it:
    /// a call into one is resolved at its first call, by `kn_dll_get`, as 1.x
    /// resolves a `dll`. Anything not listed — libc, the runtime — is a
    /// symbol the link already provides.
    pub foreign_libraries: std::collections::BTreeSet<String>,
    /// What a library exports to a C host, in declaration order: the plain
    /// symbol, its parameters and its return type as C sees them. Empty for a
    /// program. The CLI writes the library's header from this.
    pub exports: Vec<ExportDef>,
}

/// One function a library exports under a plain C name.
#[derive(Clone, Debug)]
pub struct ExportDef {
    pub symbol: String,
    pub params: Vec<(String, TyId)>,
    pub ret: TyId,
}

impl Module {
    pub fn new(name: impl Into<String>, kind: ModuleKind, target: Target) -> Module {
        Module {
            name: name.into(),
            target,
            types: TyTable::new(target.ptr_bits),
            records: Vec::new(),
            globals: Vec::new(),
            funcs: Vec::new(),
            entry: None,
            kind,
            source: None,
            allocator: Allocator::Libc,
            foreign_libraries: Default::default(),
            exports: Vec::new(),
        }
    }

    /// Whether anything in the module calls a support-library command.
    ///
    /// A command is reached over the slot ABI and lives in the runtime, so a
    /// module that calls one cannot be linked against libc alone — whatever
    /// the front end chose for printing. The driver asks this rather than
    /// making the reader know which of `s.Length` and `s + t` is a command.
    pub fn calls_commands(&self) -> bool {
        fn in_expr(e: &Expr) -> bool {
            match e {
                Expr::Call(c) => match &**c {
                    Call::Command { .. } => true,
                    Call::Direct { args, .. } => args.iter().any(in_expr),
                    Call::Dll { args, .. } => args.iter().any(in_expr),
                    Call::Indirect { callee, args, .. } => {
                        in_expr(callee) || args.iter().any(in_expr)
                    }
                },
                Expr::Field(x, _)
                | Expr::Not(x)
                | Expr::Neg(x, _)
                | Expr::Cast { value: x, .. }
                | Expr::OptionalHasValue(x)
                | Expr::OptionalGet(x)
                | Expr::MakeClosure { env: x, .. } => in_expr(x),
                Expr::MakeOptional(_, x) => x.as_deref().is_some_and(in_expr),
                Expr::Index(a, b)
                | Expr::ElemPtr(a, b)
                | Expr::Bin(_, a, b, _)
                | Expr::FuncValue { fn_ptr: a, env: b } => in_expr(a) || in_expr(b),
                Expr::MakeRecord(_, xs) | Expr::MakeArray(_, xs) | Expr::MakeTuple(xs) => {
                    xs.iter().any(in_expr)
                }
                Expr::Int(..)
                | Expr::Float(..)
                | Expr::Bool(_)
                | Expr::Str(_)
                | Expr::Null(_)
                | Expr::Local(_)
                | Expr::Global(_)
                | Expr::FuncPtr(_) => false,
            }
        }
        fn in_place(p: &Place) -> bool {
            match p {
                Place::Local(_) | Place::Global(_) => false,
                Place::Field(e, _) => in_expr(e),
                Place::Index(a, b) => in_expr(a) || in_expr(b),
            }
        }
        fn in_stmts(body: &[Stmt]) -> bool {
            body.iter().any(|s| match s {
                Stmt::Let { value, .. } => in_expr(value),
                Stmt::Assign { place, value } => in_place(place) || in_expr(value),
                Stmt::Expr(e) => in_expr(e),
                Stmt::If { cond, then, els } => {
                    in_expr(cond) || in_stmts(then) || in_stmts(els)
                }
                Stmt::Loop { body } => in_stmts(body),
                Stmt::Return(e) => e.as_ref().is_some_and(in_expr),
                Stmt::Break | Stmt::Continue | Stmt::Line(_) => false,
            })
        }
        self.funcs.iter().any(|f| in_stmts(&f.body))
    }

    pub fn record(&self, id: RecordId) -> &RecordDef {
        &self.records[id.0 as usize]
    }
    pub fn func(&self, id: FuncId) -> &Func {
        &self.funcs[id.0 as usize]
    }
    pub fn global(&self, id: GlobalId) -> &GlobalDef {
        &self.globals[id.0 as usize]
    }
}

// ─── Records ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Equality {
    /// `record` — `==` compares fields.
    ByValue,
    /// `class` — `==` compares references.
    ByRef,
}

#[derive(Clone, Debug)]
pub enum Layout {
    /// Runtime-owned; fields reached through `kn_rec_*`.
    Managed,
    /// A flat C struct; the front end has computed everything.
    C {
        size: i64,
        align: i64,
        /// Byte offset of each field, in field order.
        offsets: Vec<i64>,
    },
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
}

#[derive(Clone, Debug)]
pub struct RecordDef {
    pub id: RecordId,
    pub name: String,
    pub fields: Vec<FieldDef>,
    pub layout: Layout,
    pub equality: Equality,
}

// ─── Globals ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct GlobalDef {
    pub id: GlobalId,
    pub name: String,
    pub ty: TyId,
    /// A pointer-typed global is a collector root in an executable.
    pub is_gc_root: bool,
}

// ─── Functions ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CallConv {
    Kiln,
    Cdecl,
    Stdcall,
    System,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Linkage {
    Internal,
    Exported,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
    /// The enclosing subprogram, for debug scoping. `0` = none.
    pub scope: usize,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: TyId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: TyId,
    pub span: Span,
    /// `Some(i)` if this local is parameter `i` (0-based), spilled to a slot.
    pub is_arg: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub id: FuncId,
    pub symbol: String,
    /// The line the function is declared on, for its subprogram entry.
    pub line: usize,
    /// The file it is declared in, when that is not the module's own source —
    /// a method of a type from a unit file beside the program. `None` is the
    /// module's `source`.
    pub file: Option<String>,
    pub params: Vec<Param>,
    pub ret: TyId,
    pub conv: CallConv,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
    pub linkage: Linkage,
    pub synthetic: bool,
}

// ─── Statements ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Stmt {
    /// Store into a local's slot.
    Let {
        local: LocalId,
        value: Expr,
    },
    /// Store into an l-value.
    Assign {
        place: Place,
        value: Expr,
    },
    /// A call whose value is dropped.
    Expr(Expr),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    /// A plain loop; leave with `Break`, restart with `Continue`.
    Loop {
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    /// A source line marker. The front end puts one before each statement it
    /// lowers, and the emitter attaches it to the instructions that follow —
    /// which is all a line table is.
    Line(usize),
}

/// The l-value subset.
#[derive(Clone, Debug)]
pub enum Place {
    Local(LocalId),
    Global(GlobalId),
    Field(Box<Expr>, usize),
    Index(Box<Expr>, Box<Expr>),
}

// ─── Expressions ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And, // bitwise / logical-on-i1
    Or,
    Xor,
    Shl,
    Shr,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int(i128, TyId),
    Float(f64, TyId),
    Bool(bool),
    Str(String),
    Null(TyId),

    Local(LocalId),
    Global(GlobalId),
    /// Read field `usize` of a record value.
    Field(Box<Expr>, usize),
    Index(Box<Expr>, Box<Expr>),

    /// A typed binary op; `TyId` is the operand type (drives signedness).
    Bin(BinOp, Box<Expr>, Box<Expr>, TyId),
    Not(Box<Expr>),
    Neg(Box<Expr>, TyId),
    /// Convert `value` to `to` (truncate / extend / reinterpret).
    Cast {
        value: Box<Expr>,
        to: TyId,
    },

    MakeRecord(RecordId, Vec<Expr>),
    MakeArray(TyId, Vec<Expr>),
    MakeTuple(Vec<Expr>),
    /// `Some(x)` / `None`.
    MakeOptional(TyId, Option<Box<Expr>>),
    OptionalHasValue(Box<Expr>),
    OptionalGet(Box<Expr>),

    /// A `{fn ptr, env ptr}` closure value.
    MakeClosure {
        func: FuncId,
        env: Box<Expr>,
    },

    /// The address of a function, as a `Ptr`. Lets a front end store a method
    /// in a table (an interface's dispatch record, say).
    FuncPtr(FuncId),

    /// A callable `{fn, env}` pair built from two pointer values, for dispatch
    /// through a table rather than to a statically known function.
    FuncValue {
        fn_ptr: Box<Expr>,
        env: Box<Expr>,
    },

    /// The address of an element of an array-like buffer, as a `Ptr`. Lets a
    /// front end compute an offset into a buffer without a pointer-arithmetic
    /// node of its own.
    ElemPtr(Box<Expr>, Box<Expr>),

    Call(Box<Call>),
}

/// A slot type carried on a command argument: its ABI tag plus the value type.
#[derive(Clone, Copy, Debug)]
pub struct SlotTy {
    pub tag: i32,
    pub ty: TyId,
}

#[derive(Clone, Debug)]
pub enum Call {
    /// A user/method/lifted function, by resolved symbol.
    Direct { func: FuncId, args: Vec<Expr> },
    /// A support-library command over the slot ABI.
    Command {
        symbol: String,
        args: Vec<Expr>,
        arg_slots: Vec<SlotTy>,
        ret: TyId,
    },
    /// An indirect typed call through a `Func` value (closure or fn pointer).
    Indirect {
        callee: Box<Expr>,
        args: Vec<Expr>,
        /// The `Func` type of the callee.
        sig: TyId,
    },
    /// A foreign function.
    Dll {
        library: String,
        symbol: String,
        conv: CallConv,
        args: Vec<Expr>,
        arg_tys: Vec<TyId>,
        ret: TyId,
        /// A C variadic (`printf`); the fixed args are `arg_tys`.
        varargs: bool,
    },
}
