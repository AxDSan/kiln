//! Recursive-descent parser: `.kiln` text -> `Module`.
//!
//! Grammar (v0):
//! ```text
//! module  := "module" IDENT NEWLINE item*
//! item    := sub
//! sub     := "sub" IDENT params? (":" type)? conv? NEWLINE stmt* "end" NEWLINE
//! conv    := "stdcall" | "cdecl" | "system"
//! params  := "(" (param ("," param)*)? ")"
//! param   := IDENT ":" type ("=" expr)?   -- a default; only the trailing
//!   -- parameters may have one, and a `dll` may have none.
//! stmt    := let | assign | call | indirect | return | for | break | continue
//!          | incdec | check
//! check   := "check" call NEWLINE   -- and `let x: T = check EXPR`; both expand
//!   -- to the statement plus `if last_error_code() <> 0 <return> end`.
//!   -- a simple statement (call, assign, return, incdec) may carry a one-line
//!   -- suffix `STMT "if" expr NEWLINE`, sugar for `if expr NEWLINE STMT ... end`.
//! assign  := lvalue asgop expr NEWLINE
//! asgop   := "=" | "+=" | "-=" | "*=" | "/=" | "mod=" | "&="
//!   -- compound forms desugar to `lvalue = lvalue OP expr` (`&=` via `concat`).
//! incdec  := ("increment" | "decrement") lvalue NEWLINE  -- `lvalue = lvalue ± 1`
//! lvalue  := IDENT ("[" expr "]" | "." IDENT)*
//! return  := "return" expr? NEWLINE
//! let     := ("let" | "var") IDENT (":" type)? ("=" expr)? NEWLINE
//!   -- the type may be left out when the value has one; the value may be left
//!   -- out only for a c-record `var`, which starts zeroed.
//! call    := "call" IDENT "(" args? ")" NEWLINE
//! args    := arg ("," arg)* ","?
//! arg     := (IDENT ":")? expr   -- a named argument goes to the parameter of
//!   -- that name; the named ones come after the positional ones.
//! indirect:= "call" "through" target "(" (expr ("," expr)*)? ")"
//!            (":" type)? conv? NEWLINE
//! target  := "(" expr ")" | IDENT ("." IDENT)? ("[" expr "]" | "." IDENT)*
//! record  := "record" IDENT NEWLINE (IDENT ":" type NEWLINE)+ "end" NEWLINE
//! type    := ("int" | "int64" | "double" | "text" | "bool" | "bytes" | IDENT)
//!            ("[" "]" | "{" "}")?
//! expr    := ifexpr | or ("otherwise" or)?
//!   -- `otherwise` yields its right side when the left one's call failed;
//!   -- one per expression.
//! ifexpr  := "if" or "then" expr "else" expr   -- the conditional VALUE; a
//!   -- statement beginning with `if` is always the block form.
//! or      := and ("or" and)*
//! and     := not_ ("and" not_)*
//! not_    := "not" not_ | cmp
//! cmp     := bor (cmpop bor (cmpop bor)?)?   -- two in a row is a chained
//!          | bor ("not"? "in") bor           -- comparison; membership test
//! cmpop   := "=" | "<>" | "<" | "<=" | ">" | ">="
//! bor     := bxor ("bor" bxor)*
//! bxor    := band ("bxor" band)*
//! band    := shift ("band" shift)*
//! shift   := sum (("shl" | "shr" | "ushr") sum)*
//! sum     := term (("+" | "-") term)*
//! term    := factor (("*" | "/" | "%") factor)*
//! factor  := ("-" | "bnot")? postfix
//! postfix := primary ("[" expr "]" | "." IDENT | "." IDENT "(" args? ")")*
//!   -- `x.f(a)` is `f(x, a)`; a `.` with no `(` stays a property/field read.
//! primary := INT | BITS | FLOAT | STRING | list | dict | new | call | IDENT
//!          | "(" expr ")" | indirect-expr
//! indirect-expr := the `indirect` statement's shape without the NEWLINE, used
//!            for its value; the `: type` is then required.
//! list    := "[" (expr ("," expr)*)? "]"
//! dict    := "{" (expr ":" expr ("," expr ":" expr)*)? "}"
//! new     := IDENT "(" IDENT ":" expr ("," IDENT ":" expr)* ")"
//!          | IDENT "{" ("..."? ".." expr ",")? (IDENT ":" expr ("," IDENT ":" expr)*)? "}"
//!   -- the brace form is the same record; with a leading `...base` (`..base`
//!   -- is the same spelling) every field it does not name is copied from
//!   -- `base`, which has to be a place.
//! call    := IDENT "(" args? ")"
//! ```

use std::collections::HashMap;

use crate::lexer::{lex, Spanned, Tok};
use crate::sema::{bits_bare_type, bits_value};
use crate::{carray, intern, BinOp, BitOp, CallConv, CmpOp, Component, ConstDef, DllDecl, Elem, Expr, Form, GlobalVar, Ident, Item,
    LogicalOp, Module, RecordDef, Span, Stmt, StmtKind, Sub, Target, Ty,};

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error (line {}): {}", self.line, self.msg)
    }
}
impl std::error::Error for ParseError {}

pub fn parse(src: &str) -> Result<Module, ParseError> {
    parse_with(src, ParseOptions::default())
}

/// What a parse may leave out.
///
/// The one knob is `asserts`, and it exists because `assert` is the single
/// construct whose *presence* is a build choice rather than a language one: a
/// release build compiles the checks out entirely, and "entirely" has to mean
/// no statement, not a branch on `false` that a later pass is trusted to
/// remove. Everything else about the parse is identical either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    /// Whether `assert` produces a statement. `false` drops each one where it
    /// is written — the condition is still parsed, so a release build refuses
    /// a malformed `assert` exactly as a debug build does.
    pub asserts: bool,
}

impl Default for ParseOptions {
    /// Asserts on: every reader but a release build wants them, and the ones
    /// that only *look* at a file — the language server, `inspect`, the
    /// designer — must see the whole program.
    fn default() -> ParseOptions {
        ParseOptions { asserts: true }
    }
}

pub fn parse_with(src: &str, opts: ParseOptions) -> Result<Module, ParseError> {
    let toks = lex(src).map_err(|e| ParseError {
        line: e.line,
        msg: e.msg,
    })?;
    let enums = scan_enums(&toks);
    Parser {
        toks,
        pos: 0,
        pending: Vec::new(),
        cur_ret: None,
        in_sub: false,
        enums,
        hidden: 0,
        opts,
        lines: src.lines().map(|l| l.to_string()).collect(),
    }
    .module()
}

/// The enum names and members declared anywhere in the file, read off the token
/// stream before the parse proper.
///
/// A pre-pass rather than a fact learnt as items are read, because `Colour.red`
/// may be written in a subroutine above the `enum` that declares it — records
/// may be declared after their use and an enum has no reason to be stricter.
/// The shape demanded is exactly `enum NAME NEWLINE`, at the start of a line,
/// so a variable called `enum` inside a body is not mistaken for a declaration.
fn scan_enums(toks: &[Spanned]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for i in 0..toks.len() {
        if !matches!(&toks[i].tok, Tok::Ident(w) if w == "enum") {
            continue;
        }
        if i > 0 && !matches!(toks[i - 1].tok, Tok::Newline) {
            continue;
        }
        let Some(Tok::Ident(name)) = toks.get(i + 1).map(|t| &t.tok) else {
            continue;
        };
        if !matches!(toks.get(i + 2).map(|t| &t.tok), Some(Tok::Newline)) {
            continue;
        }
        let mut members = Vec::new();
        let mut j = i + 3;
        while let Some(t) = toks.get(j) {
            match &t.tok {
                Tok::Ident(m) => members.push(m.clone()),
                Tok::Comma | Tok::Newline => {}
                _ => break,
            }
            j += 1;
        }
        out.insert(name.clone(), members);
    }
    out
}

struct Parser {
    toks: Vec<Spanned>,
    pos: usize,
    /// Statements a desugar produced *after* the one being returned.
    ///
    /// A statement parser returns exactly one `Stmt`, and `check EXPR` needs
    /// two — the call, then the guard that returns when it failed. Rather than
    /// give every statement a vector it does not need, the extra ones are left
    /// here and the two statement loops (`sub` and `block`) drain them straight
    /// after the statement they belong to, which puts them in source order.
    pending: Vec<Stmt>,
    /// The return type of the subroutine being parsed, so `check` knows what
    /// value its early `return` must carry. `None` inside a sub that returns
    /// nothing, and meaningless unless `in_sub`.
    cur_ret: Option<Ty>,
    /// Whether a subroutine body is being parsed at all — `check` outside one
    /// has no `return` to reach.
    in_sub: bool,
    /// Every `enum` in the file and its members, read before the parse — see
    /// `scan_enums`. It is what lets `Colour.red` fold to the constant it names
    /// and what lets `Colour` be written as a parameter type.
    enums: HashMap<String, Vec<String>>,
    /// How many hidden names have been minted, so two `repeat` loops in one
    /// subroutine do not both declare the same counter. The names carry a `$`,
    /// which no identifier can, so they cannot collide with a written one.
    hidden: usize,
    opts: ParseOptions,
    /// The source, by line, for the message an `assert` with none of its own
    /// carries: the condition as the author wrote it, sliced out by the
    /// columns the tokens already record.
    lines: Vec<String>,
}

/// A parsed assignment target. It holds enough to build both the expression a
/// *read* of the target is (the left operand a compound assignment reuses) and
/// the *write* statement, so one parser serves `x = e`, `x += e`, `increment x`
/// and every target shape.
enum Lvalue {
    Var(String),
    Index { name: String, index: Expr },
    Property { component: String, property: String },
    Place(Expr),
}

impl Lvalue {
    /// The expression a read of this target is — cloned, since a compound
    /// assignment names the target on both sides (`x += e` is `x = x + e`).
    fn read(&self) -> Expr {
        match self {
            Lvalue::Var(name) => Expr::Var(name.clone()),
            Lvalue::Index { name, index } => Expr::Index {
                base: Box::new(Expr::Var(name.clone())),
                index: Box::new(index.clone()),
            },
            Lvalue::Property { component, property } => Expr::GetProperty {
                component: component.clone(),
                property: property.clone(),
            },
            Lvalue::Place(e) => e.clone(),
        }
    }

    /// The statement that writes `value` into this target — the same
    /// `StmtKind` a plain `=` would have produced.
    fn write(self, value: Expr) -> StmtKind {
        match self {
            Lvalue::Var(name) => StmtKind::Assign { name, value },
            Lvalue::Index { name, index } => StmtKind::SetIndex { name, index, value },
            Lvalue::Property { component, property } => {
                StmtKind::SetProperty { component, property, value }
            }
            Lvalue::Place(place) => StmtKind::SetPlace { place, value },
        }
    }
}

/// Which assignment operator a target was followed by.
enum AssignOp {
    /// A plain `=`: the value is the right-hand side unchanged.
    Plain,
    /// A `+= -= *= /= mod=` — arithmetic, desugaring to `target = target OP e`.
    Bin(BinOp),
    /// `&=` — text append, desugaring to `target = concat(target, e)`.
    Concat,
}

/// The value a compound assignment stores. For `target OP= rhs` the desugar is
/// `target = target OP rhs`, reusing the very `=` statement path so the type
/// rules are the ordinary ones. `&=` joins text through `concat`, the command an
/// author could have called by name.
fn apply_assign_op(op: AssignOp, target: Expr, rhs: Expr) -> Expr {
    match op {
        AssignOp::Plain => rhs,
        AssignOp::Bin(b) => Expr::Bin(b, Box::new(target), Box::new(rhs)),
        AssignOp::Concat => Expr::Call {
            cmd: "concat".to_string(),
            args: vec![target, rhs],
        },
    }
}

// --- String interpolation ---------------------------------------------------
//
// A text literal may carry `{expr}` holes: `"Row {i} of {n}"`. The lexer has
// already decoded the literal's escapes (`\n`, `\t`, `\\`, `\"`, `\0`) and
// passed its braces through untouched, so the whole of interpolation lives in
// the parser, splitting that decoded string. `{{` and `}}` are the escaped
// single braces; every other `{` opens a hole that runs to its matching `}`
// (nested braces — a dict literal — balance), and the text between is re-lexed
// and parsed as a full expression. The literal desugars to the left-folded
// concat of its literal chunks and its holes, each hole wrapped in
// `Expr::ToText`. A literal with no holes is returned as the one `TextLit` it
// was before interpolation existed — no concat where none is needed.

/// Is there a real `{expr}` hole in this decoded literal? `{{`/`}}` are escaped
/// braces and do not count. Used where a hole is not allowed (a `const`).
fn text_has_hole(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' if i + 1 < b.len() && b[i + 1] == b'{' => i += 2,
            b'}' if i + 1 < b.len() && b[i + 1] == b'}' => i += 2,
            b'{' => return true,
            _ => i += 1,
        }
    }
    false
}

/// Turn `{{`/`}}` into single braces in a literal with no holes — the same
/// unescaping the interpolation path does to its literal chunks, for the one
/// place that keeps a raw `TextLit` of its own (a `const`).
fn unescape_braces(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' if i + 1 < b.len() && b[i + 1] == b'{' => {
                out.push('{');
                i += 2;
            }
            b'}' if i + 1 < b.len() && b[i + 1] == b'}' => {
                out.push('}');
                i += 2;
            }
            _ => {
                let start = i;
                while i < b.len() && b[i] != b'{' && b[i] != b'}' {
                    i += 1;
                }
                if i == start {
                    // A lone brace with no partner: keep it, as this path is
                    // only reached for text that `text_has_hole` said has none.
                    out.push(b[i] as char);
                    i += 1;
                } else {
                    out.push_str(&s[start..i]);
                }
            }
        }
    }
    out
}

/// Parse one hole's text as a full expression. The hole is re-lexed on its own,
/// so its errors carry the literal's line rather than the sub-lex's line 1, and
/// every token must belong to the single expression: a leftover `:` is a format
/// spec (reserved, not yet supported) and anything else is a syntax error,
/// both said against the hole so they cannot mis-parse.
fn parse_hole(text: &str, line: usize, enums: &HashMap<String, Vec<String>>) -> Result<Expr, ParseError> {
    let shown = text.trim();
    let toks = lex(text).map_err(|e| ParseError {
        line,
        msg: format!("in the interpolation hole `{{{shown}}}`: {}", e.msg),
    })?;
    // A hole is one expression and nothing else: no statement can appear in it,
    // so the statement-desugar state stays empty and `check` (which needs a
    // subroutine to return from) is refused there.
    let mut sub = Parser {
        toks,
        pos: 0,
        pending: Vec::new(),
        cur_ret: None,
        in_sub: false,
        // The enums the file declares, so `"{Colour.red}"` names the same
        // constant inside a hole that it names outside one.
        enums: enums.clone(),
        hidden: 0,
        opts: ParseOptions::default(),
        lines: Vec::new(),
    };
    let e = sub.expr().map_err(|err| ParseError {
        line,
        msg: format!("in the interpolation hole `{{{shown}}}`: {}", err.msg),
    })?;
    match sub.peek() {
        Tok::Eof => Ok(e),
        Tok::Colon => Err(ParseError {
            line,
            msg: format!("format specs like `{{{shown}:...}}` are not supported yet"),
        }),
        other => Err(ParseError {
            line,
            msg: format!(
                "unexpected {other:?} after the expression in the interpolation hole `{{{shown}}}`"
            ),
        }),
    }
}

/// Split a decoded literal into its interpolation pieces: literal chunks as
/// `TextLit`, holes as `ToText`. Always yields at least one piece (the empty
/// string is one empty `TextLit`), so the fold that follows never sees nothing.
fn split_interp(s: &str, line: usize, enums: &HashMap<String, Vec<String>>) -> Result<Vec<Expr>, ParseError> {
    let b = s.as_bytes();
    let n = b.len();
    let mut pieces: Vec<Expr> = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < n {
        match b[i] {
            b'{' if i + 1 < n && b[i + 1] == b'{' => {
                lit.push('{');
                i += 2;
            }
            b'}' if i + 1 < n && b[i + 1] == b'}' => {
                lit.push('}');
                i += 2;
            }
            b'}' => {
                return Err(ParseError {
                    line,
                    msg: "a lone `}` in text — write `}}` for a literal brace".into(),
                })
            }
            b'{' => {
                if !lit.is_empty() {
                    pieces.push(Expr::TextLit(std::mem::take(&mut lit)));
                }
                // Walk to the `}` that closes this hole, counting nested braces
                // so a dict literal inside the expression balances.
                let start = i + 1;
                let mut depth = 1usize;
                let mut j = start;
                while j < n {
                    match b[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if depth != 0 {
                    return Err(ParseError {
                        line,
                        msg: "an interpolation hole is missing its closing `}`".into(),
                    });
                }
                let hole = &s[start..j];
                if hole.trim().is_empty() {
                    return Err(ParseError {
                        line,
                        msg: "an empty interpolation hole `{}` — put an expression between the braces"
                            .into(),
                    });
                }
                let expr = parse_hole(hole, line, enums)?;
                pieces.push(Expr::ToText {
                    value: Box::new(expr),
                    hole: hole.trim().to_string(),
                });
                i = j + 1;
            }
            _ => {
                // A run of ordinary bytes up to the next brace. Braces are ASCII
                // and never a UTF-8 continuation byte, so slicing at one is
                // always a char boundary.
                let start = i;
                while i < n && b[i] != b'{' && b[i] != b'}' {
                    i += 1;
                }
                lit.push_str(&s[start..i]);
            }
        }
    }
    if !lit.is_empty() || pieces.is_empty() {
        pieces.push(Expr::TextLit(lit));
    }
    Ok(pieces)
}

/// Desugar a decoded text literal: an all-literal string stays one `TextLit`
/// (only `{{`/`}}` unescaped); a string with holes becomes the left-folded
/// concat chain — `concat(concat(chunk0, ToText(h1)), chunk2)` — that a hand
/// written program would spell with `concat` and `int_to_text`.
fn interpolate(s: &str, line: usize, enums: &HashMap<String, Vec<String>>) -> Result<Expr, ParseError> {
    let pieces = split_interp(s, line, enums)?;
    let mut it = pieces.into_iter();
    let mut acc = it.next().expect("split_interp yields at least one piece");
    for p in it {
        acc = Expr::Call {
            cmd: "concat".to_string(),
            args: vec![acc, p],
        };
    }
    Ok(acc)
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn line(&self) -> usize {
        self.toks[self.pos].line
    }
    /// The span of the token at `pos`.
    fn span_at(&self, pos: usize) -> Span {
        let t = &self.toks[pos.min(self.toks.len() - 1)];
        Span::new(t.line, t.col, t.end_col)
    }
    /// A statement whose header began at token `start`: its span runs to the
    /// end of that line, and its identifiers are the ones on it. The body of
    /// an `if` or a loop is not part of the header — each statement in it
    /// answers for itself.
    fn finish(&self, kind: StmtKind, start: usize) -> Stmt {
        let first = self.span_at(start);
        let mut span = first;
        let mut idents = Vec::new();
        for t in &self.toks[start..] {
            if matches!(t.tok, Tok::Newline | Tok::Eof) {
                break;
            }
            span.end_col = t.end_col;
            if let Tok::Ident(name) = &t.tok {
                idents.push(Ident {
                    name: name.clone(),
                    span: Span::new(t.line, t.col, t.end_col),
                });
            }
        }
        Stmt {
            kind,
            line: first.line,
            span,
            idents,
        }
    }
    /// The token `n` places past the cursor, saturating at the final `Eof`.
    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }
    fn bump(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            line: self.line(),
            msg: msg.into(),
        })
    }
    fn expect(&mut self, want: &Tok, what: &str) -> Result<(), ParseError> {
        if self.peek() == want {
            self.bump();
            Ok(())
        } else {
            self.err(format!("expected {what}, found {:?}", self.peek()))
        }
    }
    /// Consume zero or more newline separators.
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
    }
    fn ident(&mut self, what: &str) -> Result<String, ParseError> {
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            other => self.err(format!("expected {what}, found {other:?}")),
        }
    }

    fn module(&mut self) -> Result<Module, ParseError> {
        self.skip_newlines();
        // `unit <name>` is the other header. It is a SOFT keyword, matched only
        // here: a program is `module`, a file meant to be `use`d by one is
        // `unit`, and the word is free everywhere else.
        let is_unit = if matches!(self.peek(), Tok::Ident(w) if w == "unit") {
            self.bump();
            true
        } else {
            self.expect(&Tok::Module, "`module` or `unit`")?;
            false
        };
        let name = self.ident(if is_unit { "unit name" } else { "module name" })?;
        self.expect(&Tok::Newline, "newline after module name")?;

        // Optional `target <kind>` and `use <lib>` declarations precede the
        // items.
        //
        // `target` is a *soft* keyword — matched as an identifier in this one
        // position rather than reserved in the lexer. Reserving it would steal
        // `target` as a variable and property name everywhere, which is a poor
        // trade for one declaration.
        let mut target = None;
        let mut uses = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(self.peek(), Tok::Ident(w) if w == "target") {
                let line = self.line();
                if is_unit {
                    return Err(ParseError {
                        line,
                        msg: "a unit has no `target` — the program that uses it decides what is built"
                            .into(),
                    });
                }
                self.bump();
                let kind = self.ident("target kind")?;
                match Target::parse(&kind) {
                    Some(t) => target = Some(t),
                    None => {
                        return Err(ParseError {
                            line,
                            msg: format!(
                                "unknown target `{kind}` — expected console, gui, sharedlib \
                                 or staticlib"
                            ),
                        })
                    }
                }
                self.expect(&Tok::Newline, "newline after `target`")?;
                continue;
            }
            if matches!(self.peek(), Tok::Use) {
                self.bump();
                let lib = self.ident("library name")?;
                self.expect(&Tok::Newline, "newline after `use`")?;
                uses.push(lib);
            } else {
                break;
            }
        }

        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::Sub => items.push(Item::Sub(self.sub()?)),
                Tok::Form => items.push(Item::Form(self.form()?)),
                Tok::Var => items.push(Item::Var(self.global_var()?)),
                // `record` is a SOFT keyword, matched in this one position:
                // `record point` and `timer ticker` are the same two tokens, so
                // the word itself has to decide, and reserving it would steal an
                // ordinary noun from every variable in the language.
                Tok::Ident(w) if w == "record" => {
                    items.push(Item::UserType(self.record_def()?))
                }
                // `dll` is a SOFT keyword, matched in this one position beside
                // `sub`: `dll open ...` and `timer dll` are the same two tokens
                // otherwise, and reserving `dll` would steal the word from every
                // program that has a variable called that.
                Tok::Ident(w) if w == "dll" => items.push(Item::Dll(self.dll_decl()?)),
                // `const NAME = LITERAL` — a named constant. `const` is a SOFT
                // keyword matched only here: `const foo = 1` and a component
                // `const foo` differ by the `=`, and reserving the word would
                // steal it from every program that has a variable called that.
                Tok::Ident(w) if w == "const" => {
                    items.push(Item::Const(self.const_def()?))
                }
                // `enum Colour` — a set of named ints. A SOFT keyword like the
                // rest out here, and claimed only in the shape a declaration
                // has: the word, a name, and the end of the line. A component
                // called `enum something` is two identifiers on a line too, so
                // the newline is what separates them.
                Tok::Ident(w)
                    if w == "enum"
                        && matches!(self.peek_at(1), Tok::Ident(_))
                        && matches!(self.peek_at(2), Tok::Newline) =>
                {
                    for c in self.enum_def()? {
                        items.push(Item::Const(c));
                    }
                }
                // `type id` at module level is a NON-VISUAL component — a timer,
                // a server. It reads exactly as it does inside a form, because
                // the only thing it lacks is a rectangle to be drawn in; the
                // validator is what rejects a button out here.
                Tok::Ident(type_name) => {
                    let type_name = type_name.clone();
                    self.bump();
                    items.push(Item::Component(self.component(type_name)?));
                }
                Tok::Eof => break,
                other => {
                    return self.err(format!(
                        "expected `sub`, `dll`, `const`, `form`, `var`, a component, or end of \
                         file, found {other:?}"
                    ))
                }
            }
        }
        Ok(Module { name, target, uses, items, is_unit })
    }

    /// ```text
    /// sub := "sub" IDENT params? (":" type)? conv? NEWLINE stmt* "end" NEWLINE
    /// ```
    ///
    /// Both the parameter list and the return type are optional, so `sub main`
    /// — an entry point or an event handler — still means exactly what it did
    /// before parameters existed.
    fn sub(&mut self) -> Result<Sub, ParseError> {
        let sub_line = self.line();
        self.expect(&Tok::Sub, "`sub`")?;
        let name_span = self.span_at(self.pos);
        let name = self.ident("subroutine name")?;

        // `sub name(a: int, b: text)`; `sub name()` is the same as `sub name`.
        // A trailing parameter may carry `= EXPR`, the value a call that leaves
        // it out gets.
        let mut params: Vec<(String, Ty)> = Vec::new();
        let mut defaults: Vec<Option<Expr>> = Vec::new();
        if matches!(self.peek(), Tok::LParen) {
            self.bump();
            if !matches!(self.peek(), Tok::RParen) {
                loop {
                    let pname = self.ident("parameter name")?;
                    self.expect(&Tok::Colon, "`:` after parameter name")?;
                    let pty = self.type_keyword()?;
                    if params.iter().any(|(n, _)| *n == pname) {
                        return self.err(format!(
                            "subroutine `{name}` declares parameter `{pname}` twice"
                        ));
                    }
                    let default = if matches!(self.peek(), Tok::Eq) {
                        self.bump();
                        Some(self.expr()?)
                    } else {
                        None
                    };
                    // Only the tail may be optional: with a gap, `f(1, 2)` would
                    // fill different slots depending on where the reader started
                    // counting, and there is no spelling that says which.
                    if default.is_none() {
                        if let Some(i) = defaults.iter().position(|d| d.is_some()) {
                            return self.err(format!(
                                "subroutine `{name}`: parameter `{pname}` has no default but \
                                 `{}` before it does — a default is only allowed on the last \
                                 parameters",
                                params[i].0
                            ));
                        }
                    }
                    defaults.push(default);
                    params.push((pname, pty));
                    match self.peek() {
                        Tok::Comma => {
                            self.bump();
                            // A single trailing comma is allowed: `sub f(a: int,)`.
                            if matches!(self.peek(), Tok::RParen) {
                                break;
                            }
                        }
                        Tok::RParen => break,
                        other => {
                            return self
                                .err(format!("expected `,` or `)` in parameters, found {other:?}"))
                        }
                    }
                }
            }
            self.expect(&Tok::RParen, "`)` after parameters")?;
        }

        // `: type` — the same `name: type` shape a `let` uses, so a return type
        // reads like the declaration it is.
        let ret = if matches!(self.peek(), Tok::Colon) {
            self.bump();
            Some(self.type_keyword()?)
        } else {
            None
        };
        // An optional trailing convention marker, the same one a `dll` takes:
        // `sub wndproc(...): int64 system`. It documents the convention C calls
        // this sub with when its `address of` is handed across, and is a no-op
        // on every 64-bit target — carried for a future 32-bit backend.
        let conv = self.call_conv_opt(&format!("subroutine `{name}`"))?;
        self.expect(&Tok::Newline, "newline after subroutine name")?;

        // What `check` inside this body returns when the call it guards failed.
        // Subs do not nest, so one slot is enough; it is restored to nothing on
        // the way out so a `check` at module level is still refused.
        self.cur_ret = ret;
        self.in_sub = true;
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::End => {
                    self.bump();
                    break;
                }
                Tok::Eof => return self.err("unexpected end of file inside `sub` (missing `end`)"),
                _ => {
                    if let Some(st) = self.statement()? {
                        body.push(st);
                    }
                }
            }
            // A desugar that produced a follow-on statement (a `check` guard)
            // left it here; it belongs directly after the statement it guards.
            body.extend(self.pending.drain(..));
        }
        self.in_sub = false;
        self.cur_ret = None;
        // Trailing newline after `end` is optional (EOF is fine).
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(Sub {
            name,
            params,
            defaults,
            ret,
            conv,
            line: sub_line,
            name_span,
            body,
        })
    }

    /// ```text
    /// dll := "dll" IDENT "(" ffiparams? ")" (":" ffitype)?
    ///        "from" STRING ("as" STRING)? conv? NEWLINE
    /// ```
    ///
    /// A foreign function: the same header a `sub` has, but ending in `from` and
    /// with no body. `from` and `as` are soft keywords — plain identifiers the
    /// parser recognises only in this position, so neither is reserved anywhere
    /// else. Only the C-representable types may appear in the signature; the
    /// rest are refused here, where the message can name the offending type.
    fn dll_decl(&mut self) -> Result<DllDecl, ParseError> {
        let dll_line = self.line();
        // `dll` reached this method as a soft keyword (an identifier), so it is
        // consumed here rather than by `expect`.
        self.bump();
        let name_span = self.span_at(self.pos);
        let name = self.ident("foreign function name")?;

        // The parameter list is required: `()` is what keeps `dll foo` from
        // reading as a two-identifier component declaration, so a bare
        // `dll foo` is a mistake, and `expect` names the missing `(`. An empty
        // list `()` is a niladic foreign function.
        self.expect(&Tok::LParen, "`(` after foreign function name")?;
        let mut params: Vec<(String, Ty)> = Vec::new();
        if !matches!(self.peek(), Tok::RParen) {
            loop {
                let pname = self.ident("parameter name")?;
                self.expect(&Tok::Colon, "`:` after parameter name")?;
                let pty = self.ffi_type(&format!("foreign function `{name}`"), true)?;
                if params.iter().any(|(n, _)| *n == pname) {
                    return self.err(format!(
                        "foreign function `{name}` declares parameter `{pname}` twice"
                    ));
                }
                // A default belongs to whoever wrote the body, and a `dll` has
                // none: the C function on the other side decides what its
                // arguments mean, and Kiln inventing a value for one it was
                // not given would be a guess with someone else's memory.
                if matches!(self.peek(), Tok::Eq) {
                    return self.err(format!(
                        "foreign function `{name}`: parameter `{pname}` cannot have a default \
                         — a `dll` names a C function, which has no say in what a missing \
                         argument means; write a `sub` that wraps it"
                    ));
                }
                params.push((pname, pty));
                match self.peek() {
                    Tok::Comma => {
                        self.bump();
                        // A single trailing comma is allowed: `dll f(a: int,)`.
                        if matches!(self.peek(), Tok::RParen) {
                            break;
                        }
                    }
                    Tok::RParen => break,
                    other => {
                        return self
                            .err(format!("expected `,` or `)` in parameters, found {other:?}"))
                    }
                }
            }
        }
        self.expect(&Tok::RParen, "`)` after parameters")?;

        // `: type` — an optional return; its absence is a call-only (C `void`)
        // foreign function.
        let ret = if matches!(self.peek(), Tok::Colon) {
            self.bump();
            Some(self.ffi_type(&format!("foreign function `{name}`"), false)?)
        } else {
            None
        };

        // `from "lib"` — required, and the diagnostic says so rather than
        // blaming the newline it would otherwise trip over.
        match self.peek() {
            Tok::Ident(w) if w == "from" => {
                self.bump();
            }
            other => {
                return self.err(format!(
                    "foreign function `{name}` needs `from \"<library>\"`, found {other:?}"
                ))
            }
        }
        let library = match self.bump() {
            // A raw literal too: `from r"C:\\tools\\my.dll"` is the spelling a
            // Windows path wants, and a library name has no holes to lose.
            Tok::Str(s) | Tok::RawStr(s) => s,
            other => {
                return self.err(format!(
                    "expected a library name in quotes after `from`, found {other:?}"
                ))
            }
        };
        if library.is_empty() {
            return self.err(format!("foreign function `{name}`: the `from` library is empty"));
        }

        // `as "symbol"` — an optional override for the exported name, for when
        // the symbol a library exports is not a legal Kiln identifier or
        // simply differs from the name a program wants to call it by.
        let symbol = if matches!(self.peek(), Tok::Ident(w) if w == "as") {
            self.bump();
            match self.bump() {
                Tok::Str(s) | Tok::RawStr(s) if !s.is_empty() => Some(s),
                Tok::Str(_) | Tok::RawStr(_) => {
                    return self.err(format!(
                        "foreign function `{name}`: the `as` symbol name is empty"
                    ))
                }
                other => {
                    return self
                        .err(format!("expected a symbol name in quotes after `as`, found {other:?}"))
                }
            }
        } else {
            None
        };

        // `stdcall` / `cdecl` / `system` — an optional convention marker, last
        // on the line after `from` and any `as`. It is documentation and
        // forward-compat: the backend emits the same call for every target it
        // builds (all 64-bit, one C convention), and carries the marker for a
        // future 32-bit backend.
        let conv = self.call_conv_opt(&format!("foreign function `{name}`"))?;
        // `as` belongs before the convention (`from "x" as "y" system`). A
        // trailing `as` is the two written out of order — name that, rather than
        // let `expect(Newline)` blame the `as` token with a generic message.
        if conv.is_some() && matches!(self.peek(), Tok::Ident(w) if w == "as") {
            return self.err(format!(
                "foreign function `{name}`: write `as \"symbol\"` before the calling \
                 convention, not after it"
            ));
        }

        self.expect(&Tok::Newline, "newline after foreign function declaration")?;
        Ok(DllDecl {
            name,
            params,
            ret,
            library,
            symbol,
            conv,
            line: dll_line,
            name_span,
        })
    }

    /// A type in a C signature — a `dll` declaration's, or the one a
    /// `call through` site writes: the C-representable subset only. An array,
    /// a dictionary, a byte-set or a record is refused here — a later stage may
    /// pass one *by pointer*, but nothing in this stage marshals an aggregate
    /// across the boundary by value.
    ///
    /// `subject` is the whole naming phrase the diagnostic opens with
    /// (``foreign function `add` `` or `` `call through` ``), so the one rule
    /// serves both surfaces and their messages still read as themselves.
    fn ffi_type(&mut self, subject: &str, allow_record: bool) -> Result<Ty, ParseError> {
        let t = self.type_keyword()?;
        match t {
            Ty::Int | Ty::Int64 | Ty::Double | Ty::Bool | Ty::Text | Ty::Ptr => Ok(t),
            // A bare record name is allowed as a PARAMETER: a c-record parameter
            // means the C prototype takes a pointer to that struct, and the
            // caller passes the c-record (or `address of` it). The parser does
            // not yet know the record is `is c`, so the validator confirms it —
            // and rejects a heap record, or an undeclared name, with the line.
            // A record RETURN is refused here: handing a struct back by value is
            // a different ABI this stage does not implement — return a `ptr`.
            Ty::Record(_) if allow_record => Ok(t),
            Ty::Record(n) => self.err(format!(
                "{subject}: a C call cannot return the record `{n}` by \
                 value — return a `ptr` to it (or an out-parameter typed `{n}`)"
            )),
            other => self.err(format!(
                "{subject}: `{}` cannot cross the C boundary — a C \
                 signature takes int, int64, double, bool, text, ptr or a c-record parameter",
                other.as_str()
            )),
        }
    }

    /// An optional calling-convention marker at the end of a `dll` or `sub`
    /// header, before the newline: `stdcall`, `cdecl` or `system`. It is a soft
    /// keyword — a plain identifier the parser recognises only in this trailing
    /// position — so none of the three is reserved anywhere else. An identifier
    /// here that is not one of the three is refused, with the allowed set named,
    /// rather than left to trip the newline check with a worse message. `what`
    /// is the thing being declared, for that diagnostic.
    fn call_conv_opt(&mut self, what: &str) -> Result<Option<CallConv>, ParseError> {
        let word = match self.peek() {
            Tok::Ident(w) => w.clone(),
            _ => return Ok(None),
        };
        match CallConv::parse(&word) {
            Some(conv) => {
                self.bump();
                Ok(Some(conv))
            }
            None => self.err(format!(
                "{what}: `{word}` is not a calling convention — use stdcall, cdecl or system \
                 (or leave it off; on a 64-bit target they are the same)"
            )),
        }
    }

    /// ```text
    /// form      := "form" IDENT NEWLINE member* "end" NEWLINE
    /// member    := property | binding | component
    /// property  := IDENT "=" expr NEWLINE
    /// binding   := "on" IDENT ":" IDENT NEWLINE
    /// component := IDENT IDENT NEWLINE (property | binding)* "end" NEWLINE
    /// ```
    fn form(&mut self) -> Result<Form, ParseError> {
        let first_line = self.line();
        self.expect(&Tok::Form, "`form`")?;
        let name = self.ident("form name")?;
        self.expect(&Tok::Newline, "newline after form name")?;

        let mut form = Form {
            name,
            line_span: (first_line, first_line),
            properties: Vec::new(),
            handlers: Vec::new(),
            property_spans: Vec::new(),
            handler_spans: Vec::new(),
            children: Vec::new(),
        };
        loop {
            self.skip_newlines();
            match self.peek().clone() {
                Tok::End => {
                    form.line_span.1 = self.line();
                    self.bump();
                    break;
                }
                Tok::On => {
                    let (event, handler, span) = self.binding()?;
                    form.handlers.push((event, handler));
                    form.handler_spans.push(span);
                }
                Tok::Ident(first) => {
                    let span = self.span_at(self.pos);
                    self.bump();
                    if matches!(self.peek(), Tok::Eq) {
                        self.bump();
                        let value = self.expr()?;
                        self.expect(&Tok::Newline, "newline after property")?;
                        form.properties.push((first, value));
                        form.property_spans.push(span);
                    } else {
                        form.children.push(self.component(first)?);
                    }
                }
                Tok::Eof => {
                    return self.err("unexpected end of file inside `form` (missing `end`)")
                }
                other => {
                    return self.err(format!(
                        "expected a property, component, `on`, or `end`, found {other:?}"
                    ))
                }
            }
        }
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(form)
    }

    /// ```text
    /// ```text
    /// enum := "enum" IDENT NEWLINE (IDENT ("," IDENT)*  NEWLINE)+ "end" NEWLINE
    /// ```
    ///
    /// A set of named whole numbers, and nothing more: each member becomes a
    /// `const` named `Enum.member`, numbered from **1** in declaration order —
    /// the same place every position in Kiln counts from, so the first
    /// member of an enum and the first element of an array agree.
    ///
    /// The dotted name is why the members do not crowd the module's namespace:
    /// no identifier can contain a `.`, so `Colour.red` can only ever be
    /// reached by writing the enum's name, and two enums may both have a
    /// `red`. `Colour.red` folds to its constant in `postfix_with`, and
    /// `Colour` written as a type is `int` — an enum is a *name* for ints, not
    /// a type of its own.
    fn enum_def(&mut self) -> Result<Vec<ConstDef>, ParseError> {
        let line = self.line();
        self.bump(); // `enum`, a soft keyword consumed here
        let name = self.ident("enum name")?;
        self.expect(&Tok::Newline, "newline after the enum name")?;
        let mut out: Vec<ConstDef> = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(self.peek(), Tok::End) {
                self.bump();
                break;
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.err(format!("unexpected end of file inside `enum {name}` (missing `end`)"));
            }
            let member_span = self.span_at(self.pos);
            let member = self.ident("a member name")?;
            if out.iter().any(|c| c.name == format!("{name}.{member}")) {
                return self.err(format!("enum `{name}` declares `{member}` twice"));
            }
            // Numbered from 1, in declaration order.
            let value = i64::try_from(out.len() + 1).expect("an enum this long cannot be written");
            out.push(ConstDef {
                name: format!("{name}.{member}"),
                value: Expr::IntLit(value),
                ty: Ty::Int,
                line: member_span.line,
                name_span: member_span,
            });
            match self.peek() {
                // `red, green, blue` on one line, or one per line: a comma
                // between members is a separator either way.
                Tok::Comma => {
                    self.bump();
                }
                Tok::Newline | Tok::End => {}
                other => {
                    return self.err(format!(
                        "expected `,`, a newline or `end` after enum member `{member}`, \
                         found {other:?}"
                    ))
                }
            }
        }
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        if out.is_empty() {
            return Err(ParseError {
                line,
                msg: format!("enum `{name}` has no members"),
            });
        }
        Ok(out)
    }

    /// ```text
    /// const := "const" IDENT "=" literal NEWLINE
    /// literal := "-"? (INT | FLOAT | STRING | "true" | "false")
    /// ```
    ///
    /// A named constant is a single literal, nothing more: no arithmetic, no
    /// reference to another constant. Keeping the value a bare literal is what
    /// lets its type be known here and lets every later stage fold a reference
    /// to it by evaluating the one literal it stands for.
    fn const_def(&mut self) -> Result<ConstDef, ParseError> {
        let line = self.line();
        self.bump(); // `const`, a soft keyword consumed here
        let name_span = self.span_at(self.pos);
        let name = self.ident("constant name")?;
        self.expect(&Tok::Eq, "`=` after a constant name")?;
        let (value, ty) = self.const_literal(&name)?;
        self.expect(&Tok::Newline, "newline after a constant")?;
        Ok(ConstDef { name, value, ty, line, name_span })
    }

    /// The literal on the right of `const NAME =`: an integer, a double, a text
    /// or a bool, with an optional leading `-` folded into the number exactly
    /// as a negated literal is folded elsewhere. Anything richer — a variable,
    /// a call, an expression — is refused here, where the message can say a
    /// constant is a literal.
    fn const_literal(&mut self, name: &str) -> Result<(Expr, Ty), ParseError> {
        let negate = matches!(self.peek(), Tok::Minus);
        if negate {
            self.bump();
        }
        let (expr, ty) = match self.bump() {
            Tok::Int(v) => {
                let v = if negate { v.checked_neg().ok_or_else(|| ParseError {
                    line: self.line(),
                    msg: format!("constant `{name}`: the value is out of range"),
                })? } else { v };
                // An `int` literal that does not fit `i32` types `int64`, the
                // same rule `int_literal_type` applies everywhere else.
                let ty = if i32::try_from(v).is_ok() { Ty::Int } else { Ty::Int64 };
                (Expr::IntLit(v), ty)
            }
            // `const WS_POPUP = 0x8000_0000`. A constant is its literal, so it
            // keeps the literal's width-from-context behaviour: the declared
            // type here is what the pattern means *on its own*, and a use that
            // wants an `int64` still gets the 64-bit reading.
            Tok::Bits(v) if !negate => (Expr::BitsLit(v), bits_bare_type(v)),
            Tok::Bits(v) => {
                let n = bits_value(v).checked_neg().ok_or_else(|| ParseError {
                    line: self.line(),
                    msg: format!("constant `{name}`: the value is out of range"),
                })?;
                (Expr::IntLit(n), if i32::try_from(n).is_ok() { Ty::Int } else { Ty::Int64 })
            }
            Tok::Float(v) => (Expr::DoubleLit(if negate { -v } else { v }), Ty::Double),
            Tok::Str(_) | Tok::RawStr(_) | Tok::True | Tok::False if negate => {
                return self.err(format!(
                    "constant `{name}`: `-` may only precede a number"
                ))
            }
            // A constant is a literal, so a hole — whose value is only known
            // when the program runs — is refused here, where the message can
            // say so. `{{`/`}}` are still literal braces and are kept.
            Tok::Str(s) if text_has_hole(&s) => {
                return self.err(format!(
                    "constant `{name}` is a literal; string interpolation `{{...}}` needs \
                     values known only when the program runs"
                ))
            }
            Tok::Str(s) => (Expr::TextLit(unescape_braces(&s)), Ty::Text),
            // A raw literal has no holes to refuse and no braces to unescape —
            // its bytes are the constant.
            Tok::RawStr(s) => (Expr::TextLit(s), Ty::Text),
            Tok::True => (Expr::BoolLit(true), Ty::Bool),
            Tok::False => (Expr::BoolLit(false), Ty::Bool),
            other => {
                return self.err(format!(
                    "constant `{name}` must be a literal (a number, a text or a bool), \
                     found {other:?}"
                ))
            }
        };
        Ok((expr, ty))
    }

    /// ```text
    /// record := "record" IDENT NEWLINE (IDENT ":" type NEWLINE)+ "end" NEWLINE
    /// ```
    fn record_def(&mut self) -> Result<RecordDef, ParseError> {
        let line = self.line();
        self.bump(); // `record`
        let name = self.ident("record name")?;
        // `record NAME is c` marks a C-layout record. `is` and `c` are two
        // ordinary idents in this one spot — soft, like `record` itself — so a
        // record whose fields happen to be named `is` or `c` is unaffected: the
        // marker is only these two words, in this order, before the newline.
        let is_c = if matches!(self.peek(), Tok::Ident(w) if w == "is") {
            self.bump(); // `is`
            match self.bump() {
                Tok::Ident(w) if w == "c" => true,
                other => {
                    return self.err(format!(
                        "expected `c` after `is` in `record {name} is c`, found {other:?}"
                    ))
                }
            }
        } else {
            false
        };
        self.expect(&Tok::Newline, "newline after record name")?;
        let mut fields: Vec<(String, Ty)> = Vec::new();
        loop {
            self.skip_newlines();
            match self.peek().clone() {
                Tok::End => {
                    self.bump();
                    break;
                }
                Tok::Ident(field) => {
                    self.bump();
                    self.expect(&Tok::Colon, "`:` after a field name")?;
                    let ty = if is_c {
                        self.crecord_field_type(&name)?
                    } else {
                        self.type_keyword()?
                    };
                    self.expect(&Tok::Newline, "newline after a field")?;
                    if fields.iter().any(|(n, _)| *n == field) {
                        return self
                            .err(format!("record `{name}` declares field `{field}` twice"));
                    }
                    fields.push((field, ty));
                }
                Tok::Eof => {
                    return self.err("unexpected end of file inside `record` (missing `end`)")
                }
                other => {
                    return self.err(format!("expected a field or `end`, found {other:?}"))
                }
            }
        }
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        // Every field is named at construction, so a record with none could
        // only be written `point()` — which is a call, and would need a rule of
        // its own for no gain.
        if fields.is_empty() {
            return Err(ParseError {
                line,
                msg: format!(
                    "record `{name}` has no fields — a record names a group of values"
                ),
            });
        }
        Ok(RecordDef { name, fields, is_c, line })
    }

    /// A field type inside a `record NAME is c`: the C-representable scalar set
    /// a `dll` allows, plus the widths that mean something only in a layout —
    /// `byte` (a `char`/`uint8_t`), `int16` / `word` (a `WORD`/`uint16_t`) and
    /// `float` (a 4-byte IEEE float) — plus another `is c` record nested by
    /// value, plus a fixed inline array `T[N]` of any of those.
    ///
    /// The word is read here rather than through `type_keyword`, because
    /// `type_keyword` reads `[` as the list suffix and would choke on the count
    /// in `byte[16]`, and because `byte`, `int16`, `word` and `float` are
    /// ordinary words everywhere else — they are recognised by spelling, in
    /// this one position, so nothing about them leaks into the language at
    /// large.
    ///
    /// Whether a nested record name exists and is `is c` is the validator's
    /// question, exactly as for any other type: a record may be declared after
    /// the one that nests it, and a one-pass parser has not read it yet.
    fn crecord_field_type(&mut self, rec: &str) -> Result<Ty, ParseError> {
        let elem = match self.bump() {
            Tok::Ident(w) => match w.as_str() {
                "byte" => Ty::Byte,
                // Two spellings of one width: `int16` says what it is, `word`
                // is what a Win32 header calls it, and a transcription reads
                // better for having the name it was transcribed from.
                "int16" | "word" => Ty::Int16,
                "float" => Ty::Float,
                other => match Ty::from_keyword(other) {
                    Some(t @ (Ty::Int | Ty::Int64 | Ty::Double | Ty::Bool | Ty::Text | Ty::Ptr)) => t,
                    Some(t) => {
                        return self.err(format!(
                            "c-record `{rec}`: `{}` is not a C-layout field type — use int, \
                             int64, int16 (word), double, float, bool, text, ptr, byte, \
                             another `is c` record, or `T[N]`",
                            t.as_str()
                        ))
                    }
                    None => Ty::Record(intern(other)),
                },
            },
            other => {
                return self.err(format!(
                    "c-record `{rec}`: expected a field type, found {other:?}"
                ))
            }
        };
        // A dictionary is a runtime-owned object with no by-value C shape, and
        // it has no place in a layout — say so where the `{` is, rather than
        // leaving the reader a complaint about a missing newline.
        if matches!(self.peek(), Tok::LBrace) {
            return self.err(format!(
                "c-record `{rec}`: `{}{{}}` is not a C-layout field type — a dictionary is a \
                 runtime-owned object; hold a `ptr` to bytes instead",
                elem.as_str()
            ));
        }
        // `[N]` — a fixed inline array. The count is part of the type, so it
        // must be a literal here; `size of` and every offset are compile-time
        // numbers and a count that was not would make neither.
        if !matches!(self.peek(), Tok::LBracket) {
            return Ok(elem);
        }
        self.bump();
        let count = match self.bump() {
            Tok::Int(n) if n >= 1 => n,
            // `T[]` is a Kiln list: a pointer to a runtime-owned array, and
            // not a block of bytes a struct can hold.
            Tok::RBracket => {
                return self.err(format!(
                    "c-record `{rec}`: `{}[]` is not a C-layout field type — a list is a \
                     runtime-owned object; give a count for a fixed inline array \
                     (`{}[4]`), or hold a `ptr` to bytes",
                    elem.as_str(),
                    elem.as_str()
                ))
            }
            Tok::Int(n) => {
                return self.err(format!(
                    "c-record `{rec}`: an array field holds at least one element, not {n}"
                ))
            }
            other => {
                return self.err(format!(
                    "c-record `{rec}`: an array field needs a literal count, as in \
                     `rgb: byte[32]` — found {other:?}"
                ))
            }
        };
        self.expect(&Tok::RBracket, "`]` after an array field's count")?;
        if matches!(self.peek(), Tok::LBracket) {
            return self.err(format!(
                "c-record `{rec}`: an array field is one dimension — `byte[4][4]` is not a \
                 field type; nest a record that holds `byte[4]` instead"
            ));
        }
        if count > u32::MAX as i64 {
            return self.err(format!("c-record `{rec}`: array count {count} is too large"));
        }
        Ok(carray(elem, count as u32))
    }

    /// A component instance; `type_name` has already been consumed.
    fn component(&mut self, type_name: String) -> Result<Component, ParseError> {
        let id = self.ident("component id")?;
        self.expect(&Tok::Newline, "newline after component id")?;
        let mut c = Component {
            type_name,
            id,
            properties: Vec::new(),
            handlers: Vec::new(),
            property_spans: Vec::new(),
            handler_spans: Vec::new(),
        };
        loop {
            self.skip_newlines();
            match self.peek().clone() {
                Tok::End => {
                    self.bump();
                    break;
                }
                Tok::On => {
                    let (event, handler, span) = self.binding()?;
                    c.handlers.push((event, handler));
                    c.handler_spans.push(span);
                }
                Tok::Ident(name) => {
                    let span = self.span_at(self.pos);
                    self.bump();
                    self.expect(&Tok::Eq, "`=` after property name")?;
                    let value = self.expr()?;
                    self.expect(&Tok::Newline, "newline after property")?;
                    c.properties.push((name, value));
                    c.property_spans.push(span);
                }
                Tok::Eof => {
                    return self.err("unexpected end of file inside component (missing `end`)")
                }
                other => {
                    return self.err(format!(
                        "expected a property, `on`, or `end`, found {other:?}"
                    ))
                }
            }
        }
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(c)
    }

    /// `on <event>: <subroutine>`, and the span of the event name — the part
    /// of the line a diagnostic about the binding points at.
    fn binding(&mut self) -> Result<(String, String, Span), ParseError> {
        self.expect(&Tok::On, "`on`")?;
        let span = self.span_at(self.pos);
        let event = self.ident("event name")?;
        self.expect(&Tok::Colon, "`:` after event name")?;
        let handler = self.ident("handler subroutine name")?;
        self.expect(&Tok::Newline, "newline after event binding")?;
        Ok((event, handler, span))
    }

    /// A module-level `var NAME: TY = EXPR`.
    fn global_var(&mut self) -> Result<GlobalVar, ParseError> {
        self.expect(&Tok::Var, "`var`")?;
        let name = self.ident("variable name")?;
        self.expect(&Tok::Colon, "`:` after variable name")?;
        let ty = self.type_keyword()?;
        self.expect(&Tok::Eq, "`=`")?;
        let value = self.expr()?;
        self.expect(&Tok::Newline, "newline after `var`")?;
        Ok(GlobalVar { name, ty, value })
    }

    /// ```text
    /// type := IDENT ("[" "]")?
    /// ```
    ///
    /// The `[]` suffix follows the element type rather than wrapping it, so
    /// `int[]` reads as "ints" in the same left-to-right order the rest of a
    /// declaration does.
    fn type_keyword(&mut self) -> Result<Ty, ParseError> {
        let t = self.type_inner()?;
        if matches!(self.peek(), Tok::Question) {
            return self.err(
                "`?` marks a local that may hold no value, and only a local: an optional is a \
                 value plus the hidden truth beside it, so it cannot cross into a parameter, a \
                 return type, a list, a field or a module variable. Write `let v: T? = ...` \
                 inside a subroutine and unwrap it there",
            );
        }
        Ok(t)
    }

    /// The type in a `let` / `var` **inside a subroutine**, where a trailing `?`
    /// is allowed: `let home: text? = env_get("HOME")`. Nowhere else accepts one
    /// — see [`Ty::Optional`] for why.
    fn local_type(&mut self) -> Result<Ty, ParseError> {
        let base = self.type_inner()?;
        if !matches!(self.peek(), Tok::Question) {
            return Ok(base);
        }
        self.bump(); // `?`
        match Elem::from_ty(base) {
            Some(e) => Ok(Ty::Optional(e)),
            None => self.err(format!(
                "`{}?` is not a type — an optional holds one value (a number, a text, a truth \
                 value or a record), and {} is not one of those",
                base.as_str(),
                base.as_str()
            )),
        }
    }

    fn type_inner(&mut self) -> Result<Ty, ParseError> {
        let base = match self.bump() {
            Tok::Ident(w) => match Ty::from_keyword(&w) {
                Some(t) => t,
                // An enum's name is a name for `int`s, so writing it as a type
                // *is* writing `int`: a parameter typed `Colour` takes
                // `Colour.red` and takes `1`, and every rule about ints — a
                // comparison, arithmetic, crossing to C — applies unchanged.
                // The alternative, a distinct type that coerces, would buy one
                // diagnostic and cost a coercion rule in every checker.
                None if self.enums.contains_key(&w) => Ty::Int,
                // Any other word is a record type, by name. Whether a record
                // with that name exists is the validator's question: a record
                // may be declared after the subroutine that uses it, and a
                // one-pass parser has not read it yet.
                None => Ty::Record(intern(&w)),
            },
            other => return self.err(format!("expected a type, found {other:?}")),
        };
        // `[]` for a list, `{}` for a dictionary keyed by text. The suffix
        // follows the element type in both, so `int[]` and `int{}` read as
        // "ints" and "ints by name" in the order the rest of a declaration does.
        let (close, closing, wrap, what): (Tok, &str, fn(Elem) -> Ty, &str) = match self.peek() {
            Tok::LBracket => (Tok::RBracket, "`]` after `[` in an array type", Ty::Array, "a list"),
            Tok::LBrace => (Tok::RBrace, "`}` after `{` in a dictionary type", Ty::Dict, "a dictionary"),
            _ => return Ok(base),
        };
        self.bump();
        self.expect(&close, closing)?;
        // A second suffix is caught here rather than left to the checker so the
        // message names the real limitation instead of the token that follows.
        if matches!(self.peek(), Tok::LBracket | Tok::LBrace) {
            return self.err(format!(
                "{what} cannot hold arrays or dictionaries — `int[][]` is not a type"
            ));
        }
        match Elem::from_ty(base) {
            Some(e) => Ok(wrap(e)),
            None => self.err(format!("{what} cannot hold {} values", base.as_str())),
        }
    }

    /// A statement starting with an identifier: one of the soft-keyword
    /// statements, an assignment, or a property-set.
    fn stmt_ident(&mut self) -> Result<Option<Stmt>, ParseError> {
        let start = self.pos;
        // The statement words that are soft keywords. Each is claimed only when
        // what follows could not continue an assignment to a variable of that
        // name — so `match = 1`, `repeat[0] = x`, `assert.text = "hi"` and
        // `repeat mod= 2` all keep meaning what they always did. `repeat` is
        // also a core command (`repeat("ab", 3)`), which is untouched: a bare
        // call is spelled with `call`, so the word out here never was one.
        if self.leads_stmt("match") {
            return Ok(Some(self.stmt_match()?));
        }
        if self.leads_stmt("repeat") {
            return Ok(Some(self.stmt_repeat()?));
        }
        if self.leads_stmt("assert") {
            return self.stmt_assert();
        }
        if self.leads_stmt("defer") {
            return Ok(Some(self.stmt_defer()?));
        }
        // `check file_write_text(p, s)` — run the call for its effect and leave
        // the subroutine if it failed. The result is discarded, exactly as a
        // `call` discards one.
        if self.is_check_kw() {
            self.bump(); // `check`
            let e = self.expr()?;
            let Expr::Call { cmd, args } = e else {
                return self.err(
                    "`check` on its own line guards a call — write `check name(...)`, or bind the \
                     value with `let x: T = check name(...)`",
                );
            };
            // Deliberately not `finish_simple`: a trailing one-line `if` would
            // wrap the call alone and leave the guard running unconditionally,
            // which would return on a failure the condition said to skip.
            match self.peek() {
                Tok::Newline => {
                    self.bump();
                }
                Tok::Eof => {}
                other => {
                    return self.err(format!(
                        "expected a newline after `check`, found {other:?}; a one-line `if` cannot \
                         guard a `check`, because the propagation would not be guarded with it"
                    ))
                }
            }
            let stmt = self.finish(StmtKind::Call { cmd, args }, start);
            self.pending.push(self.check_guard(start)?);
            return Ok(Some(stmt));
        }
        // `increment x` / `decrement x` are soft keywords: the word is the
        // statement only when a target name follows it. `increment = 5` (an
        // assignment to a variable named `increment`), `increment[0] = ...` and
        // every other spelling stay ordinary, because there the next token is
        // not the start of a target name. Both desugar to `target = target ± 1`
        // on any place a compound assignment reaches.
        if matches!(self.peek(), Tok::Ident(w) if w == "increment" || w == "decrement")
            && matches!(self.peek_at(1), Tok::Ident(_))
        {
            let up = matches!(self.peek(), Tok::Ident(w) if w == "increment");
            self.bump(); // the `increment` / `decrement` keyword
            let target = self.ident("the name to increment or decrement")?;
            let lv = self.parse_lvalue(target)?;
            let op = if up { BinOp::Add } else { BinOp::Sub };
            let value = Expr::Bin(op, Box::new(lv.read()), Box::new(Expr::IntLit(1)));
            let kind = lv.write(value);
            return self.finish_simple(kind, start).map(Some);
        }
        let name = self.ident("variable or component name")?;
        let lv = self.parse_lvalue(name)?;
        // `=` or a compound assignment (`+= -= *= /= mod= &=`).
        let Some(op) = self.assign_op() else {
            return self.err(format!(
                "expected `=`, a compound assignment (`+=`, `-=`, `*=`, `/=`, `mod=`, `&=`), \
                 `[` (element) or `.` (property), found {:?}",
                self.peek()
            ));
        };
        let rhs = self.expr()?;
        let value = apply_assign_op(op, lv.read(), rhs);
        let kind = lv.write(value);
        self.finish_simple(kind, start).map(Some)
    }

    /// Parse an assignment target, with the leading `name` already read: a bare
    /// variable, one element (`xs[i]`), one property (`c.p`), or a multi-step
    /// path into a c-record (`r.pt.x`, `r.rgb[3]`). The result carries enough to
    /// build both a *read* of the target (a compound assignment's left operand)
    /// and the *write* statement — so `x = e`, `x += e` and `increment x` all
    /// reach the target through this one parser.
    fn parse_lvalue(&mut self, name: String) -> Result<Lvalue, ParseError> {
        match self.peek() {
            Tok::LBracket => {
                self.bump();
                let index = self.expr()?;
                // `xs[a..b] = ...` — a slice is a value, not a place: it is a
                // freshly built run, so assigning to it would write into
                // something the program cannot reach afterwards. Said here
                // rather than left as a stray `..` in an index.
                if matches!(self.peek(), Tok::DotDot) {
                    return self.err(
                        "a slice `a..b` is a value, not a place — assign to one position \
                         at a time, or build the new collection and assign that"
                            .to_string(),
                    );
                }
                self.expect(&Tok::RBracket, "`]` after the index")?;
                // `xs[i][j]` / `xs[i].f`: more than one step is a path.
                if matches!(self.peek(), Tok::Dot | Tok::LBracket) {
                    let base = Expr::Index {
                        base: Box::new(Expr::Var(name)),
                        index: Box::new(index),
                    };
                    return Ok(Lvalue::Place(self.parse_place_tail(base)?));
                }
                Ok(Lvalue::Index { name, index })
            }
            Tok::Dot => {
                self.bump();
                let property = self.ident("property name")?;
                // `r.pt.x` / `r.rgb[3]`: everything past the first step is a
                // path into a c-record. One step stays the form that also reaches
                // a heap record and a component property.
                if matches!(self.peek(), Tok::Dot | Tok::LBracket) {
                    let base = Expr::GetProperty {
                        component: name,
                        property,
                    };
                    return Ok(Lvalue::Place(self.parse_place_tail(base)?));
                }
                Ok(Lvalue::Property {
                    component: name,
                    property,
                })
            }
            _ => Ok(Lvalue::Var(name)),
        }
    }

    /// The `.field` / `[index]` steps of a multi-step target after its first
    /// step `base`, built as the very `Expr` a read of it would be — so the
    /// checker and backend have one shape to handle, not a mirror grammar for
    /// the left-hand side.
    fn parse_place_tail(&mut self, base: Expr) -> Result<Expr, ParseError> {
        let mut place = base;
        loop {
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let field = self.ident("a field name after `.`")?;
                    place = Expr::Field {
                        base: Box::new(place),
                        name: field,
                    };
                }
                Tok::LBracket => {
                    self.bump();
                    let index = self.expr()?;
                    // `xs[a..b] = ...` — a slice is a value, not a place: it is a
                    // freshly built run, so assigning to it would write into
                    // something the program cannot reach afterwards. Said here
                    // rather than left as a stray `..` in an index.
                    if matches!(self.peek(), Tok::DotDot) {
                        return self.err(
                            "a slice `a..b` is a value, not a place — assign to one position \
                             at a time, or build the new collection and assign that"
                                .to_string(),
                        );
                    }
                    self.expect(&Tok::RBracket, "`]` after the index")?;
                    place = Expr::Index {
                        base: Box::new(place),
                        index: Box::new(index),
                    };
                }
                _ => break,
            }
        }
        Ok(place)
    }

    /// Read an assignment operator in target position: a plain `=`, or one of
    /// the compound forms. `None` when the next token is not one, so the caller
    /// makes its own diagnostic. `mod=` is spelled with the word `mod` (there is
    /// no `mod` token), recognised only here — a variable named `mod` is still
    /// assignable with the two tokens `mod =`.
    fn assign_op(&mut self) -> Option<AssignOp> {
        if matches!(self.peek(), Tok::Ident(w) if w == "mod") && matches!(self.peek_at(1), Tok::Eq)
        {
            self.bump(); // `mod`
            self.bump(); // `=`
            return Some(AssignOp::Bin(BinOp::Rem));
        }
        let op = match self.peek() {
            Tok::Eq => AssignOp::Plain,
            Tok::PlusEq => AssignOp::Bin(BinOp::Add),
            Tok::MinusEq => AssignOp::Bin(BinOp::Sub),
            Tok::StarEq => AssignOp::Bin(BinOp::Mul),
            Tok::SlashEq => AssignOp::Bin(BinOp::Div),
            Tok::AmpEq => AssignOp::Concat,
            _ => return None,
        };
        self.bump();
        Some(op)
    }

    /// End a simple statement (call, assignment, return, increment): consume the
    /// trailing newline, or fold a one-line `STMT if COND` suffix into an
    /// ordinary `if COND \n STMT \n end`. The block `if` never reaches here, so
    /// it is untouched.
    fn finish_simple(&mut self, kind: StmtKind, start: usize) -> Result<Stmt, ParseError> {
        if matches!(self.peek(), Tok::If) {
            // The statement, spanning up to the `if`, becomes the sole arm body.
            let inner = self.finish(kind, start);
            let if_start = self.pos;
            self.bump(); // `if`
            let cond = self.expr()?;
            self.expect(&Tok::Newline, "newline after a one-line `if` condition")?;
            return Ok(self.finish(
                StmtKind::If {
                    arms: vec![(cond, vec![inner])],
                    otherwise: None,
                },
                if_start,
            ));
        }
        match self.peek() {
            Tok::Newline => {
                self.bump();
            }
            // The final statement of a file may have no trailing newline.
            Tok::Eof => {}
            other => {
                return self.err(format!(
                    "expected a newline or a one-line `if` after the statement, found {other:?}"
                ))
            }
        }
        Ok(self.finish(kind, start))
    }

    /// The words that are operators when they follow a complete operand.
    ///
    /// `check` is claimed only when an identifier follows it, and one of these
    /// following it means the `check` was itself an operand — `check band mask`
    /// is a bitwise-and of a variable named `check`, and it must stay one.
    const INFIX_WORDS: &'static [&'static str] = &[
        "band", "bor", "bxor", "shl", "shr", "ushr", "in", "otherwise", "then", "mod", "to",
        "step", "at", "each", "of", "from", "as",
    ];

    /// Whether the cursor is on `check` used as the propagation keyword.
    ///
    /// A soft keyword: it means this only when a *name* follows, which is what
    /// `check <call>` always looks like and what an expression starting with a
    /// variable called `check` never does — `check = 1`, `check + 1`,
    /// `check(1)` and `check[0]` all keep their old meaning.
    fn is_check_kw(&self) -> bool {
        if self.word() != Some("check") {
            return false;
        }
        match self.peek_at(1) {
            Tok::Ident(w) => !Self::INFIX_WORDS.contains(&w.as_str()),
            _ => false,
        }
    }

    /// The guard `check` expands to: `if last_error_code() <> 0 <return> end`.
    ///
    /// This is the whole of the feature — the value itself is bound by the
    /// statement that carries it, and this is the four lines a program writes
    /// by hand today, emitted straight after. `start` is the `check` token, so
    /// a diagnostic about the synthesized `return` points at what was written.
    fn check_guard(&self, start: usize) -> Result<Stmt, ParseError> {
        if !self.in_sub {
            return Err(ParseError {
                line: self.toks[start].line,
                msg: "`check` returns from the subroutine it is in, and there is none here"
                    .to_string(),
            });
        }
        // The value the early `return` carries: the sentinel a failing command
        // of that type already returns, so a caller that checks the result sees
        // the same failure it would have seen from the call itself.
        let value = match self.cur_ret {
            None => None,
            Some(Ty::Int) | Some(Ty::Int64) => Some(Expr::IntLit(0)),
            Some(Ty::Double) => Some(Expr::DoubleLit(0.0)),
            Some(Ty::Text) => Some(Expr::TextLit(String::new())),
            Some(Ty::Bool) => Some(Expr::BoolLit(false)),
            Some(Ty::Ptr) => Some(Expr::Call {
                cmd: "ptr_null".to_string(),
                args: Vec::new(),
            }),
            Some(t) => {
                return Err(ParseError {
                    line: self.toks[start].line,
                    msg: format!(
                        "`check` returns early on failure, and {} has no failure value to return — \
                         test `last_error_code()` yourself here",
                        t.as_str()
                    ),
                })
            }
        };
        let ret = self.finish(StmtKind::Return { value }, start);
        Ok(self.finish(
            StmtKind::If {
                arms: vec![(
                    Expr::Cmp(
                        CmpOp::Ne,
                        Box::new(Expr::Call {
                            cmd: "last_error_code".to_string(),
                            args: Vec::new(),
                        }),
                        Box::new(Expr::IntLit(0)),
                    ),
                    vec![ret],
                )],
                otherwise: None,
            },
            start,
        ))
    }

    /// Whether the cursor is on `word` used as a statement keyword.
    ///
    /// Every one of these words is a SOFT keyword: it leads a statement only
    /// when what follows could not be the rest of an assignment to a variable
    /// spelled the same way. That is the whole rule, and it is what keeps
    /// `match`, `repeat` and `assert` usable as ordinary names.
    fn leads_stmt(&self, word: &str) -> bool {
        if self.word() != Some(word) {
            return false;
        }
        match self.peek_at(1) {
            // `match = 1`, `repeat += 2`, `assert[0] = x`, `match.text = "hi"`:
            // an assignment or a path into one, all of them the old meaning.
            Tok::Eq
            | Tok::PlusEq
            | Tok::MinusEq
            | Tok::StarEq
            | Tok::SlashEq
            | Tok::AmpEq
            | Tok::LBracket
            | Tok::Dot => false,
            // `repeat mod= 2` — the one compound assignment spelled with a word.
            Tok::Ident(w) if w == "mod" && matches!(self.peek_at(2), Tok::Eq) => false,
            // Nothing follows, so nothing is being led.
            Tok::Newline | Tok::Eof => false,
            _ => true,
        }
    }

    /// A name no program can write, for a binding the program never sees.
    /// `$` is not an identifier character, and the counter runs over the whole
    /// parse so two `repeat` loops in one subroutine cannot collide.
    fn fresh_hidden(&mut self, tag: &str) -> String {
        self.hidden += 1;
        format!("${tag}${}", self.hidden)
    }

    /// `defer STMT` — one statement, run when the block it was written in is
    /// left, whichever way it is left. See [`StmtKind::Defer`] for what that
    /// expands to; the parser's whole job is to read the statement and to
    /// refuse the shapes the expansion could not honour.
    ///
    /// `defer` is a soft keyword: it leads the statement only where a following
    /// token could not continue an assignment to a variable of that name, so
    /// `defer = 1` and `defer.text = "x"` are what they always were.
    fn stmt_defer(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.bump(); // `defer`
        let line = self.line();
        let Some(inner) = self.statement()? else {
            // `defer assert ...` in a release build: the statement compiled to
            // nothing, so there is nothing to defer.
            return Err(ParseError {
                line,
                msg: "`defer` needs a statement to run".into(),
            });
        };
        // `check` leaves a guard behind it (`self.pending`), and that guard
        // would run where the `defer` was written while the call it guards ran
        // at the end — two halves of one statement in two places.
        if !self.pending.is_empty() {
            self.pending.clear();
            return Err(ParseError {
                line,
                msg: "`defer check ...` would run the failure guard where the `defer` is written \
                      and the call at the end of the block; write `defer call name(...)`".into(),
            });
        }
        let refuse = |what: &str| {
            Err(ParseError {
                line,
                msg: format!(
                    "`defer` runs one simple statement when the block ends — {what}. Write the \
                     call, assignment or property write you want to happen last"
                ),
            })
        };
        match &inner.kind {
            // The one-line `STMT if COND` folds into an `if` before this sees
            // it, so both spellings land here and get the one message.
            StmtKind::If { .. } => {
                return refuse(
                    "an `if` is a block of its own, and its end is not the end of this one",
                )
            }
            StmtKind::While { .. } | StmtKind::For { .. } | StmtKind::ForEach { .. } => {
                return refuse("a loop is a block of its own")
            }
            StmtKind::Match { .. } => return refuse("a `match` is a block of its own"),
            StmtKind::Let { .. } | StmtKind::LetInfer { .. } => {
                return refuse("a binding made at the end of a block could never be read")
            }
            StmtKind::Return { .. } => {
                return refuse("a deferred `return` would leave the subroutine from its own cleanup")
            }
            StmtKind::Break | StmtKind::Continue => {
                return refuse("a deferred jump would leave the loop from its own cleanup")
            }
            StmtKind::Defer(_) => return refuse("a `defer` of a `defer` says nothing the first did not"),
            _ => {}
        }
        Ok(self.finish(StmtKind::Defer(Box::new(inner)), start))
    }

    /// ```text
    /// match := "match" expr NEWLINE
    ///          ("when" expr ("," expr)* ":" (stmt | NEWLINE stmt*))+
    ///          ("else" ":" (stmt | NEWLINE stmt*))?
    ///          "end" NEWLINE
    /// ```
    ///
    /// An if/else-if chain that tests one value with `=`, written once. The
    /// value is evaluated **once** — see `StmtKind::Match` for why that is what
    /// makes this a node rather than a rewrite here — and each `when` may list
    /// several values, which means any of them. `else` is optional, and a
    /// `match` that matches nothing does nothing.
    ///
    /// Real pattern matching — binding a name out of the value, matching a
    /// shape — is deliberately not here: this compares, and comparison is all
    /// an if-chain can do.
    fn stmt_match(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.bump(); // `match`, a soft keyword consumed here
        let scrutinee = self.expr()?;
        self.expect(&Tok::Newline, "newline after the value a `match` tests")?;
        let mut arms: Vec<(Vec<Expr>, Vec<Stmt>)> = Vec::new();
        let mut otherwise = None;
        loop {
            self.skip_newlines();
            match self.peek() {
                Tok::Ident(w) if w == "when" => {
                    self.bump();
                    let mut values = vec![self.expr()?];
                    while matches!(self.peek(), Tok::Comma) {
                        self.bump();
                        values.push(self.expr()?);
                    }
                    self.expect(&Tok::Colon, "`:` after the values a `when` matches")?;
                    arms.push((values, self.arm_body()?));
                }
                Tok::Else => {
                    self.bump();
                    self.expect(&Tok::Colon, "`:` after `else`")?;
                    otherwise = Some(self.arm_body()?);
                    self.skip_newlines();
                    self.expect(&Tok::End, "`end` after the `else` of a `match`")?;
                    break;
                }
                Tok::End => {
                    self.bump();
                    break;
                }
                other => {
                    return self.err(format!(
                        "expected `when`, `else` or `end` in a `match`, found {other:?}"
                    ))
                }
            }
        }
        if arms.is_empty() {
            return self.err("a `match` needs at least one `when`");
        }
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(
            StmtKind::Match {
                scrutinee,
                arms,
                otherwise,
            },
            start,
        ))
    }

    /// The body of one `when` or `else`, with the `:` already read: either the
    /// rest of the line as a single statement, or — when the line ends there —
    /// the statements up to the next `when`, the `else`, or the `end`.
    fn arm_body(&mut self) -> Result<Vec<Stmt>, ParseError> {
        if !matches!(self.peek(), Tok::Newline) {
            let mut body: Vec<Stmt> = self.statement()?.into_iter().collect();
            // A desugar that produced a follow-on statement (a `check` guard)
            // belongs inside the arm, directly after what it guards — the two
            // statement loops drain this for the same reason.
            body.extend(self.pending.drain(..));
            return Ok(body);
        }
        self.bump(); // the newline
        self.block_until(&|p: &Parser| {
            matches!(p.peek(), Tok::Else | Tok::End) || p.leads_stmt("when")
        })
    }

    /// `repeat N times NEWLINE ... end` — a counted loop with no visible index.
    ///
    /// Sugar for `for <hidden> = 1 to N`, and desugared to exactly that: the
    /// count is evaluated once before the first turn, `break` and `continue`
    /// behave, and a count of zero runs the body no times — every one of those
    /// is the counting loop's own behaviour, because this *is* the counting
    /// loop with its counter hidden. `times` is a soft keyword in this one
    /// position.
    fn stmt_repeat(&mut self) -> Result<Stmt, ParseError> {
        let head = self.pos;
        self.bump(); // `repeat`, a soft keyword consumed here
        let count = self.expr()?;
        match self.peek() {
            Tok::Ident(w) if w == "times" => {
                self.bump();
            }
            other => {
                return self.err(format!(
                    "expected `times` after the count in `repeat N times`, found {other:?}"
                ))
            }
        }
        self.expect(&Tok::Newline, "newline after `repeat N times`")?;
        let body = self.block(&[Tok::End])?;
        self.expect(&Tok::End, "`end`")?;
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        let var = self.fresh_hidden("repeat");
        Ok(self.finish(
            StmtKind::For {
                var,
                start: Expr::IntLit(1),
                limit: count,
                step: 1,
                body,
            },
            head,
        ))
    }

    /// ```text
    /// assert := "assert" expr ("," expr)? NEWLINE
    /// ```
    ///
    /// Sugar for `if not COND` around one call: `assert_failed(msg)` prints the
    /// message and stops the program. With no message of its own an `assert`
    /// quotes the condition as it was written, which is the message a reader
    /// wanted anyway.
    ///
    /// In a **release** build the whole statement is dropped here — no branch,
    /// no call, no message in the binary. The condition is still parsed, so a
    /// misspelt `assert` is a compile error in both builds; only the emitted
    /// code differs.
    fn stmt_assert(&mut self) -> Result<Option<Stmt>, ParseError> {
        let start = self.pos;
        self.bump(); // `assert`, a soft keyword consumed here
        let cond_start = self.pos;
        let cond = self.expr()?;
        let cond_end = self.pos;
        let message = if matches!(self.peek(), Tok::Comma) {
            self.bump();
            self.expr()?
        } else {
            Expr::TextLit(format!(
                "assertion failed: {}",
                self.source_between(cond_start, cond_end)
            ))
        };
        match self.peek() {
            Tok::Newline => {
                self.bump();
            }
            Tok::Eof => {}
            other => {
                return self.err(format!(
                    "expected a newline after `assert`, found {other:?} — a message goes \
                     after a comma, as in `assert n > 0, \"n must be positive\"`"
                ))
            }
        }
        if !self.opts.asserts {
            return Ok(None);
        }
        let fail = self.finish(
            StmtKind::Call {
                cmd: "assert_failed".to_string(),
                args: vec![message],
            },
            start,
        );
        Ok(Some(self.finish(
            StmtKind::If {
                arms: vec![(Expr::Not(Box::new(cond)), vec![fail])],
                otherwise: None,
            },
            start,
        )))
    }

    /// The source text the tokens `[from, to)` were read from, for an
    /// `assert`'s own account of its condition. Sliced out of the line rather
    /// than printed back from the tokens, so what the message quotes is what
    /// the author actually typed, spacing and all. A condition spread over more
    /// than one line is not one, so only the first line's part is quoted.
    fn source_between(&self, from: usize, to: usize) -> String {
        let first = &self.toks[from.min(self.toks.len() - 1)];
        let last = &self.toks[to.saturating_sub(1).min(self.toks.len() - 1)];
        let Some(line) = self.lines.get(first.line.saturating_sub(1)) else {
            return String::new();
        };
        let start = first.col.saturating_sub(1);
        let end = if last.line == first.line {
            last.end_col.saturating_sub(1).min(line.len())
        } else {
            line.len()
        };
        line.get(start..end).unwrap_or("").trim().to_string()
    }

    fn stmt_let(&mut self, mutable: bool) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.expect(
            if mutable { &Tok::Var } else { &Tok::Let },
            "`let` or `var`",
        )?;
        let name = self.ident("variable name")?;
        // `let n = length(xs)` — the annotation may be left out when the
        // initializer says what the type is. `None` here means "read it off the
        // value", which only the desugar (registry in hand) can do.
        let ty = if matches!(self.peek(), Tok::Colon) {
            self.bump();
            Some(self.local_type()?)
        } else {
            if matches!(self.peek(), Tok::Newline) {
                return self.err(format!(
                    "`{name}` has neither a type nor a value — write `{}{name}: TYPE` or \
                     `{}{name} = EXPR`",
                    if mutable { "var " } else { "let " },
                    if mutable { "var " } else { "let " }
                ));
            }
            None
        };
        // `var r: RECT` with no `= EXPR` is a zeroed c-record local. The parser
        // cannot yet know `RECT` is a c-record (one pass), so it accepts the
        // omitted initializer for any type and hands the checker a `ZeroInit`,
        // which the checker allows only for a c-record `var`. A missing `=` on
        // an ordinary type becomes a clear "only a c-record may be left
        // uninitialised" there, with the declared type to name.
        // `let d: text = check file_read_text(p)` — bind the value as usual,
        // and follow the binding with the guard that leaves on failure.
        let mut checked = None;
        let value = if matches!(self.peek(), Tok::Newline) {
            Expr::ZeroInit
        } else {
            self.expect(&Tok::Eq, "`=`")?;
            if self.is_check_kw() {
                checked = Some(self.pos);
                self.bump();
            }
            self.expr()?
        };
        self.expect(&Tok::Newline, "newline after `let`")?;
        let kind = match ty {
            Some(ty) => StmtKind::Let {
                name,
                ty,
                value,
                mutable,
            },
            None => StmtKind::LetInfer {
                name,
                value,
                mutable,
            },
        };
        let stmt = self.finish(kind, start);
        if let Some(at) = checked {
            self.pending.push(self.check_guard(at)?);
        }
        Ok(stmt)
    }

    /// Statements until one of `terminators`, which is not consumed.
    fn block(&mut self, terminators: &[Tok]) -> Result<Vec<Stmt>, ParseError> {
        self.block_until(&|p: &Parser| terminators.contains(p.peek()))
    }

    /// The body of a block, up to whatever `stop` says ends it.
    ///
    /// A predicate rather than a list of tokens because a `match` arm ends at
    /// the *word* `when`, and only when that word leads a `when` clause — a
    /// statement that happens to assign to a variable called `when` is still a
    /// statement, and a token list could not tell the two apart.
    fn block_until(&mut self, stop: &dyn Fn(&Parser) -> bool) -> Result<Vec<Stmt>, ParseError> {
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            if stop(self) {
                return Ok(body);
            }
            if matches!(self.peek(), Tok::Eof) {
                return self.err("unexpected end of file (missing `end`)");
            }
            if let Some(st) = self.statement()? {
                body.push(st);
            }
            body.extend(self.pending.drain(..));
        }
    }

    /// One statement of any kind.
    ///
    /// `None` is a statement that compiled to *nothing*: an `assert` in a
    /// release build, which is the whole of what "compiled out" means. Every
    /// other statement is a `Some`.
    fn statement(&mut self) -> Result<Option<Stmt>, ParseError> {
        Ok(Some(match self.peek() {
            Tok::Let => self.stmt_let(false)?,
            Tok::Var => self.stmt_let(true)?,
            Tok::Call => self.stmt_call()?,
            Tok::If => self.stmt_if()?,
            Tok::While => self.stmt_while()?,
            Tok::For => self.stmt_for()?,
            Tok::Break => self.stmt_jump(true)?,
            Tok::Continue => self.stmt_jump(false)?,
            Tok::Return => self.stmt_return()?,
            // Either `name = expr`, `component.property = expr`, or one of the
            // statement words that are soft keywords; `stmt_ident` sorts them.
            Tok::Ident(_) => return self.stmt_ident(),
            Tok::Eof => return self.err("unexpected end of file (missing `end`)"),
            other => return self.err(format!("expected a statement or `end`, found {other:?}")),
        }))
    }

    /// `if COND NEWLINE ... (else if COND NEWLINE ...)* (else NEWLINE ...)? end`
    fn stmt_if(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.expect(&Tok::If, "`if`")?;
        if self.leads_some() {
            return self.stmt_if_some(start);
        }
        let mut arms = Vec::new();
        let mut otherwise = None;
        loop {
            let cond = self.expr()?;
            // `if c then a else b` is the *value* form, and a statement is not
            // where a value goes. Naming that beats "expected a newline".
            if self.word() == Some("then") {
                return self.err(
                    "`if COND then A else B` is the value form of `if` — write it where a value \
                     goes, as in `let n: int = if c then 1 else 2`; a block `if` has its body on \
                     the following lines",
                );
            }
            self.expect(&Tok::Newline, "newline after the condition")?;
            let body = self.block(&[Tok::Else, Tok::End])?;
            arms.push((cond, body));

            match self.peek() {
                Tok::Else => {
                    self.bump();
                    if matches!(self.peek(), Tok::If) {
                        self.bump();
                        continue; // `else if`
                    }
                    self.expect(&Tok::Newline, "newline after `else`")?;
                    otherwise = Some(self.block(&[Tok::End])?);
                    self.expect(&Tok::End, "`end`")?;
                    break;
                }
                Tok::End => {
                    self.bump();
                    break;
                }
                other => return self.err(format!("expected `else` or `end`, found {other:?}")),
            }
        }
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(StmtKind::If { arms, otherwise }, start))
    }

    /// `if some EXPR as NAME NEWLINE ... (else NEWLINE ...)? end` — the body
    /// runs with the optional's value bound to `NAME`, and only when there is
    /// one. See [`StmtKind::IfSome`]; the expansion is the desugar's, because
    /// only it knows what type `NAME` is.
    fn stmt_if_some(&mut self, start: usize) -> Result<Stmt, ParseError> {
        self.bump(); // `some`
        let value = self.expr()?;
        if self.word() != Some("as") {
            return self.err(format!(
                "expected `as` and a name for the value — `if some v as value`, found {:?}",
                self.peek()
            ));
        }
        self.bump(); // `as`
        let bind = self.ident("the name to bind the value to")?;
        self.expect(&Tok::Newline, "newline after `if some ... as ...`")?;
        let body = self.block(&[Tok::Else, Tok::End])?;
        let otherwise = if matches!(self.peek(), Tok::Else) {
            self.bump();
            // `else if` would start a second condition that cannot see the
            // binding, and reads as though it could.
            if matches!(self.peek(), Tok::If) {
                return self.err(
                    "`else if` cannot follow `if some ... as ...`: the name bound above is not in \
                     scope in a second condition. Close this `if` and open another",
                );
            }
            self.expect(&Tok::Newline, "newline after `else`")?;
            Some(self.block(&[Tok::End])?)
        } else {
            None
        };
        self.expect(&Tok::End, "`end`")?;
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(
            StmtKind::IfSome {
                value,
                bind,
                body,
                otherwise,
            },
            start,
        ))
    }

    /// `while COND NEWLINE ... end`
    fn stmt_while(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.expect(&Tok::While, "`while`")?;
        let cond = self.expr()?;
        self.expect(&Tok::Newline, "newline after the condition")?;
        let body = self.block(&[Tok::End])?;
        self.expect(&Tok::End, "`end`")?;
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(StmtKind::While { cond, body }, start))
    }

    /// ```text
    /// for := "for" "each" foreach
    ///      | "for" IDENT "in" expr ".." expr ("step" INT)? NEWLINE stmt* "end"
    ///      | "for" IDENT "=" expr "to" expr ("step" INT)? NEWLINE stmt* "end"
    /// ```
    ///
    /// Three loops share the `for` keyword. `each`, `in`, `to` and `step` are
    /// **soft** keywords, matched as identifiers in these positions only —
    /// reserving them would steal ordinary words from every variable and
    /// property name in the language. The first token after `for` chooses:
    /// `each` (before a binding name) is the for-each form, an `in` after the
    /// loop variable is the range form, and an `=` is the original counting
    /// loop. The two new forms are sugar — each lands on a counted loop the
    /// language already has.
    fn stmt_for(&mut self) -> Result<Stmt, ParseError> {
        let head = self.pos;
        self.expect(&Tok::For, "`for`")?;

        // `for each ELEM ... in COLL` — iterate a collection. `each` is the
        // for-each form only when a binding name follows it, so a counting loop
        // whose variable is literally `each` (`for each = 1 to 3`) is unchanged.
        if matches!(self.peek(), Tok::Ident(w) if w == "each")
            && matches!(self.peek_at(1), Tok::Ident(_))
        {
            return self.stmt_for_each(head);
        }

        let var = self.ident("loop variable name")?;

        // `for i in A..B` — a range loop. `in` sits where the counting loop's
        // `=` would, straight after the loop variable, so the two never collide;
        // it is the same soft keyword the membership test spells.
        if matches!(self.peek(), Tok::Ident(w) if w == "in") {
            return self.stmt_for_range(var, head);
        }

        self.expect(&Tok::Eq, "`=` after the loop variable")?;
        let start = self.expr()?;
        if !matches!(self.peek(), Tok::Ident(w) if w == "to") {
            return self.err(format!(
                "expected `to` after the start value, found {:?}",
                self.peek()
            ));
        }
        self.bump();
        let limit = self.expr()?;
        let step = self.parse_step()?;
        self.expect(&Tok::Newline, "newline after the loop header")?;
        let body = self.block(&[Tok::End])?;
        self.expect(&Tok::End, "`end`")?;
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(
            StmtKind::For {
                var,
                start,
                limit,
                step,
                body,
            },
            head,
        ))
    }

    /// The optional `step K` closing a loop header, shared by the counting `for`
    /// and the range loop so both read the one literal the one way. `K` is a
    /// whole-number literal (it may be negative) so the loop's direction — and
    /// thus whether it counts while `i <= limit` or `i >= limit` — is known
    /// without a run-time test; `step 0` never advances and is refused.
    fn parse_step(&mut self) -> Result<i64, ParseError> {
        if !matches!(self.peek(), Tok::Ident(w) if w == "step") {
            return Ok(1);
        }
        self.bump();
        let line = self.line();
        match self.expr()? {
            Expr::IntLit(0) => Err(ParseError {
                line,
                msg: "`step 0` never advances the loop variable".into(),
            }),
            Expr::IntLit(v) => Ok(v),
            _ => Err(ParseError {
                line,
                msg: "`step` needs a whole-number literal, such as `step 2` or `step -1`".into(),
            }),
        }
    }

    /// `for i in A..B [step S]` — sugar for the counting `for i = A to B
    /// [step S]`, and desugared to exactly that: `StmtKind::For`. Both bounds
    /// are inclusive and evaluated once, `break`/`continue` behave, and the
    /// step sign is a compile-time fact — every one of those is the counting
    /// loop's own behaviour, because this *is* the counting loop under a second
    /// spelling. The bounds count with `int`, the type that loop counts with.
    /// `..` has already fused in the lexer, so the lower bound's expression ends
    /// cleanly at it rather than reading the first dot as a field access.
    fn stmt_for_range(&mut self, var: String, head: usize) -> Result<Stmt, ParseError> {
        self.bump(); // `in`
        let start = self.expr()?;
        self.expect(&Tok::DotDot, "`..` between the range bounds, as in `1..10`")?;
        let limit = self.expr()?;
        let step = self.parse_step()?;
        self.expect(&Tok::Newline, "newline after the loop header")?;
        let body = self.block(&[Tok::End])?;
        self.expect(&Tok::End, "`end`")?;
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(
            StmtKind::For {
                var,
                start,
                limit,
                step,
                body,
            },
            head,
        ))
    }

    /// `for each ELEM [, VALUE] [at IDX] in COLL NEWLINE ... end`, with `each`
    /// already peeked. The element binding is required; a `, VALUE` second name
    /// is the dictionary two-binding form (the key is `ELEM`, the value is
    /// `VALUE`); an `at IDX` binds the 1-based position. `at` and `in` are soft
    /// keywords in these slots only. What the collection is — and therefore what
    /// each binding's type is — is the checker's to work out; the parser only
    /// records the names and the collection expression.
    fn stmt_for_each(&mut self, head: usize) -> Result<Stmt, ParseError> {
        self.bump(); // `each`
        let elem = self.ident("the element binding name after `for each`")?;
        let value = if matches!(self.peek(), Tok::Comma) {
            self.bump();
            Some(self.ident("the value binding name after `,`")?)
        } else {
            None
        };
        let index = if matches!(self.peek(), Tok::Ident(w) if w == "at") {
            self.bump();
            Some(self.ident("the index binding name after `at`")?)
        } else {
            None
        };
        match self.peek() {
            Tok::Ident(w) if w == "in" => {
                self.bump();
            }
            other => {
                return self.err(format!(
                    "expected `in` before the collection in `for each`, found {other:?}"
                ))
            }
        }
        let coll = self.expr()?;
        self.expect(&Tok::Newline, "newline after the `for each` header")?;
        let body = self.block(&[Tok::End])?;
        self.expect(&Tok::End, "`end`")?;
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(
            StmtKind::ForEach {
                elem,
                value,
                index,
                coll,
                body,
            },
            head,
        ))
    }

    /// `break` / `continue`.
    fn stmt_jump(&mut self, is_break: bool) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.bump();
        if matches!(self.peek(), Tok::Newline) {
            self.bump();
        }
        Ok(self.finish(
            if is_break {
                StmtKind::Break
            } else {
                StmtKind::Continue
            },
            start,
        ))
    }

    /// `return` or `return EXPR`. A bare `return` is the one that leaves a sub
    /// with no return type early; anything else on the line is the value.
    fn stmt_return(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.expect(&Tok::Return, "`return`")?;
        // No value when the statement ends right here — a newline, EOF, or the
        // start of a one-line `if` suffix (`return if done`), where the `if` is
        // the suffix and not the returned expression.
        //
        // `return if c then a else b` is the one place the guard suffix and the
        // conditional *value* start with the same token, and `then` on the line
        // is what tells them apart: the guard form has no `then`, and the value
        // form cannot be written without one.
        let bare_if = matches!(self.peek(), Tok::If) && !self.line_has_then();
        let value = if matches!(self.peek(), Tok::Newline | Tok::Eof) || bare_if {
            None
        } else {
            Some(self.expr()?)
        };
        self.finish_simple(StmtKind::Return { value }, start)
    }

    fn stmt_call(&mut self) -> Result<Stmt, ParseError> {
        let start = self.pos;
        self.expect(&Tok::Call, "`call`")?;
        // `call through <ptr>(args...)` — an indirect call in statement
        // position, its result (if the site declares one) discarded, exactly as
        // `call add(1, 2)` discards a subroutine's.
        if matches!(self.peek(), Tok::Ident(w) if w == "through") {
            let (callee, args, ret, conv) = self.call_through_tail()?;
            return self.finish_simple(
                StmtKind::CallThrough { callee, args, ret, conv },
                start,
            );
        }
        let cmd = self.ident("command name")?;
        // `call xs.sort()` — the method spelling reaches statement position
        // too, so a command called for its effect is written the same way it is
        // written for its value. The receiver is a whole postfix expression
        // (`call people[1].greet()`), and the `(` is the same rule it is in an
        // expression: without one this was never a statement.
        if matches!(self.peek(), Tok::Dot | Tok::LBracket) {
            let recv = self.postfix(Expr::Var(cmd))?;
            let Expr::Call { cmd, args } = recv else {
                return self.err(
                    "`call` runs a command or a subroutine — write `call name(...)`, or the method \
                     spelling `call value.name(...)`",
                );
            };
            return self.finish_simple(StmtKind::Call { cmd, args }, start);
        }
        self.expect(&Tok::LParen, "`(`")?;
        let args = self.arg_list()?;
        self.finish_simple(StmtKind::Call { cmd, args }, start)
    }

    /// The whole of `through <callee>(args...)[: type][convention]`, with the
    /// `call` already consumed — shared by the statement and the expression, so
    /// the two forms cannot drift into different grammars.
    ///
    /// `through` is a *soft* keyword: it is recognised only in this one slot,
    /// straight after `call`, so a variable, a field or a parameter named
    /// `through` is untouched everywhere else.
    fn call_through_tail(
        &mut self,
    ) -> Result<(Expr, Vec<Expr>, Option<Ty>, Option<CallConv>), ParseError> {
        self.bump(); // `through`
        let callee = self.call_through_callee()?;
        self.expect(&Tok::LParen, "`(` starting the arguments of `call through`")?;
        let args = self.arg_list()?;
        // `: type` — the C return. Absent is a `void` call, the same reading a
        // `dll` line with no `: type` has.
        let ret = if matches!(self.peek(), Tok::Colon) {
            self.bump();
            Some(self.ffi_type("`call through`", false)?)
        } else {
            None
        };
        let conv = self.call_conv_opt("`call through`")?;
        // A second `(` here means the callee was written as a call —
        // `call through dlsym(h, "add")(1, 2)` — and the first parenthesis was
        // read as the argument list, because that is what a parenthesis after
        // the callee means. Say so, rather than leave it to trip the newline.
        if matches!(self.peek(), Tok::LParen) {
            return self.err(
                "`call through` read `(...)` as its arguments, and there is a second `(...)` \
                 after it — a callee that is itself a call goes in parentheses, as in \
                 `call through (GetProcAddress(h, \"add\"))(1, 2)`"
                    .to_string(),
            );
        }
        Ok((callee, args, ret, conv))
    }

    /// The address half of a `call through`: `(any expression)`, or a name and
    /// the `.field` / `[i]` path from it.
    ///
    /// A bare name may NOT be followed by `(` here, because that `(` opens the
    /// argument list — which is why the callee is parsed by this narrow rule
    /// rather than by `expr()`. Anything more involved is written in
    /// parentheses: `call through (ptr_read_ptr(vtable, 24))(object)` is how a
    /// COM method, read out of the object's own table, is reached.
    fn call_through_callee(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Tok::LParen) {
            self.bump();
            let e = self.expr()?;
            self.expect(&Tok::RParen, "`)` closing the callee of `call through`")?;
            return Ok(e);
        }
        let name = self.ident("a `ptr` value, or `(expression)`, after `call through`")?;
        // The first `.` is a `GetProperty` and later ones are `Field`, exactly
        // as `primary` builds them — so `r.fn_ptr` types by the same rule that
        // reads it anywhere else.
        let base = if matches!(self.peek(), Tok::Dot) {
            self.bump();
            let property = self.ident("field name")?;
            Expr::GetProperty { component: name, property }
        } else {
            Expr::Var(name)
        };
        self.postfix_with(base, false)
    }

    /// Parse a comma-separated argument list, assuming the opening `(` has been
    /// consumed; consumes the closing `)`.
    fn arg_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if !matches!(self.peek(), Tok::RParen) {
            loop {
                args.push(self.argument()?);
                match self.peek() {
                    Tok::Comma => {
                        self.bump();
                        // A single trailing comma is allowed: `f(a, b,)`.
                        if matches!(self.peek(), Tok::RParen) {
                            break;
                        }
                    }
                    Tok::RParen => break,
                    other => return self.err(format!("expected `,` or `)`, found {other:?}")),
                }
            }
        }
        self.expect(&Tok::RParen, "`)`")?;
        Ok(args)
    }

    /// Whether what follows the `{` at `self.pos + at` is a record body: the
    /// spread, or a named field. Newlines are stepped over, because a wide
    /// record is written one field to a line and the `{` is then the end of it.
    fn opens_record_body(&self, at: usize) -> bool {
        let mut i = at;
        while matches!(self.peek_at(i), Tok::Newline) {
            i += 1;
        }
        matches!(self.peek_at(i), Tok::DotDot)
            || (matches!(self.peek_at(i), Tok::Ident(_))
                && matches!(self.peek_at(i + 1), Tok::Colon))
    }

    /// `NAME{field: v, ...}` and `NAME{...base, field: v}`, from the `{`.
    ///
    /// The brace form is the paren form's twin — same node, same rules — and
    /// exists because `point{x: 1}` reads as a *value* where `point(x: 1)`
    /// reads as a call. The spread is the one thing only this form has: it
    /// carries the fields the author did not mention over from `base`.
    fn record_braces(&mut self, name: String) -> Result<Expr, ParseError> {
        self.expect(&Tok::LBrace, "`{`")?;
        // A record literal may run over lines: a wide record is easier to read
        // one field to a line, and there is no statement inside the braces for
        // a newline to end.
        self.skip_newlines();
        let mut base: Option<Expr> = None;
        if matches!(self.peek(), Tok::DotDot) {
            self.bump();
            // `...p` and `..p` are one spelling: the lexer fuses the first two
            // dots and leaves the third, so the third is consumed here.
            if matches!(self.peek(), Tok::Dot) {
                self.bump();
            }
            base = Some(self.expr()?);
            if matches!(self.peek(), Tok::Comma) {
                self.bump();
            }
            self.skip_newlines();
        }
        let mut fields: Vec<(String, Expr)> = Vec::new();
        while !matches!(self.peek(), Tok::RBrace) {
            let field = self.ident("field name")?;
            self.expect(&Tok::Colon, "`:` after a field name")?;
            let value = self.expr()?;
            if fields.iter().any(|(n, _)| *n == field) {
                return self.err(format!("`{name}` gives field `{field}` twice"));
            }
            fields.push((field, value));
            match self.peek() {
                Tok::Comma => {
                    self.bump();
                    self.skip_newlines();
                }
                Tok::Newline => self.skip_newlines(),
                Tok::RBrace => break,
                other => {
                    return self.err(format!(
                        "expected `,` or `}}` in `{name}`, found {other:?}"
                    ))
                }
            }
        }
        self.expect(&Tok::RBrace, "`}` after the fields")?;
        Ok(match base {
            Some(base) => Expr::RecordUpdate {
                name,
                base: Box::new(base),
                fields,
            },
            None => Expr::RecordLit { name, fields },
        })
    }

    /// One argument: an expression, or `name: expression` — the named form,
    /// which the desugar matches to the parameter of that name.
    ///
    /// `IDENT :` is the whole of the lookahead, and it can mean nothing else
    /// here: a bare name followed by a colon is not an expression in any other
    /// reading.
    fn argument(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Colon) {
            let name = self.ident("argument name")?;
            self.bump(); // `:`
            let value = self.expr()?;
            return Ok(Expr::Labeled {
                name,
                value: Box::new(value),
            });
        }
        self.expr()
    }

    /// Whether `then` appears before this line ends.
    ///
    /// The single question that separates `return if done` (a guarded bare
    /// return) from `return if c then a else b` (a conditional value). A line
    /// is short and this runs once per `return if`, so a scan is cheaper than
    /// threading a mode through the expression parser.
    fn line_has_then(&self) -> bool {
        self.toks[self.pos..]
            .iter()
            .take_while(|t| !matches!(t.tok, Tok::Newline | Tok::Eof))
            .any(|t| matches!(&t.tok, Tok::Ident(w) if w == "then"))
    }

    /// The word at the cursor, when it is a plain identifier.
    ///
    /// Every soft keyword this parser reads goes through here or `infix_word`,
    /// so a name that happens to be spelled like one keeps working wherever the
    /// position does not claim it.
    fn word(&self) -> Option<&str> {
        match self.peek() {
            Tok::Ident(w) => Some(w.as_str()),
            _ => None,
        }
    }

    /// An expression, including the two lowest-precedence forms: the
    /// conditional `if C then A else B` and the fallback `E otherwise F`.
    ///
    /// Both sit below `or` because both take a whole expression on each side —
    /// `if a and b then x else y` and `read() otherwise a + b` read the way they
    /// look, with no parentheses.
    fn expr(&mut self) -> Result<Expr, ParseError> {
        // `if` in expression position is the conditional value. A *statement*
        // that starts with `if` never reaches here — `block` and `sub` route it
        // to `stmt_if` — so the block form and this one cannot be confused.
        if matches!(self.peek(), Tok::If) {
            return self.if_expr();
        }
        let lhs = self.or_expr()?;
        if self.word() == Some("otherwise") {
            self.bump();
            let fallback = self.or_expr()?;
            // One `otherwise` per expression. A second would read an error slot
            // the first fallback never cleared, so it would take the last arm
            // every time the first call failed — a wrong answer rather than an
            // error.
            if self.word() == Some("otherwise") {
                return self.err(
                    "an expression can carry one `otherwise`; the failure a second one would test \
                     is the same one the first already handled",
                );
            }
            return Ok(Expr::Otherwise {
                value: Box::new(lhs),
                fallback: Box::new(fallback),
            });
        }
        Ok(lhs)
    }

    /// `if COND then A else B` as a value. The `if` is already at the cursor.
    ///
    /// `then` is a soft keyword — it is read as one only here, between a
    /// condition and its first arm, where an identifier could never have
    /// followed a complete expression. `else` is the keyword it already was.
    /// Both arms are full expressions, so `else if ... then ... else ...`
    /// chains right the way an `else if` block does.
    fn if_expr(&mut self) -> Result<Expr, ParseError> {
        self.bump(); // `if`
        let cond = self.or_expr()?;
        if self.word() != Some("then") {
            return self.err(format!(
                "expected `then` after the condition of an `if` used as a value, found {:?}",
                self.peek()
            ));
        }
        self.bump(); // `then`
        let then = self.expr()?;
        // Not optional: a conditional *value* must produce one in both cases,
        // and there is no other value to fall back on.
        if !matches!(self.peek(), Tok::Else) {
            return self.err(
                "an `if` used as a value needs an `else` — every path must produce a value",
            );
        }
        self.bump(); // `else`
        let els = self.expr()?;
        Ok(Expr::IfElse {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        })
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.and_expr()?;
        while matches!(self.peek(), Tok::Or) {
            self.bump();
            let rhs = self.and_expr()?;
            lhs = Expr::Logical(LogicalOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.not_expr()?;
        while matches!(self.peek(), Tok::And) {
            self.bump();
            let rhs = self.not_expr()?;
            lhs = Expr::Logical(LogicalOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Tok::Not) {
            self.bump();
            return Ok(Expr::Not(Box::new(self.not_expr()?)));
        }
        self.cmp_expr()
    }

    /// A comparison, a chained comparison, or a membership test — the three
    /// things that turn operands into a truth value at this precedence.
    ///
    /// `a < b` is a plain `Cmp`. `1 <= x <= 12` — two comparisons sharing the
    /// middle — is a `Chain`, the mathematical reading (`1 <= x and x <= 12`,
    /// with `x` evaluated once); a *third* comparison has no such reading and is
    /// still refused. `e in xs` and `e not in xs` are membership tests.
    fn cmp_expr(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.bor_expr()?;
        // `e in xs` / `k in d` / `sub in text`, and each with `not in`. `in` is a
        // soft keyword, read as an operator only here where an operand has just
        // been parsed; a variable or field named `in` is untouched elsewhere.
        if let Some(negated) = self.membership_op() {
            let haystack = self.bor_expr()?;
            return Ok(Expr::In {
                needle: Box::new(lhs),
                haystack: Box::new(haystack),
                negated,
            });
        }
        let op = match self.cmp_op() {
            Some(op) => op,
            None => return Ok(lhs),
        };
        let mid = self.bor_expr()?;
        // A second comparison sharing this middle operand is a chain. Beyond
        // that there is no unambiguous reading, so a third one stays an error.
        if let Some(op2) = self.cmp_op() {
            let hi = self.bor_expr()?;
            if self.cmp_op_peek().is_some() {
                return self.err(
                    "only two comparisons can be chained (as in `1 <= x <= 12`); \
                     write the rest with `and`",
                );
            }
            return Ok(Expr::Chain {
                lo: Box::new(lhs),
                lo_op: op,
                mid: Box::new(mid),
                hi_op: op2,
                hi: Box::new(hi),
            });
        }
        Ok(Expr::Cmp(op, Box::new(lhs), Box::new(mid)))
    }

    /// The comparison operator at the cursor, without consuming it.
    fn cmp_op_peek(&self) -> Option<CmpOp> {
        match self.peek() {
            Tok::Eq => Some(CmpOp::Eq),
            Tok::Ne => Some(CmpOp::Ne),
            Tok::Lt => Some(CmpOp::Lt),
            Tok::Le => Some(CmpOp::Le),
            Tok::Gt => Some(CmpOp::Gt),
            Tok::Ge => Some(CmpOp::Ge),
            _ => None,
        }
    }

    /// The comparison operator at the cursor, consuming it if present.
    fn cmp_op(&mut self) -> Option<CmpOp> {
        let op = self.cmp_op_peek()?;
        self.bump();
        Some(op)
    }

    /// `in` / `not in` in operator position: `Some(false)` for `in`, `Some(true)`
    /// for `not in`, `None` for neither. A soft keyword, like the bitwise words.
    fn membership_op(&mut self) -> Option<bool> {
        if matches!(self.peek(), Tok::Ident(w) if w == "in") {
            self.bump();
            return Some(false);
        }
        if matches!(self.peek(), Tok::Not)
            && matches!(self.peek_at(1), Tok::Ident(w) if w == "in")
        {
            self.bump(); // `not`
            self.bump(); // `in`
            return Some(true);
        }
        None
    }

    /// The word an infix operator is spelled with, when the next token is one.
    ///
    /// `band`, `shl` and their siblings are **soft** keywords: they are read as
    /// operators only here, where a complete operand has just been parsed and
    /// an identifier could never have begun anything. A variable, a parameter
    /// or a field named `band` therefore keeps working everywhere else, and no
    /// program that compiled before this existed stops compiling.
    fn infix_word(&self) -> Option<&str> {
        match self.peek() {
            Tok::Ident(w) => Some(w.as_str()),
            _ => None,
        }
    }

    /// `bor` — the loosest bitwise level, and looser than a comparison, so
    /// `flags band WS_VISIBLE <> 0` reads the way it looks. (C binds these
    /// tighter than `==`, which is the reason C programmers parenthesise every
    /// flag test; Kiln does not need them to.)
    fn bor_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.bxor_expr()?;
        while self.infix_word() == Some("bor") {
            self.bump();
            let rhs = self.bxor_expr()?;
            lhs = Expr::Bit(BitOp::Or, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn bxor_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.band_expr()?;
        while self.infix_word() == Some("bxor") {
            self.bump();
            let rhs = self.band_expr()?;
            lhs = Expr::Bit(BitOp::Xor, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn band_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.shift_expr()?;
        while self.infix_word() == Some("band") {
            self.bump();
            let rhs = self.shift_expr()?;
            lhs = Expr::Bit(BitOp::And, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// Shifts bind tighter than the bitwise levels and looser than `+`, so
    /// `1 shl n band mask` is `(1 shl n) band mask` and `x shl a + b` is
    /// `x shl (a + b)` — the second is C's ordering, and the reading a count
    /// computed with `+` wants.
    fn shift_expr(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.sum()?;
        loop {
            let op = match self.infix_word() {
                Some("shl") => BitOp::Shl,
                Some("shr") => BitOp::Shr,
                Some("ushr") => BitOp::Ushr,
                _ => break,
            };
            self.bump();
            let rhs = self.sum()?;
            lhs = Expr::Bit(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn sum(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.term()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn term(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.factor()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => break,
            };
            self.bump();
            let rhs = self.factor()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// A factor, optionally negated. Unary `-` binds tighter than `*`, so
    /// `-a * b` is `(-a) * b`, and a negated literal is folded into the literal
    /// itself — see `Expr::Neg`.
    fn factor(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Tok::Minus) {
            self.bump();
            return Ok(match self.factor()? {
                Expr::IntLit(v) => match v.checked_neg() {
                    Some(n) => Expr::IntLit(n),
                    None => return self.err("integer literal out of range"),
                },
                // `-0xFF` is the number -255: a leading `-` says the pattern was
                // meant as a magnitude, so it collapses to a plain integer
                // literal here and stops being width-dependent.
                Expr::BitsLit(v) => match bits_value(v).checked_neg() {
                    Some(n) => Expr::IntLit(n),
                    None => return self.err("integer literal out of range"),
                },
                Expr::DoubleLit(v) => Expr::DoubleLit(-v),
                other => Expr::Neg(Box::new(other)),
            });
        }
        // `bnot EXPR` — every bit flipped. Unlike the infix words this is a
        // reserved keyword; see `Tok::BNot` for why a prefix operator cannot
        // be a soft one.
        if matches!(self.peek(), Tok::BNot) {
            self.bump();
            return Ok(Expr::BitNot(Box::new(self.factor()?)));
        }
        let primary = self.primary()?;
        self.postfix(primary)
    }

    /// `[` … `]` and `.field` after a value, repeatedly — both bind tighter
    /// than any operator, so `xs[0] + 1` adds to the element and not to the
    /// array, and `people[1].age * 2` doubles the age.
    ///
    /// The first `.` of a chain never arrives here: `p.x` is read in `primary`,
    /// where it is indistinguishable from a component property read and is left
    /// as one for the checker to resolve.
    fn postfix(&mut self, e: Expr) -> Result<Expr, ParseError> {
        self.postfix_with(e, true)
    }

    /// As `postfix`, told whether `.name(` may be read as a method call.
    ///
    /// It may not in the callee of a `call through`: there the parentheses
    /// belong to the indirect call itself, so `call through r.vtbl.release()`
    /// must read `r.vtbl.release` as the pointer and leave the `(` alone.
    /// Everywhere else a value is being built and `.name(` is the method
    /// spelling.
    fn postfix_with(&mut self, mut e: Expr, methods: bool) -> Result<Expr, ParseError> {
        loop {
            match self.peek() {
                Tok::LBracket => {
                    self.bump();
                    // `xs[a..b]` is a slice and `xs[a]` is one element; the two
                    // are told apart by the `..`, which the index expression
                    // stops at of its own accord (`..` is no operator). Either
                    // bound may be left out — `xs[..b]`, `xs[a..]`, `xs[..]` —
                    // and means the collection's own end.
                    let from = if matches!(self.peek(), Tok::DotDot) {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    if matches!(self.peek(), Tok::DotDot) {
                        self.bump();
                        let to = if matches!(self.peek(), Tok::RBracket) {
                            None
                        } else {
                            Some(self.expr()?)
                        };
                        self.expect(&Tok::RBracket, "`]` after the slice")?;
                        e = Expr::Slice {
                            base: Box::new(e),
                            from: from.map(Box::new),
                            to: to.map(Box::new),
                        };
                        continue;
                    }
                    // Not a range, so `from` is the index: a `..` in front of it
                    // was the only way it could have been left out.
                    let index = from.expect("an index is the expression just parsed");
                    self.expect(&Tok::RBracket, "`]` after the index")?;
                    e = Expr::Index {
                        base: Box::new(e),
                        index: Box::new(index),
                    };
                }
                Tok::Dot => {
                    self.bump();
                    let name = self.ident("field name")?;
                    // `x.f(a)` is the method spelling of `f(x, a)` — the value
                    // on the left becomes the first argument. The `(` is the
                    // whole rule: `.name` with no parentheses stays a field or
                    // property read, so `label1.text` is untouched.
                    if methods && matches!(self.peek(), Tok::LParen) {
                        self.bump();
                        let mut args = self.arg_list()?;
                        args.insert(0, e);
                        e = Expr::Call { cmd: name, args };
                        continue;
                    }
                    e = Expr::Field {
                        base: Box::new(e),
                        name,
                    };
                }
                _ => return Ok(e),
            }
        }
    }

    /// The tail of `[EXPR for each ELEM [, VALUE] [at IDX] in COLL [where COND]]`,
    /// with `EXPR` already parsed and `for` next.
    ///
    /// The header is the statement `for each`'s, word for word, so there is one
    /// thing to learn and not two; `where` is a soft keyword, claimed only here,
    /// between the collection and the closing `]`.
    fn comprehension(&mut self, body: Expr) -> Result<Expr, ParseError> {
        self.expect(&Tok::For, "`for`")?;
        if self.word() != Some("each") {
            return self.err(format!(
                "expected `each` after `for` in a list — `[e for each x in xs]`, found {:?}",
                self.peek()
            ));
        }
        self.bump(); // `each`
        let elem = self.ident("the element binding name after `for each`")?;
        let value = if matches!(self.peek(), Tok::Comma) {
            self.bump();
            Some(self.ident("the value binding name after `,`")?)
        } else {
            None
        };
        let index = if self.word() == Some("at") {
            self.bump();
            Some(self.ident("the index binding name after `at`")?)
        } else {
            None
        };
        if self.word() != Some("in") {
            return self.err(format!(
                "expected `in` before the collection, found {:?}",
                self.peek()
            ));
        }
        self.bump(); // `in`
        let coll = self.expr()?;
        let cond = if self.word() == Some("where") {
            self.bump();
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        self.expect(&Tok::RBracket, "`]` closing the list")?;
        Ok(Expr::Comprehension {
            body: Box::new(body),
            elem,
            value,
            index,
            coll: Box::new(coll),
            cond,
            holds: None,
        })
    }

    /// Whether an `if` opens the optional-unwrapping form: the word `some`
    /// followed by a name. A variable called `some` is untouched — `if some and
    /// x`, `if some = 1`, `if some in xs` all still read it as the value it is,
    /// because in each of those the token after the word is an operator and not
    /// the start of an expression.
    fn leads_some(&self) -> bool {
        self.word() == Some("some")
            && matches!(self.peek_at(1), Tok::Ident(w) if !Self::INFIX_WORDS.contains(&w.as_str()))
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        // `[a, b, c]` — the elements are a plain argument list, so a literal
        // may hold any expression, not only constants.
        if matches!(self.peek(), Tok::LBracket) {
            self.bump();
            let mut items = Vec::new();
            if !matches!(self.peek(), Tok::RBracket) {
                loop {
                    items.push(self.expr()?);
                    // `[EXPR for each x in xs]` — the list a loop would have
                    // built. It is only ever the *first* element that a `for`
                    // can follow, so a plain literal is undisturbed.
                    if items.len() == 1 && matches!(self.peek(), Tok::For) {
                        return self.comprehension(items.pop().unwrap());
                    }
                    match self.peek() {
                        Tok::Comma => {
                            self.bump();
                            // A single trailing comma is allowed: `[a, b,]`.
                            if matches!(self.peek(), Tok::RBracket) {
                                break;
                            }
                        }
                        Tok::RBracket => break,
                        other => {
                            return self
                                .err(format!("expected `,` or `]` in a list, found {other:?}"))
                        }
                    }
                }
            }
            self.expect(&Tok::RBracket, "`]` closing the list")?;
            return Ok(Expr::ArrayLit(items));
        }
        // `{"a": 1, "b": 2}` — a dictionary. The key is an expression, not a
        // bare word, because a key is data: the common case is one a program
        // computed, not one it typed out.
        if matches!(self.peek(), Tok::LBrace) {
            self.bump();
            let mut pairs = Vec::new();
            self.skip_newlines();
            if !matches!(self.peek(), Tok::RBrace) {
                loop {
                    self.skip_newlines();
                    let key = self.expr()?;
                    self.expect(&Tok::Colon, "`:` between a key and its value")?;
                    let value = self.expr()?;
                    pairs.push((key, value));
                    self.skip_newlines();
                    match self.peek() {
                        Tok::Comma => {
                            self.bump();
                            // A single trailing comma is allowed: `{"a": 1,}`.
                            self.skip_newlines();
                            if matches!(self.peek(), Tok::RBrace) {
                                break;
                            }
                        }
                        Tok::RBrace => break,
                        other => {
                            return self.err(format!(
                                "expected `,` or `}}` in a dictionary, found {other:?}"
                            ))
                        }
                    }
                }
            }
            self.expect(&Tok::RBrace, "`}` closing the dictionary")?;
            return Ok(Expr::DictLit(pairs));
        }
        // `call through <ptr>(args...): T` used for its value. `call` is a
        // keyword, so it began no expression before this and nothing can be
        // shadowed by allowing it here.
        if matches!(self.peek(), Tok::Call) {
            self.bump();
            if !matches!(self.peek(), Tok::Ident(w) if w == "through") {
                return self.err(
                    "`call` inside an expression is only `call through <ptr>(...)`; a command \
                     or subroutine is called by name, without `call`, when its result is wanted"
                        .to_string(),
                );
            }
            let (callee, args, ret, conv) = self.call_through_tail()?;
            return Ok(Expr::CallThrough {
                callee: Box::new(callee),
                args,
                ret,
                conv,
            });
        }
        // The line of the primary token, captured before it is consumed, so an
        // interpolation error points at the literal's own line.
        let tok_line = self.line();
        match self.bump() {
            Tok::True => Ok(Expr::BoolLit(true)),
            Tok::False => Ok(Expr::BoolLit(false)),
            Tok::Int(v) => Ok(Expr::IntLit(v)),
            Tok::Bits(v) => Ok(Expr::BitsLit(v)),
            Tok::Float(v) => Ok(Expr::DoubleLit(v)),
            Tok::Str(s) => interpolate(&s, tok_line, &self.enums.clone()),
            // A raw literal is the one text that is not split: `{` is a brace
            // and `\\d` is a backslash and a `d`, which is what makes the form
            // worth having for a regular expression or a path.
            Tok::RawStr(s) => Ok(Expr::TextLit(s)),
            Tok::Ident(name) => {
                // `address of NAME` — the address of a subroutine as a `ptr`.
                // `address` is a *soft* keyword: it means this only when the
                // next token is `of`, so a variable or field named `address`
                // keeps working everywhere else. Two idents in a row was never
                // valid, so `of` steals nothing either. The checker resolves
                // NAME and proves its signature is C-representable.
                if name == "address" && matches!(self.peek(), Tok::Ident(w) if w == "of") {
                    self.bump(); // `of`
                    let mut path =
                        self.ident("the name of a subroutine or c-record after `address of`")?;
                    // `address of r.pt` / `address of r.rgb`: a path into a
                    // c-record's own storage. Carried as the dotted spelling in
                    // one string — a subroutine name has no `.`, so the two
                    // readings can never be confused, and the checker splits it.
                    while matches!(self.peek(), Tok::Dot) {
                        self.bump();
                        let field = self.ident("a field name after `.` in `address of`")?;
                        path.push('.');
                        path.push_str(&field);
                    }
                    return Ok(Expr::AddressOf(path));
                }
                // `size of TYPE` — a compile-time byte count. `size` is a soft
                // keyword the same way `address` is: it means this only before
                // `of`, so a variable named `size` is untouched. The operand is
                // a type (a c-record name or a scalar), read with the same
                // `type_keyword` a declaration uses, so `size of RECT` and
                // `size of int` share one parse.
                if name == "size" && matches!(self.peek(), Tok::Ident(w) if w == "of") {
                    self.bump(); // `of`
                    let ty = self.type_keyword()?;
                    return Ok(Expr::SizeOf(ty));
                }
                // `point(x: 1, y: 2)` — a record. A named first argument is
                // what separates it from a call, and two tokens of lookahead
                // are enough to see one.
                if matches!(self.peek(), Tok::LParen)
                    && matches!(self.peek_at(1), Tok::Ident(_))
                    && matches!(self.peek_at(2), Tok::Colon)
                {
                    self.bump();
                    let args = self.arg_list()?;
                    // Every argument named is a record — the shape this branch
                    // was written for. One of them *not* named means a call
                    // with a named argument in it, and it stays a call so the
                    // desugar can say which parameter each name meant (or, for
                    // a record, that a field with no name is not a field).
                    if args.iter().all(|a| matches!(a, Expr::Labeled { .. })) {
                        let fields = args
                            .into_iter()
                            .map(|a| match a {
                                Expr::Labeled { name, value } => (name, *value),
                                _ => unreachable!(),
                            })
                            .collect();
                        return Ok(Expr::RecordLit { name, fields });
                    }
                    return Ok(Expr::Call { cmd: name, args });
                }
                // `point{x: 1, y: 2}` — the same record in the brace spelling,
                // and `point{...p, y: 9}`, a copy of `p` with `y` replaced. The
                // lookahead (`{` then a labelled field, or `{` then the spread)
                // is what keeps a dictionary literal that happens to follow a
                // name from being read as a record.
                if matches!(self.peek(), Tok::LBrace) && self.opens_record_body(1) {
                    return self.record_braces(name);
                }
                if matches!(self.peek(), Tok::LParen) {
                    // Call-expression: NAME(args...)
                    self.bump();
                    let args = self.arg_list()?;
                    Ok(Expr::Call { cmd: name, args })
                } else if matches!(self.peek(), Tok::Dot) {
                    self.bump();
                    // `colour.red` — a member of an enum, which is a `const`
                    // named `colour.red`. Folded here, while the two names are
                    // still side by side, so every later stage sees the plain
                    // constant reference it already knows how to read; a
                    // misspelt member is named here rather than reported as an
                    // unknown component further downstream.
                    if let Some(members) = self.enums.get(&name).cloned() {
                        let m = self.ident("an enum member name after `.`")?;
                        if !members.contains(&m) {
                            return self.err(format!(
                                "`{m}` is not a member of enum `{name}` — it has {}",
                                members.join(", ")
                            ));
                        }
                        return Ok(Expr::Var(format!("{name}.{m}")));
                    }
                    let property = self.ident("property name")?;
                    // `s.uppercase()` — the method spelling of `uppercase(s)`.
                    // A `(` right after the name is what separates a call from
                    // a read; without one this is the property read it always
                    // was (`ok_button.text`), which is why a component keeps
                    // working beside a method chain.
                    if matches!(self.peek(), Tok::LParen) {
                        self.bump();
                        let mut args = self.arg_list()?;
                        args.insert(0, Expr::Var(name));
                        return Ok(Expr::Call { cmd: property, args });
                    }
                    Ok(Expr::GetProperty {
                        component: name,
                        property,
                    })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(e)
            }
            other => self.err(format!("expected expression, found {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hello() {
        let src = "module m\nsub main\n  let x: int = 6 * 7\n  call print_int(x)\nend\n";
        let m = parse(src).unwrap();
        assert_eq!(m.name, "m");
        assert_eq!(m.subs().count(), 1);
    }

    #[test]
    fn parses_form_with_component_and_binding() {
        let src = "module m\nuse ui\nform win\n  title = \"Hi\"\n  width = 320\n  button ok\n    text = \"Go\"\n    on click: handler\n  end\nend\nsub handler\n  call print_text(\"hi\")\nend\n";
        let m = parse(src).unwrap();
        assert_eq!(m.uses, vec!["ui"]);
        let f = m.forms().next().expect("a form");
        assert_eq!(f.name, "win");
        assert_eq!(f.properties.len(), 2);
        assert_eq!(f.children.len(), 1);
        let c = &f.children[0];
        assert_eq!((c.type_name.as_str(), c.id.as_str()), ("button", "ok"));
        assert_eq!(
            c.handlers,
            vec![("click".to_string(), "handler".to_string())]
        );
        assert!(m.is_gui());
    }

    #[test]
    fn parses_parameters_and_a_return_type() {
        let m = parse("module m\nsub add(a: int, b: text): double\n  return 1.0\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        assert_eq!(
            s.params,
            vec![("a".into(), Ty::Int), ("b".into(), Ty::Text)]
        );
        assert_eq!(s.ret, Some(Ty::Double));
        assert_eq!(s.line, 2);
    }

    /// The old shape must keep parsing untouched: an entry point and an event
    /// handler are subs with no parameters and no return type.
    #[test]
    fn a_bare_sub_is_still_a_sub() {
        for src in [
            "module m\nsub main\n  call print_int(1)\nend\n",
            "module m\nsub main()\n  call print_int(1)\nend\n",
        ] {
            let s = parse(src).unwrap().subs().next().unwrap().clone();
            assert!(s.is_plain(), "{src}");
            assert_eq!(s.signature().params.len(), 0);
        }
    }

    #[test]
    fn parses_return_with_and_without_a_value() {
        let m = parse("module m\nsub main\n  if true\n    return\n  end\n  return\nend\n")
            .unwrap();
        let s = m.subs().next().unwrap();
        assert!(matches!(
            s.body.last().unwrap().kind,
            StmtKind::Return { value: None }
        ));
        // The `return` nested inside the `if` must be reached too — the body
        // loop and the block loop are separate code paths.
        let StmtKind::If { arms, .. } = &s.body[0].kind else {
            panic!("expected an if")
        };
        assert!(matches!(
            arms[0].1[0].kind,
            StmtKind::Return { value: None }
        ));
    }

    #[test]
    fn rejects_a_duplicate_parameter() {
        assert!(parse("module m\nsub f(a: int, a: int)\nend\n").is_err());
    }

    #[test]
    fn parses_address_of_a_sub() {
        let m = parse(
            "module m\nsub h(a: int): int\n  return a\nend\nsub main\n  var p: ptr = address of h\nend\n",
        )
        .unwrap();
        let main = m.subs().find(|s| s.name == "main").unwrap();
        let StmtKind::Let { value, ty, .. } = &main.body[0].kind else {
            panic!("expected a let");
        };
        assert_eq!(*ty, Ty::Ptr);
        assert_eq!(*value, Expr::AddressOf("h".to_string()));
    }

    #[test]
    fn parses_a_calling_convention_on_a_dll() {
        // After `from`, and after an `as`, and each of the three markers.
        let m = parse(
            "module m\n\
             dll a(): int from \"x\" system\n\
             dll b(): int from \"x\" as \"bb\" stdcall\n\
             dll c(): int from \"x\" cdecl\n\
             dll d(): int from \"x\"\n",
        )
        .unwrap();
        let convs: Vec<Option<CallConv>> = m.dlls().map(|d| d.conv).collect();
        assert_eq!(
            convs,
            vec![
                Some(CallConv::System),
                Some(CallConv::Stdcall),
                Some(CallConv::Cdecl),
                None
            ]
        );
    }

    #[test]
    fn parses_a_calling_convention_on_a_sub() {
        // A bare sub, a sub with a return type, and one with neither marker.
        let m = parse(
            "module m\n\
             sub bare system\nend\n\
             sub shaped(a: int): int64 stdcall\n  return 0\nend\n\
             sub plain\nend\n",
        )
        .unwrap();
        assert_eq!(
            m.subs().find(|s| s.name == "bare").unwrap().conv,
            Some(CallConv::System)
        );
        assert_eq!(
            m.subs().find(|s| s.name == "shaped").unwrap().conv,
            Some(CallConv::Stdcall)
        );
        assert_eq!(m.subs().find(|s| s.name == "plain").unwrap().conv, None);
    }

    #[test]
    fn rejects_an_unknown_calling_convention() {
        // On a `dll` and on a `sub`; the message names the offender and the set.
        for src in [
            "module m\ndll f(): int from \"x\" fastcall\n",
            "module m\nsub f(a: int): int pascal\n  return 0\nend\n",
        ] {
            let e = parse(src).expect_err("an unknown convention must be rejected");
            assert!(
                e.msg.contains("stdcall"),
                "the diagnostic should name the allowed set, got: {}",
                e.msg
            );
        }
    }

    #[test]
    fn rejects_a_convention_before_as() {
        // `as` must come first; a trailing `as` names the ordering, not a bad
        // convention.
        let e = parse("module m\ndll f(): int from \"x\" system as \"g\"\n")
            .expect_err("a convention before `as` must be rejected");
        assert!(
            e.msg.contains("before the calling convention"),
            "the diagnostic should point at the `as`/convention order, got: {}",
            e.msg
        );
    }

    /// The three words are only conventions in the trailing position. Anywhere
    /// else — a variable, a parameter name — they stay ordinary identifiers.
    #[test]
    fn a_convention_word_is_a_soft_keyword() {
        let m =
            parse("module m\nsub main\n  var system: int = 1\n  system = system + 1\nend\n").unwrap();
        let main = m.subs().next().unwrap();
        assert!(matches!(main.body[0].kind, StmtKind::Let { .. }));
        assert!(matches!(main.body[1].kind, StmtKind::Assign { .. }));
        assert_eq!(main.conv, None);
    }

    /// `call through <ptr>(args): T` parses in both positions, and the shape it
    /// carries is the callee, the arguments and the site's declared return.
    #[test]
    fn parses_call_through_as_a_value_and_as_a_statement() {
        let m = parse(
            "module m\nsub main\n  var fp: ptr = ptr_null()\n  \
             let n: int = call through fp(10, 20): int\n  call through fp(1)\nend\n",
        )
        .unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { value, .. } = &s.body[1].kind else {
            panic!("expected a let")
        };
        let Expr::CallThrough { callee, args, ret, conv } = value else {
            panic!("expected a call through, got {value:?}")
        };
        assert_eq!(**callee, Expr::Var("fp".to_string()));
        assert_eq!(args.len(), 2);
        assert_eq!(*ret, Some(Ty::Int));
        assert_eq!(*conv, None);

        let StmtKind::CallThrough { ret, args, .. } = &s.body[2].kind else {
            panic!("expected a call-through statement, got {:?}", s.body[2].kind)
        };
        // No `: type` on the statement: a C `void` call.
        assert_eq!(*ret, None);
        assert_eq!(args.len(), 1);
    }

    /// A callee in parentheses is any expression — the vtable read a COM call
    /// is made of — and a trailing convention marker is accepted as on a `dll`.
    #[test]
    fn parses_a_parenthesised_callee_and_a_convention() {
        let m = parse(
            "module m\nsub main\n  var t: ptr = ptr_null()\n  \
             call through (ptr_read_ptr(t, 0))(t) system\nend\n",
        )
        .unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::CallThrough { callee, conv, .. } = &s.body[1].kind else {
            panic!("expected a call-through statement")
        };
        assert!(matches!(callee, Expr::Call { cmd, .. } if cmd == "ptr_read_ptr"));
        assert_eq!(*conv, Some(CallConv::System));
    }

    /// `through` means something only straight after `call`; everywhere else it
    /// is an ordinary identifier.
    #[test]
    fn through_is_a_soft_keyword() {
        let m = parse("module m\nsub main\n  var through: int = 1\n  through = through + 1\nend\n")
            .expect("`through` must stay a usable name");
        assert_eq!(m.subs().next().unwrap().body.len(), 2);
    }

    /// `address` is only special before `of`: on its own it is an ordinary name,
    /// so a variable called `address` still reads and writes.
    #[test]
    fn address_is_a_soft_keyword() {
        let m = parse("module m\nsub main\n  var address: int = 1\n  address = address + 1\nend\n")
            .unwrap();
        let main = m.subs().next().unwrap();
        assert!(matches!(main.body[0].kind, StmtKind::Let { .. }));
        assert!(matches!(main.body[1].kind, StmtKind::Assign { .. }));
    }

    #[test]
    fn folds_a_negative_literal_into_the_literal() {
        // `-5` must BE the literal -5, not a negation of 5: only then does it
        // type `int` at the extremes and stay usable as a form property.
        let m = parse("module m\nsub main\n  let x: int = -2147483648\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        assert!(matches!(
            s.body[0].kind,
            StmtKind::Let {
                value: Expr::IntLit(-2147483648),
                ..
            }
        ));
    }

    #[test]
    fn negates_a_non_literal() {
        let m = parse("module m\nsub main\n  let x: int = 1\n  let y: int = -x\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        assert!(matches!(
            s.body[1].kind,
            StmtKind::Let {
                value: Expr::Neg(_),
                ..
            }
        ));
    }

    /// Unary minus binds tighter than `*`: `-2 * 3` is `(-2) * 3`, which is
    /// -6 either way, but `-a * b` and `-(a * b)` differ in the general case
    /// only by association — what must not happen is the `-` swallowing the
    /// multiplication's right operand.
    #[test]
    fn unary_minus_binds_tighter_than_multiplication() {
        let m = parse("module m\nsub main\n  let x: int = -2 * 3\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { value, .. } = &s.body[0].kind else {
            panic!()
        };
        assert_eq!(
            *value,
            Expr::Bin(
                BinOp::Mul,
                Box::new(Expr::IntLit(-2)),
                Box::new(Expr::IntLit(3))
            )
        );
    }

    #[test]
    fn parses_remainder() {
        let m = parse("module m\nsub main\n  let x: int = 7 % 2\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { value, .. } = &s.body[0].kind else {
            panic!()
        };
        assert!(matches!(value, Expr::Bin(BinOp::Rem, _, _)));
    }

    #[test]
    fn parses_for_with_and_without_a_step() {
        let m = parse("module m\nsub main\n  for i = 1 to 10\n  end\n  for j = 9 to 0 step -3\n  end\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::For { var, step, .. } = &s.body[0].kind else {
            panic!("expected a for")
        };
        assert_eq!((var.as_str(), *step), ("i", 1));
        let StmtKind::For { var, step, .. } = &s.body[1].kind else {
            panic!("expected a for")
        };
        assert_eq!((var.as_str(), *step), ("j", -3));
    }

    #[test]
    fn rejects_a_zero_step_and_a_non_literal_step() {
        assert!(parse("module m\nsub main\n  for i = 1 to 3 step 0\n  end\nend\n").is_err());
        assert!(parse("module m\nsub main\n  var k: int = 2\n  for i = 1 to 3 step k\n  end\nend\n").is_err());
        assert!(parse("module m\nsub main\n  for i = 1 10\n  end\nend\n").is_err());
    }

    /// `break` and `continue` must be reachable from BOTH statement loops —
    /// the one inside `sub` and the one inside `block` — which are separate
    /// code paths that each enumerate statement heads.
    #[test]
    fn parses_break_and_continue_in_both_statement_loops() {
        let m = parse(
            "module m\nsub main\n  break\n  while true\n    continue\n  end\nend\n",
        )
        .unwrap();
        let s = m.subs().next().unwrap();
        assert!(matches!(s.body[0].kind, StmtKind::Break));
        let StmtKind::While { body, .. } = &s.body[1].kind else {
            panic!()
        };
        assert!(matches!(body[0].kind, StmtKind::Continue));
    }

    /// `to` and `step` are soft keywords: they must stay usable as ordinary
    /// names everywhere else, or adding `for` would break existing files.
    #[test]
    fn to_and_step_are_still_ordinary_names() {
        let m = parse("module m\nsub main\n  var to: int = 1\n  var step: int = 2\n  to = step\nend\n").unwrap();
        assert_eq!(m.subs().next().unwrap().body.len(), 3);
    }

    #[test]
    fn parses_an_array_type_and_an_index() {
        let m = parse(
            "module m\nsub f(xs: text[]): int\n  let a: int = xs[0 + 1]\n  return a\nend\n",
        )
        .unwrap();
        let s = m.subs().next().unwrap();
        assert_eq!(s.params, vec![("xs".into(), Ty::Array(Elem::Text))]);
        let StmtKind::Let { value, .. } = &s.body[0].kind else {
            panic!("expected a let")
        };
        assert!(matches!(value, Expr::Index { .. }));
    }

    /// Indexing binds tighter than any operator: `xs[0] + 1` adds to the
    /// element, and folding it the other way would index the sum.
    #[test]
    fn indexing_binds_tighter_than_addition() {
        let m = parse("module m\nsub main\n  var xs: int[] = [1]\n  let a: int = xs[0] + 1\nend\n")
            .unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { value, .. } = &s.body[1].kind else {
            panic!()
        };
        let Expr::Bin(BinOp::Add, l, _) = value else {
            panic!("expected an addition, got {value:?}")
        };
        assert!(matches!(**l, Expr::Index { .. }));
    }

    #[test]
    fn parses_an_element_assignment() {
        let m =
            parse("module m\nsub main\n  var xs: int[] = [1]\n  xs[0] = 2\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        assert!(matches!(s.body[1].kind, StmtKind::SetIndex { .. }));
    }

    // --- 0.8.0 assignment and operator shorthands ---------------------------

    fn first_body(src: &str) -> Vec<Stmt> {
        parse(src).unwrap().subs().next().unwrap().body.clone()
    }

    #[test]
    fn compound_assignment_desugars_to_the_plain_assignment() {
        let b = first_body("module m\nsub main\n  var x: int = 0\n  x += 2\nend\n");
        match &b[1].kind {
            StmtKind::Assign { name, value } => {
                assert_eq!(name, "x");
                // `x += 2` is exactly `x = x + 2`.
                assert_eq!(
                    *value,
                    Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Var("x".into())),
                        Box::new(Expr::IntLit(2))
                    )
                );
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn mod_assign_and_amp_assign_desugar() {
        let b = first_body(
            "module m\nsub main\n  var x: int = 9\n  x mod= 4\n  var s: text = \"a\"\n  s &= \"b\"\nend\n",
        );
        assert!(matches!(
            &b[1].kind,
            StmtKind::Assign { value: Expr::Bin(BinOp::Rem, ..), .. }
        ));
        // `s &= "b"` is `s = concat(s, "b")`.
        match &b[3].kind {
            StmtKind::Assign { value: Expr::Call { cmd, args }, .. } => {
                assert_eq!(cmd, "concat");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected concat call, got {other:?}"),
        }
    }

    #[test]
    fn compound_assignment_reaches_a_property_and_a_path() {
        let b = first_body(
            "module m\nsub main\n  label1.text &= \"!\"\n  r.pt.x += 1\nend\n",
        );
        assert!(matches!(&b[0].kind, StmtKind::SetProperty { .. }));
        assert!(matches!(&b[1].kind, StmtKind::SetPlace { .. }));
    }

    #[test]
    fn increment_and_decrement_desugar_and_stay_soft_keywords() {
        let b = first_body(
            "module m\nsub main\n  var n: int = 0\n  increment n\n  decrement n\nend\n",
        );
        assert!(matches!(
            &b[1].kind,
            StmtKind::Assign { value: Expr::Bin(BinOp::Add, ..), .. }
        ));
        assert!(matches!(
            &b[2].kind,
            StmtKind::Assign { value: Expr::Bin(BinOp::Sub, ..), .. }
        ));
        // `increment` is a soft keyword: assigning to a variable named
        // `increment` still works, because there `=` follows the name.
        let b = first_body("module m\nsub main\n  var increment: int = 0\n  increment = 5\nend\n");
        assert!(matches!(&b[1].kind, StmtKind::Assign { name, .. } if name == "increment"));
    }

    #[test]
    fn a_chained_comparison_is_a_chain() {
        let b = first_body("module m\nsub main\n  var x: int = 5\n  if 1 <= x <= 12\n    return\n  end\nend\n");
        let StmtKind::If { arms, .. } = &b[1].kind else { panic!() };
        assert!(matches!(arms[0].0, Expr::Chain { .. }));
        // Three comparisons in a row still have no single reading.
        assert!(parse("module m\nsub main\n  if 1 < 2 < 3 < 4\n    return\n  end\nend\n").is_err());
    }

    #[test]
    fn membership_parses_and_in_stays_a_name() {
        let b = first_body("module m\nsub main\n  var xs: int[] = [1]\n  if 1 in xs\n    return\n  end\nend\n");
        let StmtKind::If { arms, .. } = &b[1].kind else { panic!() };
        assert!(matches!(arms[0].0, Expr::In { negated: false, .. }));
        let b = first_body("module m\nsub main\n  var xs: int[] = [1]\n  if 9 not in xs\n    return\n  end\nend\n");
        let StmtKind::If { arms, .. } = &b[1].kind else { panic!() };
        assert!(matches!(arms[0].0, Expr::In { negated: true, .. }));
        // `in` is a soft keyword: a variable named `in` still parses.
        assert!(parse("module m\nsub main\n  var in: int = 3\n  call print_int(in)\nend\n").is_ok());
    }

    #[test]
    fn one_line_if_wraps_the_statement() {
        let b = first_body("module m\nsub main\n  call print_int(1) if true\nend\n");
        let StmtKind::If { arms, otherwise } = &b[0].kind else {
            panic!("expected the suffix to become an if")
        };
        assert!(otherwise.is_none());
        assert_eq!(arms.len(), 1);
        assert!(matches!(arms[0].1[0].kind, StmtKind::Call { .. }));
        // `return if COND` reads the `if` as the suffix, not the return value.
        let b = first_body("module m\nsub f(): int\n  return if false\n  return 1\nend\n");
        assert!(matches!(&b[0].kind, StmtKind::If { .. }));
    }

    /// `xs[a]` and `xs[a..b]` are told apart by the `..`, and either bound may
    /// be left out. The index form must be untouched — it is the one every
    /// existing program uses.
    #[test]
    fn a_range_in_brackets_parses_as_a_slice() {
        let b = first_body(
            "module m\nsub main\n  let s: text = \"abcdef\"\n  call print_text(s[2..4])\n  call print_text(s[3..])\n  call print_text(s[..3])\n  call print_text(s[..])\nend\n",
        );
        let cases: Vec<(bool, bool)> = b[1..5]
            .iter()
            .map(|st| {
                let StmtKind::Call { args, .. } = &st.kind else {
                    panic!("expected a call")
                };
                let Expr::Slice { from, to, .. } = &args[0] else {
                    panic!("expected a slice, got {:?}", args[0])
                };
                (from.is_some(), to.is_some())
            })
            .collect();
        assert_eq!(cases, vec![(true, true), (true, false), (false, true), (false, false)]);
        // No `..`: still the plain index it always was.
        let b = first_body("module m\nsub main\n  var xs: int[] = [1]\n  call print_int(xs[1])\nend\n");
        let StmtKind::Call { args, .. } = &b[1].kind else { panic!() };
        assert!(matches!(args[0], Expr::Index { .. }));
        // A slice is a value, not a place.
        assert!(parse("module m\nsub main\n  var xs: int[] = [1]\n  xs[1..2] = [2]\nend\n").is_err());
    }

    /// A block literal carries its newlines and still splits into its holes; a
    /// raw literal is its own bytes, so it is one `TextLit` however many braces
    /// and backslashes are in it.
    #[test]
    fn block_and_raw_literals_lex_and_desugar() {
        let b = first_body("module m\nsub main\n  let s: text = \"\"\"\na\nb\"\"\"\nend\n");
        let StmtKind::Let { value: Expr::TextLit(t), .. } = &b[0].kind else {
            panic!("expected a text literal, got {:?}", b[0].kind)
        };
        // The one newline after the opening delimiter is dropped; the rest is
        // the literal's own.
        assert_eq!(t, "a\nb");
        // A hole in a block still becomes the concat chain.
        let b = first_body("module m\nsub main\n  let n: int = 1\n  let s: text = \"\"\"x{n}\"\"\"\nend\n");
        let StmtKind::Let { value, .. } = &b[1].kind else { panic!() };
        assert!(matches!(value, Expr::Call { cmd, .. } if cmd == "concat"), "{value:?}");
        // Raw: no escapes, no holes, one literal.
        let b = first_body("module m\nsub main\n  let s: text = r\"a\\nb{c}\"\nend\n");
        let StmtKind::Let { value: Expr::TextLit(t), .. } = &b[0].kind else {
            panic!("expected one raw text literal, got {:?}", b[0].kind)
        };
        assert_eq!(t, "a\\nb{c}");
        // `r` is a prefix, not a keyword.
        assert!(parse("module m\nsub main\n  var r: int = 1\n  call print_int(r)\nend\n").is_ok());
        // A newline in a one-line literal is still refused.
        assert!(parse("module m\nsub main\n  let s: text = \"a\nb\"\nend\n").is_err());
    }

    #[test]
    fn decimal_underscores_and_trailing_commas_parse() {
        let b = first_body("module m\nsub main\n  var x: int = 1_000_000\n  var xs: int[] = [1, 2,]\n  call print_int(count(xs),)\nend\n");
        assert!(matches!(&b[0].kind, StmtKind::Let { value: Expr::IntLit(1_000_000), .. }));
        // `1_.5`, `1__0`, `5_` are all rejected by the lexer.
        assert!(parse("module m\nsub main\n  var x: int = 1__0\nend\n").is_err());
        assert!(parse("module m\nsub main\n  var x: int = 5_\nend\n").is_err());
    }

    /// The limitation is named where it is hit, rather than surfacing as a
    /// complaint about the token that happens to follow.
    #[test]
    fn rejects_an_array_of_arrays() {
        let e = parse("module m\nsub main\n  let xs: int[][] = []\nend\n").unwrap_err();
        assert!(e.msg.contains("cannot hold arrays"), "{}", e.msg);
    }

    #[test]
    fn rejects_an_array_of_byte_sets() {
        assert!(parse("module m\nsub main\n  let b: bytes[] = []\nend\n").is_err());
    }

    #[test]
    fn parses_a_record_declaration_and_a_construction() {
        let m = parse(
            "module m\nrecord point\n  x: int\n  y: text\nend\n\
             sub main\n  let p: point = point(x: 1, y: \"a\")\nend\n",
        )
        .unwrap();
        let r = m.records().next().expect("a record");
        assert_eq!(r.name, "point");
        assert_eq!(r.fields, vec![("x".into(), Ty::Int), ("y".into(), Ty::Text)]);
        assert_eq!(r.field("y"), Some((2, Ty::Text)));
        let s = m.subs().next().unwrap();
        let StmtKind::Let { ty, value, .. } = &s.body[0].kind else {
            panic!("expected a let")
        };
        assert_eq!(*ty, Ty::Record("point"));
        assert!(matches!(value, Expr::RecordLit { .. }), "{value:?}");
    }

    /// `record` stays an ordinary word everywhere except that one position, or
    /// adding records would break files that already use it as a name.
    #[test]
    fn record_is_a_soft_keyword() {
        let m = parse("module m\nsub main\n  var record: int = 1\n  record = 2\nend\n").unwrap();
        assert_eq!(m.subs().next().unwrap().body.len(), 2);
    }

    /// `point(1, 2)` is a call and `point(x: 1)` is a record: the named first
    /// argument is the only difference, and two tokens of lookahead see it.
    #[test]
    fn a_named_first_argument_is_what_makes_it_a_record() {
        let m = parse("module m\nsub main\n  let a: int = f(1, 2)\n  let b: int = f(x: 1)\nend\n")
            .unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { value, .. } = &s.body[0].kind else { panic!() };
        assert!(matches!(value, Expr::Call { .. }), "{value:?}");
        let StmtKind::Let { value, .. } = &s.body[1].kind else { panic!() };
        assert!(matches!(value, Expr::RecordLit { .. }), "{value:?}");
    }

    /// A `.` further along a chain is a field read; the first one is left as a
    /// property read for the checker to resolve.
    #[test]
    fn a_dot_after_an_index_is_a_field() {
        let m = parse("module m\nsub main\n  let n: int = ps[1].x\nend\n").unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { value, .. } = &s.body[0].kind else { panic!() };
        let Expr::Field { base, name } = value else {
            panic!("expected a field read, got {value:?}")
        };
        assert_eq!(name, "x");
        assert!(matches!(**base, Expr::Index { .. }));
    }

    #[test]
    fn parses_a_dictionary_type_and_literal() {
        let m = parse(
            "module m\nsub main\n  var ages: int{} = {\"Ada\": 36}\n  ages[\"Alan\"] = 41\nend\n",
        )
        .unwrap();
        let s = m.subs().next().unwrap();
        let StmtKind::Let { ty, value, .. } = &s.body[0].kind else { panic!() };
        assert_eq!(*ty, Ty::Dict(Elem::Int));
        assert!(matches!(value, Expr::DictLit(p) if p.len() == 1), "{value:?}");
        assert!(matches!(s.body[1].kind, StmtKind::SetIndex { .. }));
    }

    #[test]
    fn rejects_a_record_with_no_fields() {
        let e = parse("module m\nrecord empty\nend\nsub main\nend\n").unwrap_err();
        assert!(e.msg.contains("has no fields"), "{}", e.msg);
    }

    #[test]
    fn parses_constants_and_types_them_by_their_literal() {
        let m = parse(
            "module m\nconst A = 42\nconst BIG = 5000000000\nconst NEG = -7\n             const PI = 3.5\nconst TAG = \"hi\"\nconst OK = true\nsub main\nend\n",
        )
        .unwrap();
        let cs: Vec<_> = m.consts().collect();
        assert_eq!(cs.len(), 6, "six constants");
        assert_eq!(cs[0].ty, Ty::Int);
        assert!(matches!(cs[0].value, Expr::IntLit(42)));
        assert_eq!(cs[1].ty, Ty::Int64, "a value past i32 is int64");
        assert!(matches!(cs[2].value, Expr::IntLit(-7)), "a leading `-` folds into the number");
        assert_eq!(cs[3].ty, Ty::Double);
        assert_eq!(cs[4].ty, Ty::Text);
        assert_eq!(cs[5].ty, Ty::Bool);
    }

    #[test]
    fn a_constant_must_be_a_literal_not_an_expression() {
        let e = parse("module m\nconst A = 1 + 2\nsub main\nend\n").unwrap_err();
        // The `+ 2` is left over after the literal `1`, so the newline check
        // trips — the point is only that a compound value does not parse.
        assert!(e.line > 0, "a non-literal constant must be rejected: {}", e.msg);
    }

    #[test]
    fn const_is_a_soft_keyword() {
        // `const` is only a declaration at module-item position; as a variable
        // name inside a sub it is an ordinary word.
        let m = parse("module m\nsub main\n  var const: int = 1\n  const = 2\nend\n").unwrap();
        assert!(m.consts().next().is_none(), "no module constant here");
    }

    #[test]
    fn precedence() {
        // 2 + 3 * 4 == 2 + (3*4)
        let m = parse("module m\nsub main\n  let x: int = 2 + 3 * 4\nend\n").unwrap();
        let Item::Sub(s) = &m.items[0] else {
            panic!("expected a subroutine")
        };
        match &s.body[0].kind {
            StmtKind::Let {
                value: Expr::Bin(BinOp::Add, _, r),
                ..
            } => {
                assert!(matches!(**r, Expr::Bin(BinOp::Mul, _, _)));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// `record NAME is c` marks the record C-layout and lets a field be `byte`,
    /// which is not a type keyword anywhere else.
    /// The bitwise level of the table, checked by shape rather than by reading
    /// the functions. The two that matter most: a comparison is looser than
    /// every bitwise operator (so a flag test needs no parentheses), and a
    /// shift is tighter than `band` but looser than `+`.
    #[test]
    fn bitwise_precedence() {
        let src = "module m\n\
                   sub main\n\
                     let a: bool = f band m <> 0\n\
                     let b: int = 1 shl 4 band 255\n\
                     let c: int = 1 shl 2 + 2\n\
                     let d: int = 1 bor 6 band 4\n\
                   end\n";
        let m = parse(src).unwrap();
        let Item::Sub(s) = &m.items[0] else { panic!() };
        let value = |i: usize| match &s.body[i].kind {
            StmtKind::Let { value, .. } => value.clone(),
            other => panic!("{other:?}"),
        };
        // `(f band m) <> 0`, not `f band (m <> 0)`.
        assert!(
            matches!(value(0), Expr::Cmp(CmpOp::Ne, l, _) if matches!(*l, Expr::Bit(BitOp::And, ..)))
        );
        // `(1 shl 4) band 255`.
        assert!(
            matches!(value(1), Expr::Bit(BitOp::And, l, _) if matches!(*l, Expr::Bit(BitOp::Shl, ..)))
        );
        // `1 shl (2 + 2)`.
        assert!(
            matches!(value(2), Expr::Bit(BitOp::Shl, _, r) if matches!(*r, Expr::Bin(BinOp::Add, ..)))
        );
        // `1 bor (6 band 4)`.
        assert!(
            matches!(value(3), Expr::Bit(BitOp::Or, _, r) if matches!(*r, Expr::Bit(BitOp::And, ..)))
        );
    }

    /// The infix operator words are soft keywords: they mean the operator only
    /// where an operator can go, so a name is still a name. `bnot` is the one
    /// that is reserved — see `Tok::BNot` for why a prefix operator cannot be
    /// soft.
    #[test]
    fn the_infix_operator_words_are_soft_keywords() {
        let src = "module m\n\
                   sub main\n\
                     let band: int = 1\n\
                     let shl: int = band\n\
                     let x: int = shl shl band\n\
                   end\n";
        let m = parse(src).unwrap();
        let Item::Sub(s) = &m.items[0] else { panic!() };
        assert_eq!(s.body.len(), 3);
        assert!(
            matches!(&s.body[1].kind, StmtKind::Let { value: Expr::Var(n), .. } if n == "band")
        );
        // `shl shl band` is the name, the operator, the name.
        let StmtKind::Let { value: Expr::Bit(BitOp::Shl, l, r), .. } = &s.body[2].kind else {
            panic!("{:?}", s.body[2].kind)
        };
        assert!(matches!(&**l, Expr::Var(n) if n == "shl"));
        assert!(matches!(&**r, Expr::Var(n) if n == "band"));
        // `bnot` is reserved, so it is never a name.
        assert!(parse("module m\nsub main\n  let bnot: int = 1\nend\n").is_err());
        let m = parse("module m\nsub main\n  let x: int = bnot 1\nend\n").unwrap();
        let Item::Sub(s) = &m.items[0] else { panic!() };
        assert!(matches!(&s.body[0].kind, StmtKind::Let { value: Expr::BitNot(_), .. }));
    }

    /// A hex or binary literal reaches the tree as the bits that were written;
    /// how wide they are is decided later, by what they meet.
    #[test]
    fn parses_bit_patterns() {
        let src = "module m\n\
                   sub main\n\
                     let a: int = 0xFF\n\
                     let b: int = 0b1010\n\
                     let c: int = 0xDEAD_BEEF\n\
                     let d: int = -0x10\n\
                   end\n";
        let m = parse(src).unwrap();
        let Item::Sub(s) = &m.items[0] else { panic!() };
        let value = |i: usize| match &s.body[i].kind {
            StmtKind::Let { value, .. } => value.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(value(0), Expr::BitsLit(0xFF));
        assert_eq!(value(1), Expr::BitsLit(0b1010));
        assert_eq!(value(2), Expr::BitsLit(0xDEAD_BEEF));
        // A leading `-` says the pattern was meant as a magnitude, so it
        // collapses to a plain number and stops depending on its width.
        assert_eq!(value(3), Expr::IntLit(-16));
    }

    #[test]
    fn parses_a_c_layout_record_with_a_byte_field() {
        let m = parse(
            "module m\nrecord Mixed is c\n  a: byte\n  b: int\nend\nsub main\nend\n",
        )
        .unwrap();
        let rec = m.records().next().expect("a record");
        assert!(rec.is_c, "`is c` should mark the record C-layout");
        assert_eq!(rec.fields[0], ("a".to_string(), Ty::Byte));
        assert_eq!(rec.fields[1], ("b".to_string(), Ty::Int));
        let (offsets, size, align) =
            rec.c_layout(&crate::Registry::new()).expect("a C layout");
        assert_eq!(offsets, vec![0, 4], "b is aligned to offset 4");
        assert_eq!(size, 8, "the struct is padded to its widest member");
        assert_eq!(align, 4, "the struct is as aligned as its widest member");
    }

    /// The field types a c-record adds for real C structs: the two extra
    /// widths, a nested `is c` record by value, and a fixed inline array.
    #[test]
    fn parses_the_wider_c_record_field_types() {
        let m = parse(
            "module m\nrecord Point is c\n  x: int\n  y: int\nend\n\
             record Wide is c\n  a: int16\n  b: word\n  c: float\n  pt: Point\n\
             \u{20} rgb: byte[16]\nend\nsub main\nend\n",
        )
        .unwrap();
        let wide = m.records().nth(1).expect("the second record");
        assert_eq!(wide.fields[0].1, Ty::Int16, "`int16` is a 16-bit field");
        assert_eq!(wide.fields[1].1, Ty::Int16, "`word` is the same width");
        assert_eq!(wide.fields[2].1, Ty::Float);
        assert_eq!(wide.fields[3].1, Ty::Record("Point"));
        assert_eq!(wide.fields[4].1, crate::carray(Ty::Byte, 16));
    }

    /// A c-record laid out with a nested record and an inline array: the
    /// offsets a C compiler would produce, from the one layout function.
    #[test]
    fn lays_out_a_nested_record_and_an_inline_array() {
        let m = parse(
            "module m\nrecord Point is c\n  x: int\n  y: int\nend\n\
             record Blob is c\n  n: int\n  pt: Point\n  rgb: byte[6]\nend\n\
             sub main\nend\n",
        )
        .unwrap();
        let mut reg = crate::Registry::new();
        for r in m.records() {
            reg.insert_record(r.clone());
        }
        let blob = m.records().nth(1).unwrap();
        let (offsets, size, align) = blob.c_layout(&reg).expect("a C layout");
        // n@0, pt@4 (a Point is 4-aligned), rgb@12, then two bytes of tail
        // padding to the struct's 4-byte alignment.
        assert_eq!(offsets, vec![0, 4, 12]);
        assert_eq!(size, 20);
        assert_eq!(align, 4);
    }

    /// A plain `record` keeps `is_c` false, and `byte` there is a record name,
    /// not the layout type — the containment holds.
    #[test]
    fn a_plain_record_is_not_c_layout() {
        let m = parse("module m\nrecord point\n  x: int\nend\nsub main\nend\n").unwrap();
        assert!(!m.records().next().unwrap().is_c);
    }

    /// `var r: RECT` with no `= value` is a zeroed c-record local; `size of` and
    /// `address of` are the two operators a c-record adds.
    #[test]
    fn parses_zero_init_and_size_of() {
        let m = parse(
            "module m\nrecord RECT is c\n  n: int\nend\n\
             sub main\n  var r: RECT\n  let s: int64 = size of RECT\nend\n",
        )
        .unwrap();
        let Item::Sub(sub) = m.items.iter().find(|i| matches!(i, Item::Sub(_))).unwrap()
        else {
            panic!()
        };
        assert!(
            matches!(&sub.body[0].kind, StmtKind::Let { value: Expr::ZeroInit, .. }),
            "an uninitialised `var` is `ZeroInit`: {:?}",
            sub.body[0].kind
        );
        assert!(
            matches!(&sub.body[1].kind, StmtKind::Let { value: Expr::SizeOf(Ty::Record("RECT")), .. }),
            "`size of RECT` is `SizeOf(Record)`: {:?}",
            sub.body[1].kind
        );
    }
}
