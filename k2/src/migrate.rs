//! `kiln migrate`: a Kiln 1.x program → Kiln 2 source.
//!
//! It converts the mechanical part — the shape of declarations, the statement
//! and expression forms, and the naming — and leaves a `// TODO(migrate):`
//! comment above anything it will not decide for you. Those comments ride the
//! tree and come back out of the printer, so a converted file tells you exactly
//! where to look.
//!
//! Indexing is *not* rebased: Kiln 2 counts positions from 1 exactly as 1.x
//! does, so a position stays a position.

use crate::ast::*;
use crate::lexer::Span;
use crate::print;
use kiln_ir as ir;

/// Convert 1.x source to K2 source.
pub fn migrate(src: &str) -> Result<String, String> {
    let m = ir::parse(src).map_err(|e| format!("{}: {}", e.line, e.msg))?;
    Ok(print::program(&module(&m)))
}

fn sp() -> Span {
    Span::default()
}

fn todo_note(what: &str) -> String {
    format!(" TODO(migrate): {what}")
}

// ─── naming ─────────────────────────────────────────────────────────────────

/// `starter_kit` → `StarterKit`. Types, methods and constants take this.
fn pascal(s: &str) -> String {
    let mut out = String::new();
    // 1.x desugaring invents names like `$each$3`; a K2 identifier holds only
    // letters, digits and underscores.
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    for part in cleaned.split('_').filter(|p| !p.is_empty()) {
        let mut cs = part.chars();
        if let Some(c) = cs.next() {
            out.extend(c.to_uppercase());
            out.push_str(cs.as_str());
        }
    }
    if out.is_empty() {
        "V".to_string()
    } else if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("V{out}")
    } else {
        out
    }
}

/// `character_id` → `characterId`. Locals and parameters take this.
fn camel(s: &str) -> String {
    let p = pascal(s);
    let mut cs = p.chars();
    match cs.next() {
        Some(c) => c.to_lowercase().chain(cs).collect(),
        None => p,
    }
}

/// `ITEM_FLAGS` → `ItemFlags`: a 1.x constant is SCREAMING_CASE, and K2
/// constants are PascalCase like everything else named at the type level.
fn const_name(s: &str) -> String {
    pascal(&s.to_lowercase())
}

// ─── types ──────────────────────────────────────────────────────────────────

fn ty(t: ir::Ty) -> TypeRef {
    use ir::Ty as T;
    let name = match t {
        T::Int => "int",
        T::Int64 => "long",
        T::Double => "double",
        T::Text => "string",
        T::Bool => "bool",
        T::Bytes => "Bytes",
        T::Ptr => "Ptr",
        T::Byte => "byte",
        T::Int16 => "short",
        T::Float => "float",
        T::Record(n) => return TypeRef::Named(pascal(n)),
        T::Array(e) => return TypeRef::Generic("List".into(), vec![elem(e)]),
        T::Dict(e) => {
            return TypeRef::Generic(
                "Dictionary".into(),
                vec![TypeRef::Named("string".into()), elem(e)],
            )
        }
        T::Optional(e) => return TypeRef::Optional(Box::new(elem(e))),
        T::CArray(_) => "Bytes",
        T::AnyArray | T::AnyElem | T::AnyDict => "object",
    };
    TypeRef::Named(name.into())
}

fn elem(e: ir::Elem) -> TypeRef {
    ty(e.ty())
}

// ─── module ─────────────────────────────────────────────────────────────────

fn module(m: &ir::Module) -> Program {
    let mut items = Vec::new();
    let mut leading = vec![format!(
        " Migrated from Kiln 1.x by `kiln migrate`. Positions are still 1-based."
    )];

    // Records become records.
    for it in &m.items {
        if let ir::Item::UserType(r) = it {
            items.push(Item::Type(record(r)));
        }
    }

    // Everything else hangs off one static class, which is what a 1.x module is.
    let mut consts = Vec::new();
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    for it in &m.items {
        match it {
            ir::Item::Const(c) => consts.push(ConstDecl {
                leading: Vec::new(),
                vis: Vis::Public,
                name: const_name(&c.name),
                ty: ty(c.ty),
                value: expr(&c.value),
                span: sp(),
            }),
            ir::Item::Var(v) => fields.push(Field {
                leading: Vec::new(),
                attrs: Vec::new(),
                vis: Vis::Private,
                name: camel(&v.name),
                ty: ty(v.ty),
                default: Some(expr(&v.value)),
                is_readonly: false,
                is_const: false,
                span: sp(),
            }),
            ir::Item::Sub(s) => methods.push(sub(s)),
            ir::Item::Dll(d) => methods.push(dll(d)),
            ir::Item::Form(_) | ir::Item::Component(_) => {
                leading.push(todo_note(
                    "this module declared a form; rewrite it as a `form` block (spec §11)",
                ));
            }
            ir::Item::UserType(_) => {}
        }
    }

    items.push(Item::Type(TypeDecl {
        leading: Vec::new(),
        attrs: Vec::new(),
        implements: Vec::new(),
        type_params: Vec::new(),
        kind: TypeKind::StaticClass,
        vis: Vis::Public,
        name: pascal(&m.name),
        record_params: Vec::new(),
        fields,
        consts,
        methods,
        doc: None,
        span: sp(),
    }));

    Program {
        leading,
        namespace: Some(pascal(&m.name)),
        usings: m
            .uses
            .iter()
            .map(|u| Using {
                path: format!("Kiln.{}", pascal(u)),
                is_static: false,
                alias: None,
                span: sp(),
            })
            .collect(),
        items,
        top_level: Vec::new(),
    }
}

fn record(r: &ir::RecordDef) -> TypeDecl {
    TypeDecl {
        leading: Vec::new(),
        attrs: if r.is_c {
            vec![Attribute {
                name: "CLayout".into(),
                args: Vec::new(),
                named: Vec::new(),
            }]
        } else {
            Vec::new()
        },
        implements: Vec::new(),
        type_params: Vec::new(),
        kind: TypeKind::Record,
        vis: Vis::Public,
        name: pascal(&r.name),
        record_params: r
            .fields
            .iter()
            .map(|(n, t)| Field {
                leading: Vec::new(),
                attrs: Vec::new(),
                vis: Vis::Public,
                name: pascal(n),
                ty: ty(*t),
                default: None,
                is_readonly: true,
                is_const: false,
                span: sp(),
            })
            .collect(),
        fields: Vec::new(),
        consts: Vec::new(),
        methods: Vec::new(),
        doc: None,
        span: sp(),
    }
}

fn dll(d: &ir::DllDecl) -> Method {
    Method {
        leading: Vec::new(),
        attrs: vec![Attribute {
            name: "Dll".into(),
            args: vec![lit_str(&d.library)],
            named: match &d.symbol {
                Some(s) => vec![("Entry".into(), lit_str(s))],
                None => Vec::new(),
            },
        }],
        is_extern: true,
        vis: Vis::Public,
        is_static: true,
        name: pascal(&d.name),
        type_params: Vec::new(),
        constraints: Vec::new(),
        params: d
            .params
            .iter()
            .map(|(n, t)| Param {
                name: camel(n),
                ty: ty(*t),
                span: sp(),
            })
            .collect(),
        ret: d.ret.map(ty).unwrap_or(TypeRef::Void),
        body: Vec::new(),
        expr_body: None,
        doc: None,
        span: sp(),
    }
}

fn sub(s: &ir::Sub) -> Method {
    Method {
        leading: Vec::new(),
        attrs: Vec::new(),
        is_extern: false,
        vis: Vis::Public,
        is_static: true,
        name: if s.name == "main" {
            "Main".into()
        } else {
            pascal(&s.name)
        },
        type_params: Vec::new(),
        constraints: Vec::new(),
        params: s
            .params
            .iter()
            .map(|(n, t)| Param {
                name: camel(n),
                ty: ty(*t),
                span: sp(),
            })
            .collect(),
        ret: s.ret.map(ty).unwrap_or(TypeRef::Void),
        body: s.body.iter().map(stmt).collect(),
        expr_body: None,
        doc: None,
        span: sp(),
    }
}

// ─── statements ─────────────────────────────────────────────────────────────

fn mk(kind: StmtKind) -> Stmt {
    Stmt {
        leading: Vec::new(),
        kind,
        span: sp(),
    }
}

fn note(mut s: Stmt, what: &str) -> Stmt {
    s.leading.push(todo_note(what));
    s
}

fn stmt(s: &ir::Stmt) -> Stmt {
    use ir::StmtKind as S;
    match &s.kind {
        S::Let {
            name,
            ty: t,
            value,
            mutable,
        } => mk(StmtKind::Local {
            name: camel(name),
            ty: Some(ty(*t)),
            mutable: *mutable,
            value: expr(value),
        }),
        S::LetInfer {
            name,
            value,
            mutable,
        } => mk(StmtKind::Local {
            name: camel(name),
            ty: None,
            mutable: *mutable,
            value: expr(value),
        }),
        S::Assign { name, value } => mk(StmtKind::Assign {
            target: ident(&camel(name)),
            op: AssignOp::Eq,
            value: expr(value),
        }),
        S::Call { cmd, args } => mk(StmtKind::Expr(call(cmd, args))),
        S::Return { value } => mk(StmtKind::Return(value.as_ref().map(expr))),
        S::Break => mk(StmtKind::Break),
        S::Continue => mk(StmtKind::Continue),
        S::If { arms, otherwise } => {
            // 1.x keeps `else if` as a list of arms; K2 nests them.
            let mut els: Vec<Stmt> = otherwise
                .as_ref()
                .map(|b| b.iter().map(stmt).collect())
                .unwrap_or_default();
            for (cond, body) in arms.iter().rev() {
                els = vec![mk(StmtKind::If {
                    cond: expr(cond),
                    then: body.iter().map(stmt).collect(),
                    els: std::mem::take(&mut els),
                })];
            }
            els.pop().unwrap_or_else(|| mk(StmtKind::Break))
        }
        S::While { cond, body } => mk(StmtKind::While {
            cond: expr(cond),
            body: body.iter().map(stmt).collect(),
        }),
        S::For {
            var,
            start,
            limit,
            step,
            body,
        } => {
            let s = mk(StmtKind::ForEach {
                var: camel(var),
                coll: Expr {
                    kind: ExprKind::Range(Box::new(expr(start)), Box::new(expr(limit)), true),
                    span: sp(),
                },
                body: body.iter().map(stmt).collect(),
            });
            if *step == 1 {
                s
            } else {
                note(
                    s,
                    &format!("this loop stepped by {step}; rewrite it as a `for`"),
                )
            }
        }
        S::ForEach {
            elem, coll, body, ..
        } => mk(StmtKind::ForEach {
            var: camel(elem),
            coll: expr(coll),
            body: body.iter().map(stmt).collect(),
        }),
        S::SetIndex { name, index, value } => mk(StmtKind::Assign {
            target: Expr {
                kind: ExprKind::Index(Box::new(ident(&camel(name))), Box::new(expr(index))),
                span: sp(),
            },
            op: AssignOp::Eq,
            value: expr(value),
        }),
        S::SetPlace { place, value } => mk(StmtKind::Assign {
            target: expr(place),
            op: AssignOp::Eq,
            value: expr(value),
        }),
        S::SetProperty {
            component,
            property,
            value,
        } => mk(StmtKind::Assign {
            target: Expr {
                kind: ExprKind::Member(Box::new(ident(&camel(component))), pascal(property)),
                span: sp(),
            },
            op: AssignOp::Eq,
            value: expr(value),
        }),
        S::Defer(inner) => mk(StmtKind::Defer(Box::new(stmt(inner)))),
        S::Match { .. } => note(
            mk(StmtKind::Expr(lit_str("match"))),
            "a `match` statement becomes a `switch` expression or an if-chain (spec §7)",
        ),
        S::IfSome { .. } => note(
            mk(StmtKind::Expr(lit_str("if some"))),
            "`if some` becomes `if (x != null)`, which narrows `x` (spec §4.3)",
        ),
        S::CallThrough { .. } => note(
            mk(StmtKind::Expr(lit_str("call through"))),
            "an indirect call becomes a `Func<...>` value (spec §4.2)",
        ),
    }
}

// ─── expressions ────────────────────────────────────────────────────────────

fn e(kind: ExprKind) -> Expr {
    Expr { kind, span: sp() }
}

fn ident(n: &str) -> Expr {
    e(ExprKind::Ident(n.to_string()))
}

fn lit_str(s: &str) -> Expr {
    e(ExprKind::Str(s.to_string()))
}

/// A 1.x command becomes its K2 spelling. The standard library's full mapping
/// arrives with Phase 4; the handful here are the ones every program uses.
fn command(name: &str) -> (Option<&'static str>, String) {
    match name {
        "print_text" | "print_int" | "print_double" | "print_bool" => {
            (Some("Console"), "WriteLine".into())
        }
        "length" => (None, "Length".into()),
        _ => (None, pascal(name)),
    }
}

fn call(cmd: &str, args: &[ir::Expr]) -> Expr {
    // 1.x expands `"a {x}"` into `concat("a ", to_text(x))` before we see it.
    // Rebuilding the interpolation is what makes migrated source read like the
    // source it came from.
    if cmd == "concat" && args.len() > 1 {
        let mut segs = Vec::new();
        for a in args {
            flatten_concat(a, &mut segs);
        }
        return e(ExprKind::Interp(segs));
    }
    let (owner, name) = command(cmd);
    let callee = match owner {
        Some(o) => e(ExprKind::Member(Box::new(ident(o)), name)),
        None => ident(&name),
    };
    e(ExprKind::Call(
        Box::new(callee),
        args.iter().map(expr).collect(),
    ))
}

/// 1.x nests `concat` to the left, so `"a {x} b {y}"` arrives as a chain.
/// Flattening it yields one interpolation rather than nested ones.
fn flatten_concat(x: &ir::Expr, out: &mut Vec<InterpSeg>) {
    match x {
        ir::Expr::Call { cmd, args } if cmd == "concat" && args.len() > 1 => {
            for a in args {
                flatten_concat(a, out);
            }
        }
        ir::Expr::TextLit(t) => out.push(InterpSeg::Lit(t.clone())),
        ir::Expr::ToText { value, .. } => out.push(InterpSeg::Expr(Box::new(expr(value)))),
        other => out.push(InterpSeg::Expr(Box::new(expr(other)))),
    }
}

fn expr(x: &ir::Expr) -> Expr {
    use ir::Expr as E;
    match x {
        E::IntLit(v) => e(ExprKind::Int(*v as i128)),
        E::BitsLit(v) => e(ExprKind::Int(*v as i128)),
        E::DoubleLit(v) => e(ExprKind::Float(*v, false)),
        E::BoolLit(b) => e(ExprKind::Bool(*b)),
        // The 1.x parser has already expanded interpolation into `concat`, so a
        // literal's braces are literal — a JSON string, say. Re-splitting them
        // would corrupt it.
        E::TextLit(s) => lit_str(s),
        E::NoneLit => e(ExprKind::Null),
        E::Var(n) => ident(&camel(n)),
        E::Call { cmd, args } => call(cmd, args),
        E::Bin(op, a, b) => {
            let o = match op {
                ir::BinOp::Add => BinOp::Add,
                ir::BinOp::Sub => BinOp::Sub,
                ir::BinOp::Mul => BinOp::Mul,
                ir::BinOp::Div => BinOp::Div,
                ir::BinOp::Rem => BinOp::Rem,
            };
            e(ExprKind::Binary(o, Box::new(expr(a)), Box::new(expr(b))))
        }
        E::Cmp(op, a, b) => {
            let o = match op {
                ir::CmpOp::Eq => BinOp::Eq,
                ir::CmpOp::Ne => BinOp::Ne,
                ir::CmpOp::Lt => BinOp::Lt,
                ir::CmpOp::Le => BinOp::Le,
                ir::CmpOp::Gt => BinOp::Gt,
                ir::CmpOp::Ge => BinOp::Ge,
            };
            e(ExprKind::Binary(o, Box::new(expr(a)), Box::new(expr(b))))
        }
        E::Logical(op, a, b) => {
            let o = match op {
                ir::LogicalOp::And => BinOp::And,
                ir::LogicalOp::Or => BinOp::Or,
            };
            e(ExprKind::Binary(o, Box::new(expr(a)), Box::new(expr(b))))
        }
        E::Bit(op, a, b) => {
            let o = match op {
                ir::BitOp::And => BinOp::BitAnd,
                ir::BitOp::Or => BinOp::BitOr,
                ir::BitOp::Xor => BinOp::BitXor,
                ir::BitOp::Shl => BinOp::Shl,
                ir::BitOp::Shr | ir::BitOp::Ushr => BinOp::Shr,
            };
            e(ExprKind::Binary(o, Box::new(expr(a)), Box::new(expr(b))))
        }
        E::Not(a) => e(ExprKind::Unary(UnOp::Not, Box::new(expr(a)))),
        E::BitNot(a) => e(ExprKind::Unary(UnOp::BitNot, Box::new(expr(a)))),
        E::Neg(a) => e(ExprKind::Unary(UnOp::Neg, Box::new(expr(a)))),
        E::Index { base, index } => e(ExprKind::Index(Box::new(expr(base)), Box::new(expr(index)))),
        E::Field { base, name } => e(ExprKind::Member(Box::new(expr(base)), pascal(name))),
        E::GetProperty {
            component,
            property,
        } => e(ExprKind::Member(
            Box::new(ident(&camel(component))),
            pascal(property),
        )),
        E::ArrayLit(items) => {
            // `{a, b}` has no literal form yet; build a List and add to it.
            let mut call_expr = e(ExprKind::New(
                TypeRef::Generic("List".into(), vec![TypeRef::Named("object".into())]),
                Vec::new(),
                Vec::new(),
            ));
            for it in items {
                call_expr = e(ExprKind::Call(
                    Box::new(e(ExprKind::Member(Box::new(call_expr), "Add".into()))),
                    vec![expr(it)],
                ));
            }
            call_expr
        }
        E::RecordLit { name, fields } => e(ExprKind::New(
            TypeRef::Named(pascal(name)),
            fields.iter().map(|(_, v)| expr(v)).collect(),
            Vec::new(),
        )),
        E::IfElse { cond, then, els } => e(ExprKind::Ternary(
            Box::new(expr(cond)),
            Box::new(expr(then)),
            Box::new(expr(els)),
        )),
        E::Otherwise { value, fallback } => e(ExprKind::NullCoalesce(
            Box::new(expr(value)),
            Box::new(expr(fallback)),
        )),
        E::HasValue(a) => e(ExprKind::Binary(
            BinOp::Ne,
            Box::new(expr(a)),
            Box::new(e(ExprKind::Null)),
        )),
        E::Unwrap(a) => e(ExprKind::Member(Box::new(expr(a)), "Value".into())),
        E::ToText { value, .. } => e(ExprKind::Call(
            Box::new(e(ExprKind::Member(
                Box::new(expr(value)),
                "ToString".into(),
            ))),
            Vec::new(),
        )),
        E::Labeled { value, .. } => expr(value),
        E::SizeOf(_) | E::AddressOf(_) | E::ZeroInit => {
            lit_str("TODO(migrate): interop expression")
        }
        other => {
            // Anything not handled keeps its shape as a string so the file
            // still parses, naming what was there. Braces are stripped: this
            // text can end up inside an interpolation, where a brace would
            // start a hole.
            lit_str(&placeholder(&format!("{other:?}")))
        }
    }
}

/// A stand-in for something migration will not translate. It must be safe
/// inside an interpolation, so it carries no braces.
fn placeholder(what: &str) -> String {
    let flat: String = what
        .chars()
        .map(|c| match c {
            '{' | '}' | '\n' => ' ',
            c => c,
        })
        .collect();
    let short: String = flat.chars().take(80).collect();
    format!("TODO(migrate): {}", short.trim())
}
