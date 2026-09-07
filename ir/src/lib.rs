//! Kiln e-code IR — v0.1 (Phase 1).
//!
//! The keystone artifact: a typed, tree-structured IR.  Phase 1 grows
//! the v0 slice with `int64`/`double` slot types, **command return types** and
//! **call-expressions** (so core commands are usable inside expressions), a
//! shared **command registry** (`registry`), a reusable **type checker**
//! (`sema`), and an **IR validator** (`validate`.1).
//!
//! The schema still reserves room for the form / component / property / event
//! nodes that later phases add (see `Item`).  Byte-set (`SDT_BIN`) and the
//! aggregate storage ABI are specified but deferred to Phase 2 (they ride on the
//! runtime↔library notification channel / memory-ownership model.2/D4).
//!
//! Only the *text* encoding is implemented; the binary encoding stays deferred.

pub mod lexer;
pub mod parser;
pub mod desugar;
pub mod registry;
pub mod sema;
pub mod validate;

pub use desugar::expand_defer;
pub use parser::{parse, parse_with, ParseError, ParseOptions};
pub use registry::Registry;
pub use validate::{validate, ValidateError};

/// A name that lives for the rest of the process, so a type can carry one.
///
/// `Ty` is `Copy` — every `vars.get(..).copied()` in the checker depends on it,
/// and a `String` inside would end that. A record type is named, though, and a
/// diagnostic that cannot say *which* record is barely a diagnostic. Interning
/// buys both: the name is a `&'static str`, and the leak is bounded by the
/// number of distinct spellings in a compilation, not by how often they are
/// used.
pub fn intern(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = NAMES.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap();
    if let Some(found) = set.get(s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// A fixed-count inline array in a C-struct record: `rgb: byte[32]`.
///
/// `elem` is a c-record field type — a scalar, or the name of another `is c`
/// record — and `count` is at least 1. Held behind `Ty::CArray(&'static
/// CArray)` so `Ty` stays `Copy`; the derives are structural, so two leaked
/// `byte[32]`s compare and hash equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CArray {
    pub elem: Ty,
    pub count: u32,
}

/// Make the type `elem[count]`.
///
/// Leaks one small value per array field written in a program — a handful over
/// a build — which is what buys `Ty: Copy`, the property every
/// `vars.get(..).copied()` in the checker rests on.
pub fn carray(elem: Ty, count: u32) -> Ty {
    Ty::CArray(Box::leak(Box::new(CArray { elem, count })))
}

/// The bindings a `for each` introduces over a collection of type `coll`: the
/// element binding's type, and — for a dictionary — the value binding's type.
/// `None` when the type cannot be iterated.
///
/// This is the single place those element types are decided, so the checker
/// (which binds the names and their types) and the backend (which reads the
/// elements out) can never disagree about what one `for each` iterates:
///   * an array of `E` yields `E`;
///   * a byte-set yields `int` — each byte read as `0`..`255`, the way
///     `bytes_at` already surfaces one;
///   * a text yields a one-character `text` (`substr` cut to length 1);
///   * a dictionary of `V` yields its key (`text`) as the element and `V` as
///     the value, so the two-binding `for each k, v in d` reads both.
pub fn foreach_elem_types(coll: Ty) -> Option<(Ty, Option<Ty>)> {
    Some(match coll {
        Ty::Array(e) => (e.ty(), None),
        Ty::Bytes => (Ty::Int, None),
        Ty::Text => (Ty::Text, None),
        Ty::Dict(v) => (Ty::Text, Some(v.ty())),
        _ => return None,
    })
}

/// What an array holds.  A separate, deliberately small enum rather than a
/// `Box<Ty>`: arrays of arrays are out of scope, and spelling that in the type
/// itself means the checker never has to discover it — `Ty` also stays `Copy`,
/// which is what every `vars.get(..).copied()` in the checker relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Elem {
    Int,
    Int64,
    Double,
    Text,
    Bool,
    /// A record, named. Records are references — an element is the same pointer
    /// the slot carries — so a list of them costs exactly what a list of text
    /// costs.
    Record(&'static str),
}

impl Elem {
    /// The element type as an ordinary type.
    pub fn ty(self) -> Ty {
        match self {
            Elem::Int => Ty::Int,
            Elem::Int64 => Ty::Int64,
            Elem::Double => Ty::Double,
            Elem::Text => Ty::Text,
            Elem::Bool => Ty::Bool,
            Elem::Record(n) => Ty::Record(n),
        }
    }

    /// The element types a scalar can be stored as; `None` for anything an
    /// array cannot hold (an array, a byte-set).
    pub fn from_ty(t: Ty) -> Option<Elem> {
        Some(match t {
            Ty::Int => Elem::Int,
            Ty::Int64 => Elem::Int64,
            Ty::Double => Elem::Double,
            Ty::Text => Elem::Text,
            Ty::Bool => Elem::Bool,
            Ty::Record(n) => Elem::Record(n),
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        self.ty().as_str()
    }
}

/// Slot data-type tags — the ABI type system.  Phase 1
/// exposes the numeric + text core; Phase 3 adds the aggregates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    /// `SDT_INT` — 32-bit signed integer.
    Int,
    /// `SDT_INT64` — 64-bit signed integer.
    Int64,
    /// `SDT_DOUBLE` — 64-bit IEEE-754 float.
    Double,
    /// `SDT_TEXT` — pointer to a NUL-terminated string; NULL = empty.
    Text,
    /// `SDT_BOOL` — truth value. Carried as an int-sized value, matching the
    /// ABI's `BOOL`, so slot marshaling has one less width to juggle.
    Bool,
    /// `SDT_BIN` — a byte-set: raw bytes, the type a PNG lives in.
    Bytes,
    /// `SDT_PTR` — a raw machine pointer: an opaque 64-bit address for C
    /// interop. Held in the slot's pointer union like text, but with none of
    /// text's meaning: no length, no ownership, no auto-conversion to or from
    /// `int64`. Arithmetic on it is explicit (`ptr_offset`); `ptr_null()` is
    /// the zero address.
    Ptr,
    /// A single byte (`0`..`255`) — a C-layout `record` field type only. It
    /// exists so a c-record can match a real C struct with `char`/`uint8_t`
    /// members and the padding around them; `Ty::from_keyword` deliberately
    /// does not produce it, so `byte` stays an ordinary word everywhere but a
    /// c-record field, and a byte field is *read and written as `int`*
    /// (`Ty::surface` maps it) — the type never becomes a value's type, crosses
    /// a slot, or takes part in arithmetic under its own name.
    Byte,
    /// A 16-bit value — a C-layout `record` field type only, the `WORD` /
    /// `uint16_t` a Win32 struct is full of. Like `Byte` it has no life of its
    /// own: it is spelled `int16` or `word` in a c-record field, it is *read
    /// and written as `int`* (`Ty::surface` maps it), and reading one widens
    /// unsigned — `0`..`65535`, the same bargain `byte` makes, because a
    /// `WORD` field is unsigned far more often than it is a `SHORT`.
    Int16,
    /// A 32-bit IEEE float — a C-layout `record` field type only, spelled
    /// `float`. Kiln has one floating type, `double`, so a `float` field is
    /// *read and written as `double`* (`Ty::surface` maps it) and the narrowing
    /// happens at the store: the value in the struct is a real 4-byte `float`,
    /// which is what a C API that declares one expects.
    Float,
    /// A fixed-count inline array inside a C-layout `record`: `rgb: byte[32]`.
    /// The field is `count` elements laid end to end in the struct's own
    /// storage — not a pointer to a runtime array — which is what a C
    /// `BYTE rgbReserved[32]` member is.
    ///
    /// Held behind a leaked `&'static` rather than a `Box` so `Ty` stays
    /// `Copy`; equality and hashing go through the pointee, so two separately
    /// leaked `byte[32]`s are the same type.
    CArray(&'static CArray),
    /// An array of `Elem`. The value in the slot is a pointer to a
    /// runtime-owned array object, exactly as text is a pointer.
    Array(Elem),
    /// **Signature-only**: "an array, whatever it holds".
    ///
    /// `count`, `sort` and `join` do not care what is in the array — the array
    /// carries its element tag at run time. Without this they would need one
    /// command per element type, which is five spellings of one idea. It never
    /// names a variable: there is no surface syntax for it.
    AnyArray,
    /// **Signature-only**: "whatever THIS call's array argument holds".
    ///
    /// This is what makes `append(xs, 5)` on a `text[]` an error with a line
    /// number instead of a silently mistyped element.
    AnyElem,
    /// A record type, by name. The value in the slot is a pointer to a
    /// runtime-owned record, exactly as an array is.
    Record(&'static str),
    /// A dictionary from `text` keys to `Elem` values, spelled `int{}`.
    /// One value type per dictionary, so reading one out has a type without a
    /// run-time question.
    Dict(Elem),
    /// **Signature-only**: "a dictionary, whatever it holds" — the `AnyArray`
    /// of the keyed collection, and for the same reason: `dict_count` does not
    /// care, and five spellings of it would be five chances to disagree.
    AnyDict,
    /// `text?` — a value that may not be there.
    ///
    /// It is a **local's** type and nothing else's: an optional never crosses a
    /// slot, a parameter, a return, an array element or a record field, because
    /// it is not one value in the ABI's sense. What the backend keeps is the
    /// value in its own type plus a hidden truth value beside it saying whether
    /// the value is there — which is why the inner type is an `Elem`: a scalar
    /// or a record, the things that fit in one slot.
    ///
    /// The checker refuses to read one as a `T`. `EXPR otherwise FALLBACK`
    /// supplies the value that is not there, and `if some v as value` binds the
    /// one that is — those two are the only ways in, and both leave a plain `T`
    /// behind them.
    Optional(Elem),
}

impl Ty {
    /// The surface-syntax / spec spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Int64 => "int64",
            Ty::Double => "double",
            Ty::Text => "text",
            Ty::Bool => "bool",
            Ty::Bytes => "bytes",
            Ty::Ptr => "ptr",
            Ty::Byte => "byte",
            // `int16` rather than `word`: one spelling in a diagnostic, and
            // `word` is the alias a Win32 transcription reaches for.
            Ty::Int16 => "int16",
            Ty::Float => "float",
            Ty::CArray(a) => intern(&format!("{}[{}]", a.elem.as_str(), a.count)),
            Ty::Array(Elem::Int) => "int[]",
            Ty::Array(Elem::Int64) => "int64[]",
            Ty::Array(Elem::Double) => "double[]",
            Ty::Array(Elem::Text) => "text[]",
            Ty::Array(Elem::Bool) => "bool[]",
            // Read in a signature, not in a sentence: `kiln commands` shows
            // `append(array, element) -> array`. The diagnostics that need a
            // phrase spell it out themselves.
            Ty::AnyArray => "array",
            Ty::AnyElem => "element",
            Ty::Record(n) => n,
            // Interned rather than matched out: an element type that is itself
            // named has no fixed set of spellings to enumerate.
            Ty::Dict(e) => intern(&format!("{}{{}}", e.as_str())),
            Ty::AnyDict => "dictionary",
            Ty::Optional(e) => intern(&format!("{}?", e.as_str())),
            Ty::Array(Elem::Record(n)) => intern(&format!("{n}[]")),
        }
    }

    /// Parse a type keyword; `None` if unknown.  The `[]` suffix is the
    /// parser's business — it is syntax, not a keyword.
    pub fn from_keyword(s: &str) -> Option<Ty> {
        Some(match s {
            "int" => Ty::Int,
            "int64" => Ty::Int64,
            "double" => Ty::Double,
            "text" => Ty::Text,
            "bool" => Ty::Bool,
            "bytes" => Ty::Bytes,
            "ptr" => Ty::Ptr,
            _ => return None,
        })
    }

    /// The type a value of this field type *appears as* in the language.
    ///
    /// Every type is itself but the three c-record layout widths, which have no
    /// life of their own: a `byte` and an `int16` read and write as an `int`
    /// (`0`..`255` and `0`..`65535`, exactly as `ptr_read_byte` does for the
    /// first), and a `float` as a `double`. Keeping this in one place is what
    /// lets those three stay walled inside a c-record's field table.
    pub fn surface(self) -> Ty {
        match self {
            Ty::Byte | Ty::Int16 => Ty::Int,
            Ty::Float => Ty::Double,
            other => other,
        }
    }

    /// This field type's size and alignment in a C struct, on every 64-bit
    /// target Kiln emits (x86-64 SysV and Windows x64 agree for scalars).
    /// `None` for a type that has no by-value C layout — the aggregates and the
    /// signature-only tags — so the caller can reject it with a real message.
    pub fn c_size_align(self) -> Option<(i64, i64)> {
        Some(match self {
            Ty::Byte => (1, 1),
            // A `WORD`/`uint16_t` member: two bytes, aligned to two.
            Ty::Int16 => (2, 2),
            // A C `float` is four bytes aligned to four, whatever Kiln
            // reads it as.
            Ty::Float => (4, 4),
            Ty::Int => (4, 4),
            // C's `int`-sized truth, so a c-record `bool` lines up with a `BOOL`
            // / `int` field a C API declares.
            Ty::Bool => (4, 4),
            Ty::Int64 | Ty::Double => (8, 8),
            // A `char*` and a raw pointer are both one 8-byte machine word.
            Ty::Text | Ty::Ptr => (8, 8),
            _ => return None,
        })
    }

    /// Whether arithmetic operators (`+ - * /`) apply.
    pub fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::Int64 | Ty::Double)
    }

    /// What this array holds, or `None` if it is not one array in particular.
    pub fn elem(self) -> Option<Elem> {
        match self {
            Ty::Array(e) => Some(e),
            _ => None,
        }
    }

    /// What this dictionary holds, or `None` if it is not one dictionary in
    /// particular.
    pub fn value(self) -> Option<Elem> {
        match self {
            Ty::Dict(e) => Some(e),
            _ => None,
        }
    }

    /// Whether a value of this type is a pointer in the slot's value union —
    /// text and both aggregates. The backend marshals all of them identically.
    pub fn is_pointer(self) -> bool {
        matches!(
            self,
            Ty::Text
                | Ty::Bytes
                | Ty::Ptr
                | Ty::Array(_)
                | Ty::AnyArray
                | Ty::AnyElem
                | Ty::Record(_)
                | Ty::Dict(_)
                | Ty::AnyDict
        )
    }

    /// The ABI `SDT_*` numeric tag (must match `abi/kiln_abi.h`).
    ///
    /// Array-ness is a flag bit above the element tag rather than a block of
    /// new numbers, because every `SDT_*` value is frozen: `int[]` has to be
    /// expressible without moving `int`.
    pub fn sdt_tag(self) -> i32 {
        const ARRAY: i32 = 0x100; // KN_SDT_ARRAY_FLAG
        const DICT: i32 = 0x200; // KN_SDT_DICT_FLAG
        const RECORD: i32 = 13; // KN_SDT_RECORD
        const ALL: i32 = 255; // KN_SDT_ALL
        match self {
            Ty::Int => 3,    // KN_SDT_INT
            Ty::Int64 => 4,  // KN_SDT_INT64
            Ty::Double => 6, // KN_SDT_DOUBLE
            Ty::Text => 9,   // KN_SDT_TEXT
            Ty::Bool => 8,   // KN_SDT_BOOL
            Ty::Bytes => 10, // KN_SDT_BIN
            Ty::Ptr => 14,   // KN_SDT_PTR
            // A byte is a c-record layout type; it never crosses the slot ABI
            // (a byte field surfaces as `int`), so this arm exists only to keep
            // the match exhaustive and is never reached in marshaling.
            Ty::Byte | Ty::Int16 => 3, // read as KN_SDT_INT
            Ty::Float => 6,            // reads as KN_SDT_DOUBLE
            // An inline array is a c-record field type and nothing else: it
            // never becomes a value that crosses the slot ABI (indexing one
            // yields the element's surface type), so this arm only keeps the
            // match exhaustive.
            Ty::CArray(_) => 14, // an address, if it ever were one
            // An optional is a local's type, never a slot's: the value crosses
            // the ABI as the plain `T` it holds, with the "is it there" kept
            // beside it in the caller's own frame. This arm keeps the match
            // exhaustive and is never reached in marshaling.
            Ty::Optional(e) => e.ty().sdt_tag(),
            Ty::Array(e) => ARRAY | e.ty().sdt_tag(),
            Ty::AnyArray => ARRAY | ALL,
            Ty::AnyElem => ALL,
            // One tag for every record type: which record it is, is a
            // compile-time fact, and a field is reached by index, so nothing at
            // run time has to tell two records apart.
            Ty::Record(_) => RECORD,
            Ty::Dict(e) => DICT | e.ty().sdt_tag(),
            Ty::AnyDict => DICT | ALL,
        }
    }

    /// Map an ABI `SDT_*` tag back to an IR type; `None` for tags not modeled in
    /// this phase (or `KN_SDT_NULL`, used for void returns).
    pub fn from_sdt_tag(tag: i32) -> Option<Ty> {
        const ARRAY: i32 = 0x100;
        const DICT: i32 = 0x200;
        const ALL: i32 = 255;
        if tag == ARRAY | ALL {
            return Some(Ty::AnyArray);
        }
        if tag == DICT | ALL {
            return Some(Ty::AnyDict);
        }
        if tag & DICT != 0 {
            return Elem::from_ty(Ty::from_sdt_tag(tag & !DICT)?).map(Ty::Dict);
        }
        if tag & ARRAY != 0 {
            return Elem::from_ty(Ty::from_sdt_tag(tag & !ARRAY)?).map(Ty::Array);
        }
        Some(match tag {
            3 => Ty::Int,
            4 => Ty::Int64,
            6 => Ty::Double,
            9 => Ty::Text,
            8 => Ty::Bool,
            10 => Ty::Bytes,
            14 => Ty::Ptr,
            ALL => Ty::AnyElem,
            _ => return None,
        })
    }
}

/// A command's signature: parameter slot types plus an optional return slot
/// (`None` = a void command, callable only as a statement).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Option<Ty>,
    /// Parameter names, parallel to `params` — what a named argument
    /// (`connect(host: "x")`) is matched against. Empty for a library command,
    /// whose metadata carries types only; a call to one is positional, and the
    /// desugar says so rather than guessing a name.
    pub names: Vec<String>,
    /// The default value of each parameter, parallel to `params`; `None` is a
    /// required one. Only a `sub` may declare defaults — a `dll` names someone
    /// else's function, which has no say in what a missing argument means.
    pub defaults: Vec<Option<Expr>>,
}

impl Signature {
    /// A signature with types only: what a library command has, and what every
    /// caller that predates named arguments builds.
    pub fn simple(params: Vec<Ty>, ret: Option<Ty>) -> Signature {
        Signature {
            params,
            ret,
            ..Signature::default()
        }
    }

    /// The 1-based position of the parameter called `name`.
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name).map(|i| i + 1)
    }
}

/// Comparison operators. `=` is equality *in expression position*; assignment
/// exists only as a statement, so `if x = 5` cannot silently assign (the C
/// footgun is structurally impossible here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "<>",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
    /// Ordering comparisons apply to numbers only; `=`/`<>` also apply to text.
    pub fn is_ordering(self) -> bool {
        !matches!(self, CmpOp::Eq | CmpOp::Ne)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

/// A bitwise operator. Separate from `BinOp` because these are the operators
/// that work on a value's *bits* rather than its magnitude: they are defined
/// on `int` and `int64` only, they never touch `double`, and they are spelled
/// as words rather than as symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    /// `band` — bitwise AND. Masking, and the way a flag is tested.
    And,
    /// `bor` — bitwise OR. How flags are combined.
    Or,
    /// `bxor` — bitwise exclusive OR.
    Xor,
    /// `shl` — shift left.
    Shl,
    /// `shr` — shift right, keeping the sign (an arithmetic shift): a negative
    /// value stays negative. `ushr` is the one that shifts zeros in.
    Shr,
    /// `ushr` — shift right filling with zeros (a logical shift), which is what
    /// a value used as a bit pattern rather than as a number wants.
    Ushr,
}

impl BitOp {
    /// The word it is written with, for a diagnostic.
    pub fn word(self) -> &'static str {
        match self {
            BitOp::And => "band",
            BitOp::Or => "bor",
            BitOp::Xor => "bxor",
            BitOp::Shl => "shl",
            BitOp::Shr => "shr",
            BitOp::Ushr => "ushr",
        }
    }

    /// Shifts are the asymmetric ones: the right operand is a *count*, not a
    /// second value, so it carries its own type and the result takes the left
    /// operand's.
    pub fn is_shift(self) -> bool {
        matches!(self, BitOp::Shl | BitOp::Shr | BitOp::Ushr)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    /// `%` — remainder. On integers this is `mod_int`'s operator spelling; on
    /// doubles it is the IEEE remainder.
    Rem,
}

impl BinOp {
    pub fn symbol(self) -> char {
        match self {
            BinOp::Add => '+',
            BinOp::Sub => '-',
            BinOp::Mul => '*',
            BinOp::Div => '/',
            BinOp::Rem => '%',
        }
    }
}

/// An expression.  Every expression has a static `Ty` (see `sema`).
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Integer literal — typed `int` unless it overflows `i32` (then `int64`).
    IntLit(i64),
    /// A hex or binary literal (`0xFF`, `0b1010`, `0xDEAD_BEEF`), held as the
    /// bit pattern that was written.
    ///
    /// It is not an `IntLit` because a bit pattern has no sign until something
    /// says how wide it is, and the answer differs by context:
    ///
    /// * where an `int64` is wanted it is that pattern in 64 bits, so
    ///   `0x8000_0000` is 2147483648 — which is what a `DWORD` constant such as
    ///   `HKEY_CLASSES_ROOT` means when it reaches an `int64` parameter;
    /// * on its own, a pattern of 32 bits or fewer is an `int` with exactly
    ///   those bits, so `0x8000_0000` is -2147483648 and `0xFFFF_FFFF` is -1 —
    ///   which is what a mask means when it meets an `int`;
    /// * a pattern wider than 32 bits is an `int64` with exactly those bits.
    ///
    /// Folding either reading in the lexer would make the other unreachable,
    /// so the choice is deferred to the one place that knows: the type the
    /// destination declares.
    BitsLit(u64),
    /// Floating-point literal (`double`).
    DoubleLit(f64),
    /// Text literal (decoded, unescaped bytes).
    TextLit(String),
    /// Reference to a `let`-bound local.
    Var(String),
    /// Binary arithmetic; both operands must share one numeric type.
    Bin(BinOp, Box<Expr>, Box<Expr>),
    /// Call to a non-void command, used as a value.
    Call { cmd: String, args: Vec<Expr> },
    /// Read a component's property: `ok_button.text`.
    GetProperty { component: String, property: String },
    /// `true` / `false`.
    BoolLit(bool),
    /// A comparison; yields `bool`. A bare `a < b`. Two comparisons that share
    /// a middle — `1 <= x <= 12` — parse as `Chain`, not as this nested twice.
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    /// `lo <op1> mid <op2> hi` — two comparisons sharing the middle operand,
    /// the mathematical reading of `1 <= x <= 12`. It is sugar for
    /// `lo <op1> mid and mid <op2> hi`, with one difference the plain
    /// conjunction cannot express: `mid` is evaluated **once**, so a call in the
    /// middle runs a single time. The backend binds `lo` and `mid` to temps in
    /// that order and then lowers exactly the conjunction; `hi` stays lazy
    /// behind the `and`'s short circuit. `Cmp`'s rules type each half.
    Chain {
        lo: Box<Expr>,
        lo_op: CmpOp,
        mid: Box<Expr>,
        hi_op: CmpOp,
        hi: Box<Expr>,
    },
    /// `e in xs`, `k in d`, `sub in text` (and each with `not in`) — a
    /// membership test yielding `bool`. It is sugar that lowers, once the
    /// checker knows what `haystack` is, to the command that already answers the
    /// question: `index_of(xs, e) <> 0` for an array (0 is "absent", positions
    /// counting from 1), `dict_has(d, k)` for a dictionary, and
    /// `find(text, sub) <> 0` for a substring. `not in` wraps the result in
    /// `not`. `find` rather than the `text` library's `contains`, so membership
    /// needs no `use`.
    In {
        needle: Box<Expr>,
        haystack: Box<Expr>,
        negated: bool,
    },
    /// One hole of a string interpolation — `{expr}` inside a `"..."` — turned
    /// to `text`. It is the single piece a text literal's holes desugar to: the
    /// literal `"Row {i} of {n}"` becomes the left-folded concat chain
    /// `concat(concat(concat("Row ", ToText(i)), " of "), ToText(n))`, with the
    /// literal chunks as plain `TextLit`s and each hole wrapped here. The parser
    /// builds that chain, so this is the only new node interpolation needs; the
    /// per-type conversion (text as-is, bool to `true`/`false`, a number through
    /// `int_to_text`/`int64_to_text`/`double_to_text`) is the same one a
    /// component property assignment already performs, so the backend lowers a
    /// hole exactly as it renders `label.text = <value>`. A type with no text
    /// form — a `ptr`, an array, a record — is a build error, and `hole` keeps
    /// the hole's source spelling so that error can name it.
    ToText {
        value: Box<Expr>,
        hole: String,
    },
    /// `if COND then A else B` used **as a value** — the conditional
    /// expression. Both arms must have one type, and that is the type of the
    /// whole thing; exactly one arm is evaluated.
    ///
    /// It is the same choice the block `if` makes, written where a value is
    /// wanted rather than where a statement is, which is what lets
    /// `let label: text = if n = 1 then "item" else "items"` be one line
    /// instead of a `var` and four. The two forms never collide: a statement
    /// that begins with `if` is always the block, and this one is reached only
    /// from expression position, where `if` began nothing before. `then` is a
    /// soft keyword — it means this only between an `if` condition and its
    /// first arm, so a variable named `then` is untouched.
    IfElse {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// `EXPR otherwise FALLBACK` — the value of `EXPR`, unless the call in it
    /// failed, in which case `FALLBACK`.
    ///
    /// A fallible command reports failure by leaving a code in the error slot
    /// (see `last_error_code`), so this is sugar for exactly what a program
    /// writes by hand today: run `EXPR` into a temporary, and then
    /// `if last_error_code() <> 0 then FALLBACK else <that temporary>`. The
    /// backend lowers it as that `IfElse`, so there is one conditional
    /// semantics and not two. Both sides must have one type.
    ///
    /// It does **not** clear the slot: `last_error_code()` still reports what
    /// went wrong afterwards, which is what lets a fallback and a log coexist.
    /// One `otherwise` per expression — a second would test a slot the first
    /// fallback did not clear and take the last arm every time, so it is
    /// refused rather than quietly wrong.
    Otherwise {
        value: Box<Expr>,
        fallback: Box<Expr>,
    },
    /// `[EXPR for each x in xs where COND]` — the list a loop would have built.
    ///
    /// It is the `for each` the language already has, written as a value: a
    /// fresh array, the same loop over the same four collection kinds, an
    /// `append` of `body` each turn, and — when `cond` is written — an `if`
    /// around that append. The backend builds exactly those statements and
    /// hands back the array, so a comprehension can do nothing a hand-written
    /// loop could not, and iterates in the one place every other loop does.
    ///
    /// The bindings are the `for each` header's, in full: `elem`, the `, VALUE`
    /// a dictionary's value binds to, and the `at IDX` position counting from 1.
    /// `holds` is the element type, which the parser cannot know (it is the type
    /// of `body` under those bindings) — the desugar fills it in, the way it
    /// answers every other question only the registry can.
    Comprehension {
        body: Box<Expr>,
        elem: String,
        value: Option<String>,
        index: Option<String>,
        coll: Box<Expr>,
        cond: Option<Box<Expr>>,
        holds: Option<Elem>,
    },
    /// `none` — the optional that holds nothing.
    ///
    /// It has no type of its own, exactly as `[]` has none: what it is the
    /// absence *of* comes from the declaration it is written under, so
    /// `let v: text? = none` is where it means something and `let v = none` is
    /// refused.
    NoneLit,
    /// Whether an optional holds a value — the hidden truth value beside it.
    ///
    /// Internal: no program writes this. `if some v as value` and
    /// `v otherwise d` are the two spellings, and both produce it.
    HasValue(Box<Expr>),
    /// The value inside an optional, read where something has already proved it
    /// is there. Internal, for the same reason [`Expr::HasValue`] is: it is one
    /// half of what `if some` and `otherwise` expand to, and unguarded it would
    /// read a value that was never stored.
    Unwrap(Box<Expr>),
    /// `and` / `or`, short-circuiting.
    Logical(LogicalOp, Box<Expr>, Box<Expr>),
    /// `not EXPR`.
    Not(Box<Expr>),
    /// `a band b`, `x shl 8` — a bitwise operation on `int` or `int64`.
    Bit(BitOp, Box<Expr>, Box<Expr>),
    /// `bnot EXPR` — every bit flipped. The bitwise counterpart of `not`,
    /// which is the one for truth values.
    BitNot(Box<Expr>),
    /// `xs[i]` — one element of an array, or one byte of a byte-set (which
    /// reads as an `int` 0..255). Bounds are checked at run time; a constant
    /// index the checker can see is checked before the program is built.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// `xs[2..5]`, `s[6..]`, `bs[..3]` — a slice: the run of a collection from
    /// one position to another, **inclusive at both ends**, positions counting
    /// from 1 like every other position in the language.
    ///
    /// It is sugar that becomes a concrete command once the checker knows what
    /// `base` is — the same bargain `in` makes — because the three things it
    /// runs over answer to three different commands: `substr` for text,
    /// `bytes_slice` for a byte-set, `slice` for an array. A missing bound is
    /// the collection's own end: `from` absent is 1, `to` absent is its length,
    /// and `xs[..]` is a copy.
    ///
    /// Out-of-range bounds are clamped rather than refused, which is what
    /// `substr` has always done — a start below 1 reads from 1, a `to` past the
    /// end stops at the end, and a `to` before the `from` is an empty result.
    /// A slice is where a program *asks* how much is there, so failing would
    /// mean writing the bounds check the slice was supposed to be.
    Slice {
        base: Box<Expr>,
        /// The first position, or `None` for "from the start".
        from: Option<Box<Expr>>,
        /// The last position, **included**, or `None` for "to the end".
        to: Option<Box<Expr>>,
    },
    /// `[a, b, c]` — a new array. Every element must share one type; an empty
    /// `[]` takes its type from where it is going, which is why the checker
    /// carries an expected type rather than inferring bottom-up alone.
    ArrayLit(Vec<Expr>),
    /// `point(x: 1, y: 2)` — a new record, every field named.
    ///
    /// Named rather than positional because the three mistakes worth catching
    /// — an unknown field, a wrong type, a missing one — are only *nameable*
    /// this way; positional construction can report the last two and has no
    /// word for the first.
    RecordLit {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    /// `point{..p, y: 9}` — a copy of `p` with some fields replaced.
    ///
    /// Sugar with a lifetime of one pass: the desugar reads the record's field
    /// list and rewrites it into the `RecordLit` the author would have written
    /// by hand, with every field they left out spelled `base.field`. `base` is
    /// restricted to a *place* (a name, or a field/index path from one) so that
    /// reading it once per field is the same as reading it once.
    RecordUpdate {
        name: String,
        base: Box<Expr>,
        fields: Vec<(String, Expr)>,
    },
    /// `port: 8080` in an argument list — a named argument, on its way to the
    /// slot the callee named `port`.
    ///
    /// It exists only between the parser and the desugar, which matches every
    /// label to a parameter and hands the rest of the compiler the plain
    /// positional call. Anything that meets one later is looking at an argument
    /// list that was never resolved, and says so.
    Labeled { name: String, value: Box<Expr> },
    /// `EXPR.name` — one field of a record.
    ///
    /// `p.x` on its own arrives as `GetProperty` (a component read and a field
    /// read are the same three tokens); this is what a `.` further along a
    /// chain becomes, so `people[1].name` works.
    Field { base: Box<Expr>, name: String },
    /// `{"a": 1, "b": 2}` — a new dictionary. Every key is `text` and every
    /// value shares one type; an empty `{}` takes its type from where it is
    /// going, exactly as `[]` does.
    DictLit(Vec<(Expr, Expr)>),
    /// `-EXPR` — arithmetic negation of a numeric value.
    ///
    /// Negated *literals* never reach here: the parser folds `-5` into
    /// `IntLit(-5)` so that it types `int` (an unfolded `2147483648` would
    /// type `int64` and make `let x: int = -2147483648` fail), and so that a
    /// form property, which must be a literal, can be negative.
    Neg(Box<Expr>),
    /// `address of NAME` — the address of a subroutine as a `ptr`, so it can be
    /// handed to a C API that calls back into it (a `CreateThread` ThreadProc,
    /// an `EnumWindows` callback, a hook detour). The named sub must have a
    /// C-representable signature; the checker resolves the name and proves that,
    /// because only then is the emitted function pointer one C can actually call.
    /// Holds a name, not an arbitrary expression: the only things with an
    /// address to take are a subroutine, a c-record local, and a place inside
    /// one — all of which a path of identifiers names.
    ///
    /// The operand may also be a **path into a c-record local** —
    /// `address of r.pt`, `address of r.rgb` — which is the address of that
    /// field inside the struct's own storage (for an inline array, of its first
    /// element). It is spelled here as the dotted path in one `String`, because
    /// a subroutine name can never contain a `.`, so the two readings never
    /// collide and every existing consumer of a bare name is unchanged.
    AddressOf(String),
    /// `size of TYPE` — the size in bytes of a type's C layout, an `int64`
    /// compile-time constant the backend folds to a number. For a c-record it
    /// is the flat struct's `sizeof` (with padding); for a scalar it is that
    /// scalar's C width. It is what a program passes to `mem_alloc`, or to a C
    /// API that wants the size of the struct it is being handed.
    SizeOf(Ty),
    /// `call through EXPR(args...): RetType` — a call to a function whose
    /// address is only known at run time.
    ///
    /// This is the counterpart of a `dll` line. A `dll` names a symbol the
    /// linker resolves; this names a `ptr` the program is holding —
    /// `GetProcAddress` handed it back, or it was read out of a COM vtable, or
    /// a plugin registered it — and the *call site* supplies the C signature
    /// that a `dll` declaration would otherwise have carried. The argument
    /// types are whatever the argument expressions are; `ret` is the `: T`
    /// after the parentheses, and `None` is a C `void` call.
    CallThrough {
        /// The address to call. Must type as `ptr`.
        callee: Box<Expr>,
        args: Vec<Expr>,
        ret: Option<Ty>,
        /// A trailing convention marker, as on a `dll` — a no-op on every
        /// 64-bit target, carried for a future 32-bit backend.
        conv: Option<CallConv>,
    },
    /// The implicit initializer of `var r: RECT` written with no `= EXPR`: a
    /// c-record local whose flat storage starts all-zero. It carries no type of
    /// its own — the `let`'s declared type says which c-record — so the checker
    /// only ever sees it with a hint, and it is valid nowhere else.
    ZeroInit,
}

/// A source position: a 1-based line and 1-based **byte** columns, `end_col`
/// one past the last byte. All zero when the position is not known.
///
/// Bytes rather than characters because that is what the lexer counts; the
/// language server converts to UTF-16 at its edge, and nothing in between
/// needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

impl Span {
    pub fn new(line: usize, col: usize, end_col: usize) -> Span {
        Span { line, col, end_col }
    }

    /// A whole line, when nothing narrower is known.
    pub fn line(line: usize) -> Span {
        Span { line, col: 0, end_col: 0 }
    }
}

/// An identifier as written: its spelling and where. A statement keeps the
/// ones on its header line so a diagnostic about `x` can underline `x` and
/// not the line it sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

/// A statement plus where it came from.
///
/// The position lives on the wrapper rather than inside each variant so that
/// existing pattern matches keep working, and so that a diagnostic can always
/// say *where* — an error without a position is nearly useless in an editor.
#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    /// 1-based source line; 0 when unknown.
    pub line: usize,
    /// The statement's header line — for `if`, `while` and `for` the header
    /// only, since the body is made of statements with positions of their own.
    pub span: Span,
    /// Every identifier on the header line, in source order. Expressions carry
    /// no positions of their own, so this is how a diagnostic about a name
    /// inside one finds its column.
    pub idents: Vec<Ident>,
}

impl Stmt {
    pub fn new(kind: StmtKind, line: usize) -> Stmt {
        Stmt {
            kind,
            line,
            span: Span::line(line),
            idents: Vec::new(),
        }
    }

    /// Where `name` is written on this statement's header line, if it is.
    pub fn ident_span(&self, name: &str) -> Option<Span> {
        self.idents.iter().find(|i| i.name == name).map(|i| i.span)
    }
}

/// A single statement inside a subroutine body.
#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    /// `let NAME = EXPR` — a binding whose type is still to be read off the
    /// initializer.
    ///
    /// The parser cannot type an expression (a command's return type lives in
    /// the registry, a local's in the enclosing scope), so an unannotated `let`
    /// arrives as this and the desugar turns it into the `Let` the author could
    /// have written. Nothing downstream ever sees one.
    LetInfer {
        name: String,
        value: Expr,
        mutable: bool,
    },
    /// `let NAME: TY = EXPR` (immutable) or `var NAME: TY = EXPR` (mutable).
    Let {
        name: String,
        ty: Ty,
        value: Expr,
        mutable: bool,
    },
    /// `NAME = EXPR` — assign to a mutable variable (local or module-level).
    Assign { name: String, value: Expr },
    /// `call CMD(args...)` — a call in statement position; a non-void return is
    /// discarded.
    Call { cmd: String, args: Vec<Expr> },
    /// `call through EXPR(args...)` in statement position — the same indirect
    /// call as the expression, with any result discarded. A `: T` may still be
    /// written (a C function that returns a value the program does not want),
    /// exactly as `call add(1, 2)` discards a `sub`'s result.
    CallThrough {
        callee: Expr,
        args: Vec<Expr>,
        ret: Option<Ty>,
        conv: Option<CallConv>,
    },
    /// `xs[i] = EXPR` — replace one element in place.
    ///
    /// This changes the array, not the name, so it is allowed on a `let`: the
    /// binding still refers to the same array. `let` promises the name will
    /// not be re-pointed, exactly as it does for a component id.
    SetIndex {
        name: String,
        index: Expr,
        value: Expr,
    },
    /// `ok_button.text = EXPR` — assign a component property at run time.
    SetProperty {
        component: String,
        property: String,
        value: Expr,
    },
    /// `r.pt.x = EXPR`, `r.rgb[3] = EXPR` — assign through a *path* into a
    /// c-record's flat storage: a nested field, or one element of an inline
    /// array, however deep.
    ///
    /// `place` is the same `Expr` the reader would evaluate (a `Field` /
    /// `Index` chain rooted at a variable), so the checker types a write with
    /// exactly the rules it types a read with — there is no second grammar for
    /// the left-hand side. The single-step forms stay `SetProperty` and
    /// `SetIndex`: those reach heap records, arrays and component properties as
    /// well, and a path reaches only a c-record.
    SetPlace { place: Expr, value: Expr },
    /// `if COND ... else if COND ... else ... end`.
    ///
    /// `arms` holds the `if`/`else if` conditions with their bodies, in order;
    /// `otherwise` is the optional final `else`.
    If {
        arms: Vec<(Expr, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    /// `while COND ... end`.
    While { cond: Expr, body: Vec<Stmt> },
    /// `match E` / `when V1, V2: ...` / `else: ...` / `end`.
    ///
    /// It is an if/else-if chain that compares one value with `=`, and it is
    /// lowered to exactly that. It earns a node of its own for the reason
    /// `ForEach` does: the chain must test **one** evaluation of `E`, so `E`
    /// has to be bound to a hidden local first, and a `let` needs its type
    /// written out — which the parser cannot know. So the binding is made where
    /// the type is: the checker types `E` and the backend pins it to a slot,
    /// after which every arm is the ordinary `Cmp(Eq)` a hand-written
    /// `if e = v1 or e = v2` would be.
    ///
    /// `arms` holds each `when` with its values (more than one is "any of
    /// these") and its body, in source order; `otherwise` is the optional
    /// final `else`. A `match` with no arm matching and no `else` does nothing,
    /// exactly as an `if` with no `else` does.
    Match {
        scrutinee: Expr,
        arms: Vec<(Vec<Expr>, Vec<Stmt>)>,
        otherwise: Option<Vec<Stmt>>,
    },
    /// `for NAME = START to LIMIT [step K] ... end`.
    ///
    /// `NAME` is an `int` that exists for the loop and is immutable inside it.
    /// `start` and `limit` are evaluated **once**, before the first iteration,
    /// so a loop cannot be lengthened by its own body. `step` is a non-zero
    /// integer literal: knowing its sign at compile time is what lets the
    /// comparison be `<=` for a counting-up loop and `>=` for a counting-down
    /// one, with no run-time test.
    For {
        var: String,
        start: Expr,
        limit: Expr,
        step: i64,
        body: Vec<Stmt>,
    },
    /// `for each ELEM [at IDX] in COLL` — and, for a dictionary, the two-binding
    /// `for each KEY, VALUE in COLL`.
    ///
    /// It is sugar for a 1-based counted loop over the collection, but it earns
    /// a node of its own for one reason the other sugars did not need: the
    /// element's type is not known until `coll` is typed, so the binding cannot
    /// be spelled with a `let` in the parser (a `let` needs its type written
    /// out). The checker infers the binding types from the collection and the
    /// backend lowers this to exactly the loop a hand-written `for` would — a
    /// hidden `int` counter from 1 to the collection's length, the element read
    /// out by index each turn — so there is no second loop semantics, only one
    /// spelling that stands in for the counter.
    ///
    /// `elem` binds each element (each array item, each byte as an `int`, each
    /// character as a one-character `text`, or each dictionary key as `text`).
    /// `value` is the dictionary value binding of the two-binding form, `None`
    /// otherwise. `index` is the optional `at IDX`, the 1-based position. All
    /// three are fresh and immutable inside the body, like a `for` counter.
    /// `coll` is evaluated once, before the first turn, so a body that grows the
    /// collection does not lengthen the loop.
    ForEach {
        elem: String,
        value: Option<String>,
        index: Option<String>,
        coll: Expr,
        body: Vec<Stmt>,
    },
    /// `break` — leave the innermost loop.
    Break,
    /// `continue` — skip to the innermost loop's next iteration.
    Continue,
    /// `return` (from a sub with no return type) or `return EXPR`.
    Return { value: Option<Expr> },
    /// `if some EXPR as NAME` — run the body with the optional's value bound to
    /// `NAME`, and only when there is one.
    ///
    /// It is the test and the unwrapping in one line, because separately they
    /// are two chances to write the second without the first. The desugar turns
    /// it into the `if` it stands for — the hidden truth value as the condition,
    /// a `let NAME` reading the value as the arm's first statement — so what
    /// runs is an ordinary conditional and `NAME` is an ordinary local, typed
    /// `T` and not `T?`, which is what makes the body's uses of it legal.
    ///
    /// The `else` runs when the value is absent, and cannot see `NAME`.
    IfSome {
        value: Expr,
        bind: String,
        body: Vec<Stmt>,
        otherwise: Option<Vec<Stmt>>,
    },
    /// `defer STMT` — run `STMT` when the enclosing block is left, whichever way
    /// it is left.
    ///
    /// It is sugar with no run-time machinery behind it: [`expand_defer`] copies
    /// the statement to every exit of the block it was written in — the last
    /// line, every `return` after it, and every `break`/`continue` that leaves
    /// that block — so what runs is the program the author could have written by
    /// hand, with the closing paired to the opening instead of scattered. Several
    /// defers in one block unwind in reverse order of declaration, because the
    /// second one's cleanup was set up while the first one's was still standing.
    ///
    /// The checker sees this node, not the copies, so a mistake inside a
    /// deferred statement is reported once.
    Defer(Box<Stmt>),
}

/// A subroutine (EPL 子程序).
///
/// `main` and event handlers are subs with an empty parameter list and no
/// return type — the shape every sub had before parameters existed, which is
/// why `sub main` keeps parsing and lowering exactly as it did.
#[derive(Debug, Clone, PartialEq)]
pub struct Sub {
    pub name: String,
    /// Declared parameters, in order: `(name, type)`. Parameters are immutable
    /// inside the body, like a `let`.
    pub params: Vec<(String, Ty)>,
    /// The default value of each parameter, parallel to `params`; `None` is a
    /// required one. Beside `params` rather than inside it so that everything
    /// which already reads the pairs keeps reading them.
    ///
    /// Only *trailing* parameters may have one — a gap would make
    /// `f(1, 2)` mean different things depending on which slot the reader
    /// counted from — and the parser refuses the gap where it is written.
    pub defaults: Vec<Option<Expr>>,
    /// Declared return type; `None` is a sub that returns nothing and may only
    /// be invoked with `call`.
    pub ret: Option<Ty>,
    /// The calling convention marker on the sub header (`sub wndproc(...): int64
    /// system`). It documents the convention C will invoke this sub with when
    /// its `address of` is handed across, and — like a `dll`'s — is a no-op on
    /// every 64-bit target Kiln emits, carried for a future 32-bit backend.
    /// It lives only on this AST node: `Signature` (what the registry keeps for
    /// argument checking) deliberately does not carry it, so a future backend
    /// reads it via `module.subs()`, not `reg.sub()`.
    pub conv: Option<CallConv>,
    /// 1-based source line of the `sub` keyword; 0 when unknown.
    pub line: usize,
    /// Where the name is written, for a diagnostic about the subroutine itself.
    pub name_span: Span,
    pub body: Vec<Stmt>,
}

impl Sub {
    /// The sub's signature, in the same shape a library command has — so one
    /// argument checker serves both.
    pub fn signature(&self) -> Signature {
        Signature {
            params: self.params.iter().map(|(_, t)| *t).collect(),
            ret: self.ret,
            names: self.params.iter().map(|(n, _)| n.clone()).collect(),
            defaults: self.defaults.clone(),
        }
    }

    /// Whether this sub has the shape an entry point or an event handler needs:
    /// nothing in, nothing out.
    pub fn is_plain(&self) -> bool {
        self.params.is_empty() && self.ret.is_none()
    }
}

/// One component instance inside a form: a type, an id, literal property
/// values, and event bindings to subroutines.  This is the IR half of the
/// component model — the designer will emit exactly this.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    /// Registered component type name, e.g. `button`.
    pub type_name: String,
    /// Author-chosen instance id, e.g. `ok_button`.  **Compile-time only** in
    /// v0: ids are not emitted into the binary (G8).
    pub id: String,
    /// Property assignments, in source order: `(name, literal)`.
    pub properties: Vec<(String, Expr)>,
    /// Event bindings: `(event name, handler subroutine name)`.
    pub handlers: Vec<(String, String)>,
    /// Where each property and each binding is written, parallel to
    /// `properties` and `handlers`. Beside them rather than inside the tuples
    /// so that everything reading the pairs keeps reading them.
    pub property_spans: Vec<Span>,
    pub handler_spans: Vec<Span>,
}

/// A form: the root component plus its children.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub name: String,
    /// 1-based `(first, last)` source lines of the whole `form … end` block.
    ///
    /// The designer rewrites **only** these lines when saving, splicing them
    /// into the original file. Re-emitting the whole module would destroy every
    /// hand-written subroutine body.
    pub line_span: (usize, usize),
    /// Properties of the form itself (title, size, …).
    pub properties: Vec<(String, Expr)>,
    pub handlers: Vec<(String, String)>,
    /// Positions parallel to `properties` and `handlers`, as on a `Component`.
    pub property_spans: Vec<Span>,
    pub handler_spans: Vec<Span>,
    pub children: Vec<Component>,
}

/// A top-level module item.  Remaining seam: `Const`, `Enum`
///.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Sub(Sub),
    /// A record declaration: `record point ... end`.
    UserType(RecordDef),
    Form(Form),
    /// A non-visual component declared at module level: `timer ticker`.
    ///
    /// It is the same `Component` a form holds, because it is the same thing —
    /// properties, events and an inspector row — minus a rectangle. A console
    /// program that waits for something is the reason this exists at all.
    Component(Component),
    /// A module-level mutable variable: `var count: int = 0`.
    ///
    /// This is where state that must outlive a single event handler lives —
    /// without it, an app has nowhere to keep a counter but the UI itself.
    Var(GlobalVar),
    /// A foreign function in a shared library: `dll MessageBoxA(...) from "user32"`.
    ///
    /// It is called exactly like a `Sub` — same call syntax, same arity/type
    /// checking — but instead of a body it names a symbol in a `.dll`/`.so`/
    /// `.dylib` that is resolved and bound at the first call. This is the one
    /// door out to a C API the language does not wrap.
    Dll(DllDecl),
    /// A named compile-time constant: `const MB_OK = 0`.
    ///
    /// Its value is a single literal, so its type is the literal's type and a
    /// reference to it folds to that literal everywhere a literal is allowed —
    /// a `dll` argument, a comparison, a `let` initializer. It exists so a kit
    /// can ship the `WM_*` / `MB_*` families a C API is written in terms of
    /// without a program spelling the magic numbers out.
    Const(ConstDef),
}

/// The C calling convention a foreign call — or a sub whose address is handed to
/// C — is made with. It is a documentation-and-forward-compat marker only:
/// every target Kiln emits today is 64-bit (x86-64 Linux, x64 Windows, 64-bit
/// macOS), and each of those has a *single* C convention, so `cdecl`, `stdcall`
/// and `system` all name the same one and the backend emits identical code for
/// each. The marker is carried so a future 32-bit backend — where the three
/// diverge and a mismatched `stdcall`/`cdecl` corrupts the stack — has the fact
/// the source stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallConv {
    /// `cdecl`: the caller cleans the stack. The one convention on every 64-bit
    /// target, and the C default on 32-bit non-Windows.
    Cdecl,
    /// `stdcall`: the callee cleans the stack. The Win32 API convention on
    /// 32-bit Windows; identical to `cdecl` on 64-bit.
    Stdcall,
    /// `system`: whatever a platform uses for its *system* APIs — `stdcall` on
    /// 32-bit Windows, `cdecl` everywhere else. The portable choice for a Win32
    /// declaration, because it stays correct if a 32-bit target ever lands.
    System,
}

impl CallConv {
    /// Parse a convention marker word; `None` for anything that is not one, so
    /// the parser can name the allowed set in its own diagnostic.
    pub fn parse(word: &str) -> Option<CallConv> {
        match word {
            "cdecl" => Some(CallConv::Cdecl),
            "stdcall" => Some(CallConv::Stdcall),
            "system" => Some(CallConv::System),
            _ => None,
        }
    }

    /// The marker word, as written in source.
    pub fn as_str(self) -> &'static str {
        match self {
            CallConv::Cdecl => "cdecl",
            CallConv::Stdcall => "stdcall",
            CallConv::System => "system",
        }
    }
}

/// A foreign function declared for calling: `dll NAME(params): ret from "lib" as "sym"`.
///
/// The declaration is the whole of it — there is no body, because the code
/// lives in someone else's library. A `dll` shares the callable namespace with
/// subroutines and commands (the validator rejects a collision), and its
/// signature is checked at every call site the same way a sub's is.
#[derive(Debug, Clone, PartialEq)]
pub struct DllDecl {
    /// The name a program calls it by.
    pub name: String,
    /// Declared parameters, in order. Only the C-representable types are
    /// allowed here (`int`, `int64`, `double`, `bool`, `text`, `ptr`); the
    /// parser rejects the rest, because there is no honest way to hand an
    /// Kiln array or record to a C function by value in this stage.
    pub params: Vec<(String, Ty)>,
    /// Declared return type; `None` is a call-only foreign function (C `void`).
    pub ret: Option<Ty>,
    /// The `from "..."` library the symbol lives in, spelled exactly as written
    /// — a bare name is decorated per platform at load time, a name with an
    /// extension or a slash is used verbatim.
    pub library: String,
    /// The exported symbol name, from `as "..."`; `None` means the symbol has
    /// the same name as the declaration, which is the common case.
    pub symbol: Option<String>,
    /// The calling convention marker, from the optional word after `from`/`as`
    /// (`... from "user32" system`). `None` when the declaration names none.
    /// A no-op on every target Kiln emits — see `CallConv` — carried for a
    /// future 32-bit backend.
    pub conv: Option<CallConv>,
    /// 1-based source line of the `dll` keyword; 0 when unknown.
    pub line: usize,
    /// Where the name is written, for a diagnostic about the declaration itself.
    pub name_span: Span,
}

impl DllDecl {
    /// The signature, in the same shape a sub or a command has — so the one
    /// argument checker serves foreign functions too.
    pub fn signature(&self) -> Signature {
        Signature {
            params: self.params.iter().map(|(_, t)| *t).collect(),
            ret: self.ret,
            // A foreign function has parameter names, so `MessageBoxA(text: "hi")`
            // works — but no defaults: the C function on the other side has no
            // opinion about a missing argument, and inventing one here would be
            // Kiln guessing at someone else's contract.
            names: self.params.iter().map(|(n, _)| n.clone()).collect(),
            defaults: vec![None; self.params.len()],
        }
    }

    /// The exported symbol to resolve: the `as` override, or the name itself.
    pub fn symbol_name(&self) -> &str {
        self.symbol.as_deref().unwrap_or(&self.name)
    }
}

/// A record type: a name for a group of related values.
///
/// A record is a **reference**, like an array and unlike a number: two names
/// for one record are two names for the same fields, and writing through
/// either is seen through both. That is the same bargain `xs[1] = 2` already
/// makes, and the alternative — copying on every assignment and every call —
/// would make a record the one aggregate in the language that behaves
/// differently from the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordDef {
    pub name: String,
    /// Fields in declaration order. The order is the layout: a field is reached
    /// by index at run time, so no field name reaches the shipped binary.
    pub fields: Vec<(String, Ty)>,
    /// `record NAME is c`: this record has a fixed C memory layout — a flat
    /// struct with natural alignment and padding — rather than the default
    /// runtime-heap object. A c-record is a value whose storage is a stack
    /// alloca; `address of` it yields the pointer a C API expects. A plain
    /// record (`is_c == false`) is unchanged: a reference to a heap object.
    pub is_c: bool,
    /// 1-based source line of the `record` keyword; 0 when unknown.
    pub line: usize,
}

impl RecordDef {
    /// The 1-based position of `name` among the fields, and its type.
    pub fn field(&self, name: &str) -> Option<(usize, Ty)> {
        self.fields
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| (i + 1, self.fields[i].1))
    }

    /// The C-struct layout of an `is c` record: the byte offset of each field
    /// (parallel to `fields`) and the total `sizeof`, computed with natural
    /// alignment — a field sits at the next offset that is a multiple of its
    /// alignment, and the whole struct is rounded up to its largest field's
    /// alignment so an array of them would stride correctly. `None` if any
    /// field has no by-value C layout (an aggregate), which the validator has
    /// already rejected for an `is c` record.
    ///
    /// This is the single source of truth for `size of`, for a field GEP, and
    /// for the padding the tests check against clang — there is no second
    /// place layout could be computed and disagree.
    pub fn c_layout(&self, reg: &Registry) -> Option<(Vec<i64>, i64, i64)> {
        self.c_layout_at(reg, 0)
    }

    /// `c_layout` with the nesting depth it has already descended. A c-record
    /// that contains itself is rejected by the validator, but the validator
    /// only ever sees one module's records at a time — a kit bundle is checked
    /// against "the registry so far" — so the layout walk carries its own
    /// bound and answers `None` rather than overflowing the stack.
    fn c_layout_at(&self, reg: &Registry, depth: u32) -> Option<(Vec<i64>, i64, i64)> {
        if depth > 32 {
            return None;
        }
        let mut offsets = Vec::with_capacity(self.fields.len());
        let mut cursor: i64 = 0;
        let mut max_align: i64 = 1;
        for (_, fty) in &self.fields {
            let (size, align) = c_field_size_align_at(*fty, reg, depth + 1)?;
            // Round the cursor up to this field's alignment before placing it —
            // the padding a C compiler inserts is exactly this rounding.
            cursor = (cursor + align - 1) / align * align;
            offsets.push(cursor);
            cursor += size;
            if align > max_align {
                max_align = align;
            }
        }
        // Tail padding: the struct's size is a multiple of its widest member's
        // alignment, so `a[1]` begins as aligned as `a[0]` did.
        let size = (cursor + max_align - 1) / max_align * max_align;
        Some((offsets, size, max_align))
    }
}

/// The size and alignment of one c-record *field* type, resolving what
/// `Ty::c_size_align` cannot on its own: a nested `is c` record (its own layout,
/// laid inline) and a fixed array (`count` elements, the element's alignment).
/// `None` when the type has no C layout at all, or when the nested record is
/// unknown, is a heap record, or nests too deep — every one of which the
/// validator reports with a real message first.
pub fn c_field_size_align(ty: Ty, reg: &Registry) -> Option<(i64, i64)> {
    c_field_size_align_at(ty, reg, 0)
}

fn c_field_size_align_at(ty: Ty, reg: &Registry, depth: u32) -> Option<(i64, i64)> {
    match ty {
        Ty::Record(name) => {
            let def = reg.record(name)?;
            if !def.is_c {
                return None;
            }
            let (_, size, align) = def.c_layout_at(reg, depth)?;
            Some((size, align))
        }
        // An inline array strides by its element's size and is aligned like one
        // element — exactly C's `T a[N]`.
        Ty::CArray(a) => {
            let (esize, ealign) = c_field_size_align_at(a.elem, reg, depth)?;
            Some((esize * a.count as i64, ealign))
        }
        other => other.c_size_align(),
    }
}

/// A named compile-time constant: `const NAME = LITERAL`.
///
/// The value is always a literal (an integer, a double, a text or a bool — the
/// parser folds a leading `-` into the number), so `ty` is fixed at parse time
/// and a reference to the name behaves in every position exactly as the literal
/// would. A constant is module-level, like a `GlobalVar`, but read-only and
/// with no storage: it never reaches the binary as anything but the number it
/// stands for.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstDef {
    pub name: String,
    /// The literal the name stands for: `IntLit`, `DoubleLit`, `TextLit` or
    /// `BoolLit`. Held as an `Expr` so the checker and the backend can fold a
    /// reference by evaluating it in place, reusing every literal path.
    pub value: Expr,
    /// The value's type, computed from the literal at parse time (an `int`
    /// literal that overflows `i32` is `int64`, as everywhere else).
    pub ty: Ty,
    /// 1-based source line of the `const` keyword; 0 when unknown.
    pub line: usize,
    /// Where the name is written, for a diagnostic about the constant itself.
    pub name_span: Span,
}

/// A module-level variable.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalVar {
    pub name: String,
    pub ty: Ty,
    /// Initializer. A literal becomes a static initializer; anything else is
    /// evaluated at program entry, in declaration order.
    pub value: Expr,
}

/// What a module is compiled into.
///
/// The target changes the *entry contract*, not the language: an executable
/// gets `ECodeStart`, a library gets no entry and exports its subroutines
/// instead. Everything else about lowering is identical, which is the point —
/// shipping a `.so` is a build-target choice, not a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Terminal program; `main` is the entry.
    Console,
    /// Windowed program; the form drives the entry, `main` runs first if present.
    Gui,
    /// `.so` / `.dll` / `.dylib` — no entry, subroutines exported.
    SharedLib,
    /// `.a` / `.lib` — no entry, subroutines exported, linked into a host.
    StaticLib,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Console => "console",
            Target::Gui => "gui",
            Target::SharedLib => "sharedlib",
            Target::StaticLib => "staticlib",
        }
    }

    pub fn parse(s: &str) -> Option<Target> {
        match s {
            "console" => Some(Target::Console),
            "gui" => Some(Target::Gui),
            "sharedlib" | "shared" | "dll" | "so" => Some(Target::SharedLib),
            "staticlib" | "static" => Some(Target::StaticLib),
            _ => None,
        }
    }

    /// Does this target produce a program with an entry point?
    pub fn is_executable(self) -> bool {
        matches!(self, Target::Console | Target::Gui)
    }

    /// The conventional file extension on this platform, or `""` for an
    /// executable (which has none on Unix).
    pub fn extension(self) -> &'static str {
        match self {
            Target::Console | Target::Gui => "",
            // Linux naming for now; Windows/macOS land with Phase 4's
            // cross-platform work.
            Target::SharedLib => "so",
            Target::StaticLib => "a",
        }
    }
}

/// A whole compilation unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub name: String,
    /// What to build this module into. `None` means "infer": a module with a
    /// form is a GUI program, otherwise a console one — so every file written
    /// before targets existed still means what it meant.
    pub target: Option<Target>,
    /// Support libraries this module uses (`use <name>`), beyond the implicit
    /// `core`.  The compiler introspects each for command signatures and links
    /// its implementations.
    pub uses: Vec<String>,
    pub items: Vec<Item>,
}

impl Module {
    /// The target to build, declared or inferred.
    pub fn target(&self) -> Target {
        self.target.unwrap_or_else(|| {
            if self.forms().next().is_some() {
                Target::Gui
            } else {
                Target::Console
            }
        })
    }

    /// Iterate the subroutines.
    pub fn subs(&self) -> impl Iterator<Item = &Sub> {
        self.items.iter().filter_map(|i| match i {
            Item::Sub(s) => Some(s),
            _ => None,
        })
    }

    /// Iterate the foreign-function declarations, in declaration order.
    pub fn dlls(&self) -> impl Iterator<Item = &DllDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Dll(d) => Some(d),
            _ => None,
        })
    }

    /// Iterate the module-level variables, in declaration order.
    pub fn globals(&self) -> impl Iterator<Item = &GlobalVar> {
        self.items.iter().filter_map(|i| match i {
            Item::Var(v) => Some(v),
            _ => None,
        })
    }

    /// Iterate the record declarations, in declaration order.
    pub fn records(&self) -> impl Iterator<Item = &RecordDef> {
        self.items.iter().filter_map(|i| match i {
            Item::UserType(r) => Some(r),
            _ => None,
        })
    }

    /// Iterate the named constants, in declaration order.
    pub fn consts(&self) -> impl Iterator<Item = &ConstDef> {
        self.items.iter().filter_map(|i| match i {
            Item::Const(c) => Some(c),
            _ => None,
        })
    }

    /// Iterate the forms.
    pub fn forms(&self) -> impl Iterator<Item = &Form> {
        self.items.iter().filter_map(|i| match i {
            Item::Form(f) => Some(f),
            _ => None,
        })
    }

    /// Iterate the module-level (non-visual) components, in declaration order.
    pub fn components(&self) -> impl Iterator<Item = &Component> {
        self.items.iter().filter_map(|i| match i {
            Item::Component(c) => Some(c),
            _ => None,
        })
    }

    /// A GUI module (one that declares a form) links the UI stack and uses the
    /// form entry; a console module keeps the original `main` path.
    pub fn is_gui(&self) -> bool {
        self.forms().next().is_some()
    }
}
