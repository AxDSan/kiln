//! The K2 abstract syntax tree. Spans on every node so diagnostics and the
//! (later) formatter can point precisely. This is the subset the first running
//! compiler handles; generics, lambdas, attributes, interfaces and forms are
//! parsed-or-stubbed and grow in later phases.

use crate::lexer::Span;

#[derive(Clone, Debug)]
pub struct Program {
    pub namespace: Option<String>,
    pub usings: Vec<Using>,
    pub items: Vec<Item>,
    /// Top-level statements (become `Main`), if any.
    pub top_level: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Using {
    pub path: String,
    pub is_static: bool,
    pub alias: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    Type(TypeDecl),
    Enum(EnumDecl),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Public,
    Internal,
    Private,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Class,
    Record,
    StaticClass,
    Struct,
}

#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub kind: TypeKind,
    pub vis: Vis,
    pub name: String,
    /// Positional record parameters (`record R(int A, string B)`).
    pub record_params: Vec<Field>,
    pub fields: Vec<Field>,
    pub consts: Vec<ConstDecl>,
    pub methods: Vec<Method>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub vis: Vis,
    pub name: String,
    /// Backing type name (`enum E : byte`), default `int`.
    pub backing: Option<TypeRef>,
    pub members: Vec<(String, Option<Expr>)>,
    pub doc: Option<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub vis: Vis,
    pub name: String,
    pub ty: TypeRef,
    pub default: Option<Expr>,
    pub is_readonly: bool,
    pub is_const: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ConstDecl {
    pub vis: Vis,
    pub name: String,
    pub ty: TypeRef,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: TypeRef,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Method {
    pub vis: Vis,
    pub is_static: bool,
    pub name: String,
    /// Generic type parameters (`Max<T>`), empty for a non-generic method.
    pub type_params: Vec<String>,
    pub params: Vec<Param>,
    pub ret: TypeRef,
    pub body: Vec<Stmt>,
    /// Expression-bodied `=> expr`.
    pub expr_body: Option<Expr>,
    pub doc: Option<String>,
    pub span: Span,
}

/// A written type. Resolution to a `kir::TyId` happens in lowering.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeRef {
    Named(String),
    /// `T?`
    Optional(Box<TypeRef>),
    /// `T[]`
    Array(Box<TypeRef>),
    /// `List<T>`, `Dictionary<K,V>`, `Result<T>`, `Func<...>` etc.
    Generic(String, Vec<TypeRef>),
    Void,
}

// ─── statements ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// `var x = e;` / `let x = e;` / `int x = e;`
    Local {
        name: String,
        ty: Option<TypeRef>,
        mutable: bool,
        value: Expr,
    },
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    /// C-style `for (init; cond; step) { }`
    For {
        init: Box<Option<Stmt>>,
        cond: Option<Expr>,
        step: Box<Option<Stmt>>,
        body: Vec<Stmt>,
    },
    /// `foreach (var x in coll) { }` — coll may be a range.
    ForEach {
        var: String,
        coll: Expr,
        body: Vec<Stmt>,
    },
    Break,
    Continue,
    Defer(Box<Stmt>),
    Block(Vec<Stmt>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Eq,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    NullCoalesce, // ??=
}

// ─── expressions ────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i128),
    Float(f64, bool /* f32 */),
    Bool(bool),
    Str(String),
    Interp(Vec<InterpSeg>),
    Char(char),
    Null,
    Ident(String),
    /// `a.b`
    Member(Box<Expr>, String),
    /// `f(args)` — callee may be Member or Ident.
    Call(Box<Expr>, Vec<Expr>),
    /// `a[i]`
    Index(Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `a..b` / `a..<b`
    Range(Box<Expr>, Box<Expr>, bool /* inclusive */),
    /// `(T)e`
    Cast(TypeRef, Box<Expr>),
    /// `new T(args)` / `new T { field = v, ... }`
    New(TypeRef, Vec<Expr>, Vec<(String, Expr)>),
    /// `cond ? a : b`
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `e ?? fallback`
    NullCoalesce(Box<Expr>, Box<Expr>),
    /// `e?` — Result/T? propagation.
    Try(Box<Expr>),
    /// `x => e`, `(a, b) => e`, `() => { ... }`
    Lambda(Lambda),
}

#[derive(Clone, Debug)]
pub struct Lambda {
    /// Parameter names, with an optional written type.
    pub params: Vec<(String, Option<TypeRef>)>,
    pub body: LambdaBody,
}

#[derive(Clone, Debug)]
pub enum LambdaBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Clone, Debug)]
pub enum InterpSeg {
    Lit(String),
    Expr(Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}
