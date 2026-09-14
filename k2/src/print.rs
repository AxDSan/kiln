//! The canonical printer: AST → K2 source.
//!
//! One spelling per program, so `kiln fmt` is a fixed point and a tree-level
//! edit (Studio rewriting a form's designer block, `kiln migrate` emitting a
//! converted program) produces source that reads as though it were written by
//! hand. Four-space indent, braces on their own line, trailing commas in
//! multi-line lists — `design/k2/spec.md` §2.

use crate::ast::*;

/// Print a whole program.
pub fn program(p: &Program) -> String {
    let mut o = Out::default();
    o.leading(&p.leading);
    if !p.leading.is_empty() {
        o.blank();
    }
    if let Some(ns) = &p.namespace {
        o.line(&format!("namespace {ns};"));
        o.blank();
    }
    for u in &p.usings {
        let mut s = String::from("using ");
        if u.is_static {
            s.push_str("static ");
        }
        match &u.alias {
            Some(a) => s.push_str(&format!("{a} = {}", u.path)),
            None => s.push_str(&u.path),
        }
        s.push(';');
        o.line(&s);
    }
    if !p.usings.is_empty() {
        o.blank();
    }
    for (i, it) in p.items.iter().enumerate() {
        if i > 0 {
            o.blank();
        }
        item(&mut o, it);
    }
    if !p.top_level.is_empty() {
        if !p.items.is_empty() {
            o.blank();
        }
        for s in &p.top_level {
            stmt(&mut o, s);
        }
    }
    o.text
}

#[derive(Default)]
struct Out {
    text: String,
    depth: usize,
}

impl Out {
    fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.text.push_str("    ");
        }
        self.text.push_str(s);
        self.text.push('\n');
    }
    fn blank(&mut self) {
        if !self.text.ends_with("\n\n") && !self.text.is_empty() {
            self.text.push('\n');
        }
    }
    fn open(&mut self, header: &str) {
        self.line(header);
        self.line("{");
        self.depth += 1;
    }
    fn close(&mut self) {
        self.depth -= 1;
        self.line("}");
    }
    /// Comments written above a construct, put back verbatim.
    fn leading(&mut self, c: &[String]) {
        for l in c {
            if l.trim().is_empty() {
                self.line("//");
            } else {
                self.line(&format!("//{l}"));
            }
        }
    }
    fn doc(&mut self, d: &Option<String>) {
        if let Some(d) = d {
            for l in d.lines() {
                self.line(&format!("/// {l}"));
            }
        }
    }
}

/// An identifier, escaped with `@` when it collides with a keyword.
fn name(s: &str) -> String {
    if crate::lexer::Kw::is_keyword(s) {
        format!("@{s}")
    } else {
        s.to_string()
    }
}

fn vis(v: Vis) -> &'static str {
    match v {
        Vis::Public => "public ",
        // `internal` is what an unwritten visibility parses as, so writing it
        // out means `kiln fmt` turns `int n;` into `internal int n;` and is not
        // a fixed point. The canonical spelling of a default is to omit it.
        Vis::Internal => "",
        Vis::Private => "private ",
    }
}

fn item(o: &mut Out, it: &Item) {
    match it {
        Item::Enum(e) => {
            o.leading(&e.leading);
            o.doc(&e.doc);
            let backing = match &e.backing {
                Some(t) => format!(" : {}", ty(t)),
                None => String::new(),
            };
            o.open(&format!("{}enum {}{backing}", vis(e.vis), e.name));
            for (n, v) in &e.members {
                match v {
                    Some(v) => o.line(&format!("{n} = {},", expr(v))),
                    None => o.line(&format!("{n},")),
                }
            }
            o.close();
        }
        Item::Interface(i) => {
            o.leading(&i.leading);
            o.doc(&i.doc);
            o.open(&format!("{}interface {}", vis(i.vis), i.name));
            for m in &i.methods {
                o.line(&format!(
                    "{} {}({});",
                    ty(&m.ret),
                    m.name,
                    params(&m.params)
                ));
            }
            o.close();
        }
        Item::Form(f) => {
            o.leading(&f.leading);
            o.doc(&f.doc);
            o.open(&format!("{}partial form {}", vis(f.vis), f.name));
            for (n, v) in &f.properties {
                o.line(&format!("{n} = {};", expr(v)));
            }
            if !f.properties.is_empty() && !f.components.is_empty() {
                o.blank();
            }
            for (i, c) in f.components.iter().enumerate() {
                // One blank line between components: a form block is a list of
                // things with bodies, and run together they are hard to read.
                if i > 0 {
                    o.blank();
                }
                o.leading(&c.leading);
                o.open(&format!("{} {}", c.type_name, c.id));
                for (n, v) in &c.properties {
                    o.line(&format!("{n} = {};", expr(v)));
                }
                for (e, h) in &c.handlers {
                    match h {
                        HandlerRef::Method(m) => o.line(&format!("{e} += {m};")),
                        HandlerRef::Lambda(l) => {
                            o.line(&format!("{e} += {};", lambda(l)));
                        }
                    }
                }
                o.close();
            }
            for fl in &f.fields {
                field(o, fl);
            }
            for (i, m) in f.methods.iter().enumerate() {
                // A blank line separates a method from what came before it —
                // but not when it is the first thing in the block, where it
                // would open the body with an empty line.
                if i > 0 || !f.fields.is_empty() || !f.components.is_empty()
                    || !f.properties.is_empty()
                {
                    o.blank();
                }
                method(o, m);
            }
            o.close();
        }
        Item::Type(t) => type_decl(o, t),
    }
}

fn type_decl(o: &mut Out, t: &TypeDecl) {
    o.leading(&t.leading);
    o.doc(&t.doc);
    for a in &t.attrs {
        o.line(&attribute(a));
    }
    let kw = match t.kind {
        TypeKind::Class => "class",
        TypeKind::StaticClass => "static class",
        TypeKind::Record => "record",
        TypeKind::Struct => "struct",
    };
    let generics = if t.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", t.type_params.join(", "))
    };
    let positional = if t.record_params.is_empty() {
        String::new()
    } else {
        let ps: Vec<String> = t
            .record_params
            .iter()
            .map(|f| {
                let attrs: String = f
                    .attrs
                    .iter()
                    .map(|a| format!("{} ", attribute(a)))
                    .collect();
                format!("{attrs}{} {}", ty(&f.ty), f.name)
            })
            .collect();
        format!("({})", ps.join(", "))
    };
    let bases = if t.implements.is_empty() {
        String::new()
    } else {
        format!(" : {}", t.implements.join(", "))
    };
    let header = format!("{}{kw} {}{generics}{positional}{bases}", vis(t.vis), t.name);
    if t.fields.is_empty() && t.consts.is_empty() && t.methods.is_empty() {
        o.line(&format!("{header};"));
        return;
    }
    o.open(&header);
    for c in &t.consts {
        o.leading(&c.leading);
        o.line(&format!(
            "{}const {} {} = {};",
            vis(c.vis),
            ty(&c.ty),
            c.name,
            expr(&c.value)
        ));
    }
    for f in &t.fields {
        field(o, f);
    }
    for (i, m) in t.methods.iter().enumerate() {
        if i > 0 || !t.fields.is_empty() || !t.consts.is_empty() {
            o.blank();
        }
        method(o, m);
    }
    o.close();
}

fn field(o: &mut Out, f: &Field) {
    o.leading(&f.leading);
    for a in &f.attrs {
        o.line(&attribute(a));
    }
    let ro = if f.is_readonly { "readonly " } else { "" };
    match &f.default {
        Some(d) => o.line(&format!(
            "{}{ro}{} {} = {};",
            vis(f.vis),
            ty(&f.ty),
            name(&f.name),
            expr(d)
        )),
        None => o.line(&format!(
            "{}{ro}{} {};",
            vis(f.vis),
            ty(&f.ty),
            name(&f.name)
        )),
    }
}

fn method(o: &mut Out, m: &Method) {
    o.leading(&m.leading);
    o.doc(&m.doc);
    for a in &m.attrs {
        o.line(&attribute(a));
    }
    let generics = if m.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", m.type_params.join(", "))
    };
    let wheres = if m.constraints.is_empty() {
        String::new()
    } else {
        let cs: Vec<String> = m
            .constraints
            .iter()
            .map(|(t, i)| format!("where {t} : {i}"))
            .collect();
        format!(" {}", cs.join(" "))
    };
    let head = format!(
        "{}{}{}{} {}{generics}({}){wheres}",
        vis(m.vis),
        if m.is_static { "static " } else { "" },
        if m.is_extern { "extern " } else { "" },
        ty(&m.ret),
        m.name,
        params(&m.params)
    );
    if m.is_extern {
        o.line(&format!("{head};"));
        return;
    }
    if let Some(e) = &m.expr_body {
        o.line(&format!("{head} => {};", expr(e)));
        return;
    }
    o.open(&head);
    for s in &m.body {
        stmt(o, s);
    }
    o.close();
}

fn params(ps: &[Param]) -> String {
    ps.iter()
        .map(|p| format!("{} {}", ty(&p.ty), name(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn attribute(a: &Attribute) -> String {
    let mut parts: Vec<String> = a.args.iter().map(expr).collect();
    parts.extend(a.named.iter().map(|(k, v)| format!("{k} = {}", expr(v))));
    if parts.is_empty() {
        format!("[{}]", a.name)
    } else {
        format!("[{}({})]", a.name, parts.join(", "))
    }
}

/// A type as it is written in source — the language server shows it in
/// signatures, so it is public rather than printer-private.
pub fn ty(t: &TypeRef) -> String {
    match t {
        TypeRef::Void => "void".into(),
        TypeRef::Named(n) => n.clone(),
        TypeRef::Optional(i) => format!("{}?", ty(i)),
        TypeRef::Array(i) => format!("{}[]", ty(i)),
        TypeRef::Generic(n, args) => {
            let a: Vec<String> = args.iter().map(ty).collect();
            format!("{n}<{}>", a.join(", "))
        }
    }
}

fn stmt(o: &mut Out, s: &Stmt) {
    o.leading(&s.leading);
    match &s.kind {
        StmtKind::Local {
            name,
            ty: t,
            mutable,
            value,
        } => {
            let head = match t {
                Some(t) => ty(t),
                None if *mutable => "var".into(),
                None => "let".into(),
            };
            o.line(&format!("{head} {} = {};", self::name(name), expr(value)));
        }
        StmtKind::Assign { target, op, value } => {
            let sym = match op {
                AssignOp::Eq => "=",
                AssignOp::Add => "+=",
                AssignOp::Sub => "-=",
                AssignOp::Mul => "*=",
                AssignOp::Div => "/=",
                AssignOp::Rem => "%=",
                AssignOp::NullCoalesce => "??=",
            };
            o.line(&format!("{} {sym} {};", expr(target), expr(value)));
        }
        StmtKind::Expr(e) => o.line(&format!("{};", expr(e))),
        StmtKind::Return(None) => o.line("return;"),
        StmtKind::Return(Some(e)) => o.line(&format!("return {};", expr(e))),
        StmtKind::Break => o.line("break;"),
        StmtKind::Continue => o.line("continue;"),
        StmtKind::Defer(d) => {
            let mut inner = Out {
                text: String::new(),
                depth: 0,
            };
            stmt(&mut inner, d);
            o.line(&format!("defer {}", inner.text.trim()));
        }
        StmtKind::Block(b) => {
            o.line("{");
            o.depth += 1;
            for s in b {
                stmt(o, s);
            }
            o.close();
        }
        StmtKind::If { cond, then, els } => {
            o.open(&format!("if ({})", expr(cond)));
            for s in then {
                stmt(o, s);
            }
            if els.is_empty() {
                o.close();
            } else {
                o.depth -= 1;
                o.line("}");
                o.line("else");
                o.line("{");
                o.depth += 1;
                for s in els {
                    stmt(o, s);
                }
                o.close();
            }
        }
        StmtKind::While { cond, body } => {
            o.open(&format!("while ({})", expr(cond)));
            for s in body {
                stmt(o, s);
            }
            o.close();
        }
        StmtKind::For {
            init,
            cond,
            step,
            body,
        } => {
            let mut head = String::from("for (");
            if let Some(i) = init.as_ref() {
                let mut t = Out::default();
                stmt(&mut t, i);
                head.push_str(t.text.trim());
            } else {
                head.push(';');
            }
            head.push(' ');
            if let Some(c) = cond {
                head.push_str(&expr(c));
            }
            head.push_str("; ");
            if let Some(st) = step.as_ref() {
                let mut t = Out::default();
                stmt(&mut t, st);
                head.push_str(t.text.trim().trim_end_matches(';'));
            }
            head.push(')');
            o.open(&head);
            for s in body {
                stmt(o, s);
            }
            o.close();
        }
        StmtKind::ForEach {
            var,
            value,
            coll,
            body,
        } => {
            match value {
                Some(v) => o.open(&format!("foreach (var ({var}, {v}) in {})", expr(coll))),
                None => o.open(&format!("foreach (var {var} in {})", expr(coll))),
            }
            for s in body {
                stmt(o, s);
            }
            o.close();
        }
    }
}

fn lambda(l: &Lambda) -> String {
    let ps: Vec<String> = l.params.iter().map(|(n, _)| name(n)).collect();
    let head = if ps.len() == 1 {
        ps[0].clone()
    } else {
        format!("({})", ps.join(", "))
    };
    match &l.body {
        LambdaBody::Expr(e) => format!("{head} => {}", expr(e)),
        LambdaBody::Block(b) => {
            let mut o = Out::default();
            for s in b {
                stmt(&mut o, s);
            }
            let body: Vec<&str> = o.text.lines().map(|l| l.trim()).collect();
            format!("{head} => {{ {} }}", body.join(" "))
        }
    }
}

/// Print an expression. Parenthesised by structure rather than by precedence,
/// so the result always re-parses to the same tree.
pub fn expr(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Collection(items) => {
            let parts: Vec<String> = items.iter().map(expr).collect();
            format!("[{}]", parts.join(", "))
        }
        ExprKind::Int(v) => v.to_string(),
        ExprKind::Float(v, f32) => {
            if *f32 {
                format!("{v}f")
            } else {
                let s = v.to_string();
                if s.contains('.') || s.contains('e') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
        }
        ExprKind::Bool(b) => b.to_string(),
        ExprKind::Str(s) => format!("\"{}\"", escape(s)),
        ExprKind::Char(c) => format!("'{}'", escape(&c.to_string())),
        ExprKind::Null => "null".into(),
        ExprKind::Ident(n) => name(n),
        ExprKind::Member(b, n) => format!("{}.{}", expr(b), name(n)),
        ExprKind::Call(c, args) => {
            let a: Vec<String> = args.iter().map(expr).collect();
            format!("{}({})", expr(c), a.join(", "))
        }
        ExprKind::Index(b, i) => format!("{}[{}]", expr(b), expr(i)),
        ExprKind::Unary(op, x) => {
            let s = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
                UnOp::BitNot => "~",
            };
            // A unary binds tighter than any binary.
            let inner = match &x.kind {
                ExprKind::Binary(..) | ExprKind::Ternary(..) | ExprKind::NullCoalesce(..) => {
                    format!("({})", expr(x))
                }
                _ => expr(x),
            };
            format!("{s}{inner}")
        }
        ExprKind::Binary(op, a, b) => {
            // Parenthesise only where precedence or associativity needs it, so
            // the canonical spelling reads the way it was written.
            let p = prec(*op);
            let left = operand(a, p, false);
            let right = operand(b, p, true);
            format!("{left} {} {right}", bin_sym(*op))
        }
        ExprKind::Range(a, b, inclusive) => {
            format!(
                "{}{}{}",
                expr(a),
                if *inclusive { ".." } else { "..<" },
                expr(b)
            )
        }
        ExprKind::Cast(t, x) => format!("({}){}", ty(t), expr(x)),
        ExprKind::New(t, args, inits) => {
            let mut s = format!("new {}", ty(t));
            if !args.is_empty() || inits.is_empty() {
                let a: Vec<String> = args.iter().map(expr).collect();
                s.push_str(&format!("({})", a.join(", ")));
            }
            if !inits.is_empty() {
                let i: Vec<String> = inits
                    .iter()
                    .map(|(n, v)| format!("{n} = {}", expr(v)))
                    .collect();
                s.push_str(&format!(" {{ {} }}", i.join(", ")));
            }
            s
        }
        ExprKind::Ternary(c, a, b) => {
            format!("({} ? {} : {})", expr(c), expr(a), expr(b))
        }
        ExprKind::NullCoalesce(a, b) => format!("({} ?? {})", expr(a), expr(b)),
        ExprKind::Try(x) => format!("{}?", expr(x)),
        ExprKind::Lambda(l) => lambda(l),
        ExprKind::Interp(segs) => {
            let mut s = String::from("$\"");
            for seg in segs {
                match seg {
                    InterpSeg::Lit(l) => {
                        s.push_str(&escape(l).replace('{', "{{").replace('}', "}}"))
                    }
                    InterpSeg::Expr(x) => s.push_str(&format!("{{{}}}", expr(x))),
                }
            }
            s.push('"');
            s
        }
        ExprKind::Switch(subject, arms) => {
            let a: Vec<String> = arms
                .iter()
                .map(|arm| {
                    let p = match &arm.pat {
                        SwitchPat::Discard => "_".to_string(),
                        SwitchPat::Const(c) => expr(c),
                        SwitchPat::Relational(op, c) => format!("{} {}", bin_sym(*op), expr(c)),
                    };
                    format!("{p} => {}", expr(&arm.value))
                })
                .collect();
            format!("{} switch {{ {} }}", expr(subject), a.join(", "))
        }
    }
}

/// Binding power, matching the parser's precedence climb.
fn prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 0,
        BinOp::And => 1,
        BinOp::BitOr => 2,
        BinOp::BitXor => 3,
        BinOp::BitAnd => 4,
        BinOp::Eq | BinOp::Ne => 5,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 6,
        BinOp::Shl | BinOp::Shr => 7,
        BinOp::Add | BinOp::Sub => 8,
        BinOp::Mul | BinOp::Div | BinOp::Rem => 9,
    }
}

/// One side of a binary operator: parenthesised when its own binding is looser,
/// or when it ties on the right of a left-associative operator.
fn operand(e: &Expr, parent: u8, is_right: bool) -> String {
    let needs = match &e.kind {
        ExprKind::Binary(op, _, _) => {
            let p = prec(*op);
            p < parent || (is_right && p == parent)
        }
        ExprKind::Ternary(..) | ExprKind::NullCoalesce(..) | ExprKind::Lambda(..) => true,
        _ => false,
    };
    if needs {
        format!("({})", expr(e))
    } else {
        expr(e)
    }
}

fn bin_sym(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

fn escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\0' => o.push_str("\\0"),
            _ => o.push(c),
        }
    }
    o
}
