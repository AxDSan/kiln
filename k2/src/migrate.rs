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
    migrate_with(src, None)
}

/// As `migrate`, with the module's libraries loaded, so the types of values
/// that come from commands are known. That is what tells a byte-set, whose
/// 1-based 1.x positions become 0-based offsets, from a list, whose do not.
pub fn migrate_with(src: &str, registry: Option<&ir::Registry>) -> Result<String, String> {
    let m = ir::parse(src).map_err(|e| format!("{}: {}", e.line, e.msg))?;
    // Comment blocks, keyed by the line they sit above; the one above
    // `module` is the file's own header.
    let lines: Vec<&str> = src.lines().collect();
    let mut comments = std::collections::HashMap::new();
    let mut header = Vec::new();
    let mut block: Vec<String> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix('#') {
            // `##` is a doc comment in 1.x; `///` is Kiln 2's.
            block.push(match rest.strip_prefix('#') {
                Some(doc) => format!("/{doc}"),
                None => rest.to_string(),
            });
            continue;
        }
        if t.is_empty() {
            block.clear();
            continue;
        }
        if !block.is_empty() {
            if t.starts_with("module") && header.is_empty() {
                header = std::mem::take(&mut block);
            } else {
                comments.insert(i + 1, std::mem::take(&mut block));
            }
        }
    }
    TYPES.with(|t| {
        let mut t = t.borrow_mut();
        t.comments = comments;
        t.reg = registry.cloned().unwrap_or_else(ir::Registry::core);
        t.globals = m
            .items
            .iter()
            .filter_map(|it| match it {
                ir::Item::Var(v) => Some((v.name.clone(), v.ty)),
                _ => None,
            })
            .collect();
        t.subs = m
            .items
            .iter()
            .filter_map(|it| match it {
                ir::Item::Sub(sb) => Some((
                    sb.name.clone(),
                    sb.params
                        .iter()
                        .enumerate()
                        .map(|(i, (n, _))| (n.clone(), sb.defaults.get(i).cloned().flatten()))
                        .collect(),
                )),
                _ => None,
            })
            .collect();
        t.records = m
            .items
            .iter()
            .filter_map(|it| match it {
                ir::Item::UserType(r) => {
                    Some((r.name.clone(), r.fields.iter().map(|(n, _)| n.clone()).collect()))
                }
                _ => None,
            })
            .collect();
        t.consts = m
            .items
            .iter()
            .filter_map(|it| match it {
                ir::Item::Const(c) => Some(c.name.clone()),
                _ => None,
            })
            .collect();
    });
    let mut program = module(&m);
    if !header.is_empty() {
        let mut lead = header;
        lead.push(String::new());
        lead.extend(program.leading.drain(..));
        program.leading = lead;
    }
    Ok(print::program(&program))
}

fn comments_for(line: usize) -> Vec<String> {
    TYPES.with(|t| t.borrow_mut().comments.remove(&line).unwrap_or_default())
}

/// What the converter knows about types while it walks a subroutine. Held in
/// a thread-local because the conversion is a set of free functions over the
/// tree, and threading it through every one of them would be all noise.
struct Types {
    reg: ir::Registry,
    globals: std::collections::HashMap<String, ir::Ty>,
    vars: std::collections::HashMap<String, ir::Ty>,
    /// Names declared as constants — including an enum's members, which 1.x
    /// declares as constants too — so a use is spelled as the declaration is.
    consts: std::collections::HashSet<String>,
    /// Each record's field names, in order — what a `with` update rebuilds.
    records: std::collections::HashMap<String, Vec<String>>,
    /// Each subroutine's parameters and defaults, so a call with named
    /// arguments can be put in order.
    subs: std::collections::HashMap<String, Vec<(String, Option<ir::Expr>)>>,
    /// The comment block written directly above each source line, `#` removed
    /// — so a statement or a subroutine keeps the explanation written for it.
    /// Taken when used, so a block is placed once.
    comments: std::collections::HashMap<usize, Vec<String>>,
}

thread_local! {
    static TYPES: std::cell::RefCell<Types> = std::cell::RefCell::new(Types {
        reg: ir::Registry::core(),
        globals: Default::default(),
        vars: Default::default(),
        consts: Default::default(),
        records: Default::default(),
        subs: Default::default(),
        comments: Default::default(),
    });
}

fn note_var(name: &str, ty: ir::Ty) {
    TYPES.with(|t| {
        t.borrow_mut().vars.insert(name.to_string(), ty);
    });
}

fn type_of(x: &ir::Expr) -> Option<ir::Ty> {
    TYPES.with(|t| {
        let t = t.borrow();
        let mut vars = t.globals.clone();
        vars.extend(t.vars.iter().map(|(k, v)| (k.clone(), *v)));
        ir::sema::type_of_expr(x, &vars, &t.reg).ok()
    })
}

/// Replace every use of the name `from` in `x` with `to`.
fn subst_ident(x: &mut Expr, from: &str, to: &Expr) {
    match &mut x.kind {
        ExprKind::Ident(n) if n == from => *x = to.clone(),
        ExprKind::Binary(_, a, b) | ExprKind::NullCoalesce(a, b) | ExprKind::Index(a, b) => {
            subst_ident(a, from, to);
            subst_ident(b, from, to);
        }
        ExprKind::Unary(_, a) | ExprKind::Member(a, _) | ExprKind::Cast(_, a) => subst_ident(a, from, to),
        ExprKind::Call(c, args) => {
            subst_ident(c, from, to);
            for a in args {
                subst_ident(a, from, to);
            }
        }
        ExprKind::Ternary(c, a, b) => {
            subst_ident(c, from, to);
            subst_ident(a, from, to);
            subst_ident(b, from, to);
        }
        ExprKind::Interp(segs) => {
            for sg in segs {
                if let InterpSeg::Expr(e) = sg {
                    subst_ident(e, from, to);
                }
            }
        }
        _ => {}
    }
}

/// `{"a": 1}` as `new Dictionary<string, int> { ["a"] = 1 }`.
fn dict_init(t: TypeRef, pairs: &[(ir::Expr, ir::Expr)]) -> Expr {
    e(ExprKind::DictInit(
        t,
        pairs.iter().map(|(k, v)| (expr(k), expr(v))).collect(),
    ))
}

/// An index into a byte-set counts from 0 in Kiln 2 and from 1 in 1.x; into
/// anything else, from 1 in both.
fn position(base_is_bytes: bool, index: &ir::Expr) -> Expr {
    if !base_is_bytes {
        return expr(index);
    }
    match index {
        ir::Expr::IntLit(v) => e(ExprKind::Int(*v as i128 - 1)),
        other => e(ExprKind::Binary(
            BinOp::Sub,
            Box::new(expr(other)),
            Box::new(e(ExprKind::Int(1))),
        )),
    }
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
    // A 1.x name written in SCREAMING_CASE is a constant by convention, and a
    // constant is PascalCase in K2. Lower-casing the first letter alone would
    // give `tIMES` — which names nothing, and disagrees with the `Times` the
    // declaration was given, so the migrated program would not compile.
    if s.chars().any(|c| c.is_alphabetic()) && !s.chars().any(|c| c.is_lowercase()) {
        return const_name(s);
    }
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
    let leading = vec![format!(
        " Migrated from Kiln 1.x by `kiln migrate`. Positions are still 1-based."
    )];

    // Records become records.
    for it in &m.items {
        if let ir::Item::UserType(r) = it {
            items.push(Item::Type(record(r)));
        }
    }

    // A module with a form becomes the form: its components in the designer's
    // half, and its state, subroutines and foreign functions in the other,
    // where the handlers can reach them. Constants stay in a static class,
    // since a form holds none.
    if let Some(form) = m.items.iter().find_map(|it| match it {
        ir::Item::Form(f) => Some(f),
        _ => None,
    }) {
        return form_module(m, form, items, leading);
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
            // A component with no rectangle sits at namespace level, beside
            // the class whose methods handle its events.
            ir::Item::Component(c) => items.push(Item::Component(component_decl(c))),
            ir::Item::Form(_) => {}
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

/// A 1.x component as Kiln 2 writes one.
fn component_decl(c: &ir::Component) -> ComponentDecl {
    ComponentDecl {
        type_name: pascal(&c.type_name),
        leading: Vec::new(),
        id: camel(&c.id),
        properties: c
            .properties
            .iter()
            .map(|(n, v)| (pascal(n), expr(v)))
            .collect(),
        handlers: c
            .handlers
            .iter()
            .map(|(ev, h)| (pascal(ev), HandlerRef::Method(pascal(h))))
            .collect(),
        span: sp(),
    }
}

/// A 1.x module with a form, as two `partial form` blocks.
fn form_module(
    m: &ir::Module,
    form: &ir::Form,
    mut items: Vec<Item>,
    mut leading: Vec<String>,
) -> Program {
    let name = pascal(&form.name);
    let component = |c: &ir::Component| ComponentDecl {
        type_name: pascal(&c.type_name),
        leading: Vec::new(),
        id: camel(&c.id),
        properties: c
            .properties
            .iter()
            .map(|(n, v)| (pascal(n), expr(v)))
            .collect(),
        handlers: c
            .handlers
            .iter()
            .map(|(ev, h)| (pascal(ev), HandlerRef::Method(pascal(h))))
            .collect(),
        span: sp(),
    };
    if !form.handlers.is_empty() {
        leading.push(todo_note(
            "the form itself had event handlers; wire them in code from `Main`",
        ));
    }

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
                vis: Vis::Internal,
                name: camel(&v.name),
                ty: ty(v.ty),
                default: Some(expr(&v.value)),
                is_readonly: false,
                is_const: false,
                span: sp(),
            }),
            ir::Item::Sub(s) => {
                let mut method = sub(s);
                // A form's methods are its own, not a class's statics.
                method.is_static = false;
                method.vis = Vis::Internal;
                methods.push(method);
            }
            ir::Item::Dll(d) => methods.push(dll(d)),
            ir::Item::Component(c) => items.push(Item::Component(component_decl(c))),
            ir::Item::Form(_) | ir::Item::UserType(_) => {}
        }
    }

    if !consts.is_empty() {
        items.push(Item::Type(TypeDecl {
            leading: Vec::new(),
            attrs: Vec::new(),
            implements: Vec::new(),
            type_params: Vec::new(),
            kind: TypeKind::StaticClass,
            vis: Vis::Public,
            name: format!("{name}Constants"),
            record_params: Vec::new(),
            fields: Vec::new(),
            consts,
            methods: Vec::new(),
            doc: None,
            span: sp(),
        }));
    }
    items.push(Item::Form(FormDecl {
        vis: Vis::Public,
        leading: Vec::new(),
        name: name.clone(),
        properties: form
            .properties
            .iter()
            .map(|(n, v)| (pascal(n), expr(v)))
            .collect(),
        components: form.children.iter().map(component).collect(),
        fields: Vec::new(),
        methods: Vec::new(),
        doc: None,
        span: sp(),
    }));
    items.push(Item::Form(FormDecl {
        vis: Vis::Public,
        leading: Vec::new(),
        name,
        properties: Vec::new(),
        components: Vec::new(),
        fields,
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
            // The C symbol is the original name unless it was renamed; the
            // method takes Kiln 2's casing, so the entry has to be said.
            named: match d.symbol.clone().or_else(|| (pascal(&d.name) != d.name).then(|| d.name.clone())) {
                Some(s) => vec![("Entry".into(), lit_str(&s))],
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
                default: None,
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
    TYPES.with(|t| {
        t.borrow_mut().vars = s.params.iter().cloned().collect();
    });
    Method {
        leading: comments_for(s.line),
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
            .enumerate()
            .map(|(i, (n, t))| Param {
                name: camel(n),
                ty: ty(*t),
                // 1.x's default values carry over, so a call that leaves one
                // out still means what it meant.
                default: s.defaults.get(i).cloned().flatten().map(|d| expr(&d)),
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
    let mut out = stmt_inner(s);
    let mut lead = comments_for(s.line);
    if !lead.is_empty() {
        lead.extend(out.leading.drain(..));
        out.leading = lead;
    }
    out
}

fn stmt_inner(s: &ir::Stmt) -> Stmt {
    use ir::StmtKind as S;
    match &s.kind {
        S::Let {
            name,
            ty: t,
            value,
            mutable,
        } => {
            note_var(name, *t);
            let value = match value {
                ir::Expr::DictLit(pairs) => dict_init(ty(*t), pairs),
                // `var p: Point` with no value is a zeroed record.
                ir::Expr::ZeroInit => e(ExprKind::New(ty(*t), Vec::new(), Vec::new())),
                other => expr(other),
            };
            mk(StmtKind::Local {
                name: camel(name),
                ty: Some(ty(*t)),
                mutable: *mutable,
                value,
            })
        }
        S::LetInfer {
            name,
            value,
            mutable,
        } => {
            if let Some(t) = type_of(value) {
                note_var(name, t);
            }
            mk(StmtKind::Local {
                name: camel(name),
                ty: None,
                mutable: *mutable,
                value: expr(value),
            })
        }
        // `xs = append(xs, v)` grows the list in 1.x by building a new one; in
        // Kiln 2 the list grows itself.
        S::Assign { name, value } => match value {
            ir::Expr::Call { cmd, args } if cmd == "append" && args.len() == 2
                && matches!(&args[0], ir::Expr::Var(v) if v == name) =>
            {
                mk(StmtKind::Expr(e(ExprKind::Call(
                    Box::new(e(ExprKind::Member(Box::new(ident(&camel(name))), "Add".into()))),
                    vec![expr(&args[1])],
                ))))
            }
            _ => mk(StmtKind::Assign {
                target: ident(&camel(name)),
                op: AssignOp::Eq,
                value: expr(value),
            }),
        },
        S::Call { cmd, args } if cmd == "dict_set" && args.len() == 3 => mk(StmtKind::Assign {
            target: e(ExprKind::Index(Box::new(expr(&args[0])), Box::new(expr(&args[1])))),
            op: AssignOp::Eq,
            value: expr(&args[2]),
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
                value: None,
                coll: Expr {
                    kind: ExprKind::Range(Box::new(expr(start)), Box::new(expr(limit)), true),
                    span: sp(),
                },
                body: body.iter().map(stmt).collect(),
            });
            if *step == 1 {
                return s;
            }
            // Any other step is a counted `for`: the comparison follows the
            // step's sign, as 1.x chose it at compile time. Leaving the range
            // here would silently run a count-down loop zero times.
            let name = camel(var);
            let cmp = if *step > 0 { BinOp::Le } else { BinOp::Ge };
            let _ = s;
            mk(StmtKind::For {
                init: Box::new(Some(mk(StmtKind::Local {
                    name: name.clone(),
                    ty: None,
                    mutable: true,
                    value: expr(start),
                }))),
                cond: Some(e(ExprKind::Binary(
                    cmp,
                    Box::new(ident(&name)),
                    Box::new(expr(limit)),
                ))),
                step: Box::new(Some(mk(StmtKind::Assign {
                    target: ident(&name),
                    op: AssignOp::Eq,
                    value: e(ExprKind::Binary(
                        BinOp::Add,
                        Box::new(ident(&name)),
                        Box::new(e(ExprKind::Int(*step as i128))),
                    )),
                }))),
                body: body.iter().map(stmt).collect(),
            })
        }
        S::ForEach {
            elem, value, coll, body, ..
        } => mk(StmtKind::ForEach {
            var: camel(elem),
            value: value.as_deref().map(camel),
            coll: expr(coll),
            body: body.iter().map(stmt).collect(),
        }),
        S::SetIndex { name, index, value } => mk(StmtKind::Assign {
            target: Expr {
                kind: ExprKind::Index(
                    Box::new(ident(&camel(name))),
                    Box::new(position(
                        type_of(&ir::Expr::Var(name.clone())) == Some(ir::Ty::Bytes),
                        index,
                    )),
                ),
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
        // `match` becomes an if-chain on the value. Each arm's values are
        // alternatives; `else` is the last branch.
        S::Match {
            scrutinee,
            arms,
            otherwise,
        } => {
            let subject = expr(scrutinee);
            let mut chain: Vec<Stmt> = otherwise
                .as_ref()
                .map(|b| b.iter().map(stmt).collect())
                .unwrap_or_default();
            for (values, body) in arms.iter().rev() {
                let mut cond: Option<Expr> = None;
                for v in values {
                    let one = e(ExprKind::Binary(
                        BinOp::Eq,
                        Box::new(subject.clone()),
                        Box::new(expr(v)),
                    ));
                    cond = Some(match cond {
                        None => one,
                        Some(c) => e(ExprKind::Binary(BinOp::Or, Box::new(c), Box::new(one))),
                    });
                }
                let Some(cond) = cond else { continue };
                chain = vec![mk(StmtKind::If {
                    cond,
                    then: body.iter().map(stmt).collect(),
                    els: chain,
                })];
            }
            match chain.len() {
                1 => chain.pop().unwrap(),
                _ => mk(StmtKind::Block(chain)),
            }
        }
        // `if some x = opt` is a presence test that binds the value.
        S::IfSome {
            value,
            bind,
            body,
            otherwise,
        } => {
            let name = camel(bind);
            let decl = mk(StmtKind::Local {
                name: name.clone(),
                ty: None,
                mutable: false,
                value: expr(value),
            });
            let test = mk(StmtKind::If {
                cond: e(ExprKind::Binary(
                    BinOp::Ne,
                    Box::new(ident(&name)),
                    Box::new(e(ExprKind::Null)),
                )),
                then: body.iter().map(stmt).collect(),
                els: otherwise
                    .as_ref()
                    .map(|b| b.iter().map(stmt).collect())
                    .unwrap_or_default(),
            });
            mk(StmtKind::Block(vec![decl, test]))
        }
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
    // 1.x's collections are reached through commands; Kiln 2's are values
    // with members. `dict_get(d, k)` is `d.Get(k)`, `count(xs)` is `xs.Count`.
    let member = |recv: &ir::Expr, name: &str, rest: &[ir::Expr]| {
        e(ExprKind::Call(
            Box::new(e(ExprKind::Member(Box::new(expr(recv)), name.into()))),
            rest.iter().map(expr).collect(),
        ))
    };
    match (cmd, args) {
        ("count" | "dict_count", [xs]) => {
            return e(ExprKind::Member(Box::new(expr(xs)), "Count".into()));
        }
        // 1.x's `dict_get` answers the value type's zero for a missing key.
        ("dict_get", [d, k]) => {
            let got = member(d, "Get", std::slice::from_ref(k));
            let zero = match type_of(d) {
                Some(ir::Ty::Dict(el)) => match el.ty() {
                    ir::Ty::Text => Some(ExprKind::Str(String::new())),
                    ir::Ty::Double | ir::Ty::Float => Some(ExprKind::Float(0.0, false)),
                    ir::Ty::Bool => Some(ExprKind::Bool(false)),
                    ir::Ty::Int | ir::Ty::Int64 | ir::Ty::Int16 => Some(ExprKind::Int(0)),
                    _ => None,
                },
                _ => None,
            };
            return match zero {
                Some(z) => e(ExprKind::NullCoalesce(Box::new(got), Box::new(e(z)))),
                None => got,
            };
        }
        ("dict_has", [d, k]) => return member(d, "ContainsKey", std::slice::from_ref(k)),
        ("dict_remove", [d, k]) => return member(d, "Remove", std::slice::from_ref(k)),
        ("contains", [xs, x]) => return member(xs, "Contains", std::slice::from_ref(x)),
        // `remove(xs, i)` removes by position in place; a command would work on
        // a copy of the list and leave this one as it was.
        ("remove", [xs, i]) => return member(xs, "RemoveAt", std::slice::from_ref(i)),
        ("index_of", [xs, x]) => return member(xs, "IndexOf", std::slice::from_ref(x)),
        // `sort(xs)` sorts this list; a command would sort a copy.
        ("sort", [xs]) => return member(xs, "Sort", &[]),
        _ => {}
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
        // `connect(timeout: 250, host: "x")` — 1.x's named arguments arrive
        // shaped like a record literal. Kiln 2 calls are positional, so the
        // arguments are put in the subroutine's own order, with defaults for
        // the ones left out.
        E::RecordLit { name, fields }
            if TYPES.with(|t| t.borrow().subs.contains_key(name))
                && !TYPES.with(|t| t.borrow().records.contains_key(name)) =>
        {
            let params = TYPES.with(|t| t.borrow().subs[name].clone());
            let last_given = params
                .iter()
                .rposition(|(p, _)| fields.iter().any(|(f, _)| f == p))
                .map(|i| i + 1)
                .unwrap_or(0);
            let args = params[..last_given]
                .iter()
                .map(|(p, default)| match fields.iter().find(|(f, _)| f == p) {
                    Some((_, v)) => expr(v),
                    None => default.as_ref().map(expr).unwrap_or_else(|| e(ExprKind::Int(0))),
                })
                .collect();
            e(ExprKind::Call(Box::new(ident(&pascal(name))), args))
        }
        // `xs[2..3]` on a list is `xs.Slice(2, 3)`; on text, `Substr`.
        E::Slice { base, from, to } => {
            let lo = from.as_deref().map(expr).unwrap_or_else(|| e(ExprKind::Int(1)));
            if type_of(base) == Some(ir::Ty::Text) {
                let count = match to.as_deref() {
                    Some(t) => e(ExprKind::Binary(
                        BinOp::Add,
                        Box::new(e(ExprKind::Binary(BinOp::Sub, Box::new(expr(t)), Box::new(lo.clone())))),
                        Box::new(e(ExprKind::Int(1))),
                    )),
                    None => e(ExprKind::Call(Box::new(ident("Length")), vec![expr(base)])),
                };
                return e(ExprKind::Call(Box::new(ident("Substr")), vec![expr(base), lo, count]));
            }
            let hi = to
                .as_deref()
                .map(expr)
                .unwrap_or_else(|| e(ExprKind::Member(Box::new(expr(base)), "Count".into())));
            e(ExprKind::Call(
                Box::new(e(ExprKind::Member(Box::new(expr(base)), "Slice".into()))),
                vec![lo, hi],
            ))
        }
        // `origin with x: 3` rebuilds the record: the changed fields from the
        // update, the rest from the original.
        E::RecordUpdate { name, base, fields } if TYPES.with(|t| t.borrow().records.contains_key(name)) => {
            let names = TYPES.with(|t| t.borrow().records[name].clone());
            let args = names
                .iter()
                .map(|f| match fields.iter().find(|(n, _)| n == f) {
                    Some((_, v)) => expr(v),
                    None => e(ExprKind::Member(Box::new(expr(base)), pascal(f))),
                })
                .collect();
            e(ExprKind::New(TypeRef::Named(pascal(name)), args, Vec::new()))
        }
        // `x in xs` / `k in d` / `sub in text`, by what the haystack is.
        E::In { needle, haystack, negated } => {
            let test = match type_of(haystack) {
                Some(ir::Ty::Dict(_)) | Some(ir::Ty::AnyDict) => e(ExprKind::Call(
                    Box::new(e(ExprKind::Member(Box::new(expr(haystack)), "ContainsKey".into()))),
                    vec![expr(needle)],
                )),
                Some(ir::Ty::Text) => e(ExprKind::Binary(
                    BinOp::Ne,
                    Box::new(e(ExprKind::Call(
                        Box::new(ident("Find")),
                        vec![expr(haystack), expr(needle)],
                    ))),
                    Box::new(e(ExprKind::Int(0))),
                )),
                _ => e(ExprKind::Call(
                    Box::new(e(ExprKind::Member(Box::new(expr(haystack)), "Contains".into()))),
                    vec![expr(needle)],
                )),
            };
            if *negated {
                e(ExprKind::Unary(UnOp::Not, Box::new(test)))
            } else {
                test
            }
        }
        // `[body for each x in xs if cond]` — `Where` then `Select`, over a list.
        // Over a dictionary its values; binding the key as well is left for a
        // person, since Kiln 2 has no pair type to select from.
        E::Comprehension { body, elem, value, index, coll, cond, .. } if index.is_none() => {
            // Over a dictionary, walk its keys; a use of the value becomes a
            // lookup by that key. Over a list, walk the list.
            // A comprehension that never uses the key walks the values directly.
            let uses = |x: &ir::Expr, name: &str| format!("{x:?}").contains(&format!("Var({name:?})"));
            let key_used = value.is_some()
                && (uses(body, elem) || cond.as_deref().is_some_and(|c| uses(c, elem)));
            if let (Some(v), false) = (value, key_used) {
                let var = camel(v);
                let lambda = |b: Expr| {
                    e(ExprKind::Lambda(Lambda {
                        params: vec![(var.clone(), None)],
                        body: LambdaBody::Expr(Box::new(b)),
                    }))
                };
                let mut src = e(ExprKind::Member(Box::new(expr(coll)), "Values".into()));
                if let Some(c) = cond {
                    src = e(ExprKind::Call(
                        Box::new(e(ExprKind::Member(Box::new(src), "Where".into()))),
                        vec![lambda(expr(c))],
                    ));
                }
                if !matches!(body.as_ref(), ir::Expr::Var(n) if n == v) {
                    src = e(ExprKind::Call(
                        Box::new(e(ExprKind::Member(Box::new(src), "Select".into()))),
                        vec![lambda(expr(body))],
                    ));
                }
                return src;
            }
            let over_dict = value.is_some();
            let var = camel(elem);
            let base = expr(coll);
            let mut src = if over_dict {
                e(ExprKind::Member(Box::new(base.clone()), "Keys".into()))
            } else {
                base.clone()
            };
            let convert = |x: &ir::Expr| {
                let mut out = expr(x);
                if let Some(v) = value {
                    let lookup = e(ExprKind::Call(
                        Box::new(e(ExprKind::Member(Box::new(base.clone()), "Get".into()))),
                        vec![ident(&var)],
                    ));
                    let lookup = match type_of(coll) {
                        Some(ir::Ty::Dict(el)) => match el.ty() {
                            ir::Ty::Text => e(ExprKind::NullCoalesce(Box::new(lookup), Box::new(e(ExprKind::Str(String::new()))))),
                            ir::Ty::Int | ir::Ty::Int64 | ir::Ty::Int16 => e(ExprKind::NullCoalesce(Box::new(lookup), Box::new(e(ExprKind::Int(0))))),
                            _ => lookup,
                        },
                        _ => lookup,
                    };
                    subst_ident(&mut out, &camel(v), &lookup);
                }
                out
            };
            let lambda = |b: Expr| {
                e(ExprKind::Lambda(Lambda {
                    params: vec![(var.clone(), None)],
                    body: LambdaBody::Expr(Box::new(b)),
                }))
            };
            if let Some(c) = cond {
                src = e(ExprKind::Call(
                    Box::new(e(ExprKind::Member(Box::new(src), "Where".into()))),
                    vec![lambda(convert(c))],
                ));
            }
            let is_identity = matches!(body.as_ref(), ir::Expr::Var(n) if n == elem);
            if !is_identity {
                src = e(ExprKind::Call(
                    Box::new(e(ExprKind::Member(Box::new(src), "Select".into()))),
                    vec![lambda(convert(body))],
                ));
            }
            src
        }
        // `"aa" * 3` repeats text in 1.x; Kiln 2 has no such operator, and
        // core's `repeat` is what it always meant.
        E::Bin(ir::BinOp::Mul, a, b) if type_of(a) == Some(ir::Ty::Text) => e(ExprKind::Call(
            Box::new(ident("Repeat")),
            vec![expr(a), expr(b)],
        )),
        // `0 <= pick <= 2` — Kiln 2 has no chained comparison, so it is the
        // conjunction. `mid` appears twice; 1.x evaluated it once, which only
        // differs when it has a side effect, and then it is marked.
        E::Chain { lo, lo_op, mid, hi_op, hi } => {
            let cmp = |op: &ir::CmpOp| match op {
                ir::CmpOp::Lt => BinOp::Lt,
                ir::CmpOp::Le => BinOp::Le,
                ir::CmpOp::Gt => BinOp::Gt,
                ir::CmpOp::Ge => BinOp::Ge,
                ir::CmpOp::Eq => BinOp::Eq,
                ir::CmpOp::Ne => BinOp::Ne,
            };
            let left = e(ExprKind::Binary(cmp(lo_op), Box::new(expr(lo)), Box::new(expr(mid))));
            let right = e(ExprKind::Binary(cmp(hi_op), Box::new(expr(mid)), Box::new(expr(hi))));
            e(ExprKind::Binary(BinOp::And, Box::new(left), Box::new(right)))
        }
        E::IntLit(v) => e(ExprKind::Int(*v as i128)),
        E::BitsLit(v) => e(ExprKind::Int(*v as i128)),
        E::DoubleLit(v) => e(ExprKind::Float(*v, false)),
        E::BoolLit(b) => e(ExprKind::Bool(*b)),
        // The 1.x parser has already expanded interpolation into `concat`, so a
        // literal's braces are literal — a JSON string, say. Re-splitting them
        // would corrupt it.
        E::TextLit(s) => lit_str(s),
        E::NoneLit => e(ExprKind::Null),
        // 1.x's `none` reaches here as a name when it is an initialiser.
        E::Var(n) if n == "none" => e(ExprKind::Null),
        E::Var(n) if TYPES.with(|t| t.borrow().consts.contains(n)) => ident(&const_name(n)),
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
                ir::BitOp::Shr => BinOp::Shr,
                // 1.x's `ushr` is C#'s `>>>`; mapping it to `>>` shifted the
                // sign in and gave `-16 ushr 2` as -4.
                ir::BitOp::Ushr => BinOp::UShr,
            };
            e(ExprKind::Binary(o, Box::new(expr(a)), Box::new(expr(b))))
        }
        E::Not(a) => e(ExprKind::Unary(UnOp::Not, Box::new(expr(a)))),
        E::BitNot(a) => e(ExprKind::Unary(UnOp::BitNot, Box::new(expr(a)))),
        E::Neg(a) => e(ExprKind::Unary(UnOp::Neg, Box::new(expr(a)))),
        // `d["k"]` reads as the value type's zero when the key is missing, in
        // 1.x. Kiln 2 has no reading indexer on a dictionary — there is nothing
        // to throw — so it is `d.Get("k") ?? zero`, which is what it meant.
        E::Index { base, index } if matches!(type_of(base), Some(ir::Ty::Dict(_))) => {
            let Some(ir::Ty::Dict(el)) = type_of(base) else { unreachable!() };
            let got = e(ExprKind::Call(
                Box::new(e(ExprKind::Member(Box::new(expr(base)), "Get".into()))),
                vec![expr(index)],
            ));
            let zero = match el.ty() {
                ir::Ty::Text => Some(ExprKind::Str(String::new())),
                ir::Ty::Double | ir::Ty::Float => Some(ExprKind::Float(0.0, false)),
                ir::Ty::Bool => Some(ExprKind::Bool(false)),
                ir::Ty::Int | ir::Ty::Int64 | ir::Ty::Int16 => Some(ExprKind::Int(0)),
                // A record has no zero to write; the reader gets the `T?`.
                _ => None,
            };
            match zero {
                Some(z) => e(ExprKind::NullCoalesce(Box::new(got), Box::new(e(z)))),
                None => got,
            }
        }
        E::Index { base, index } => e(ExprKind::Index(
            Box::new(expr(base)),
            Box::new(position(type_of(base) == Some(ir::Ty::Bytes), index)),
        )),
        E::Field { base, name } => e(ExprKind::Member(Box::new(expr(base)), pascal(name))),
        E::GetProperty {
            component,
            property,
        } => e(ExprKind::Member(
            Box::new(ident(&camel(component))),
            pascal(property),
        )),
        // `[a, b]` is a list in Kiln 2, typed by where it is used.
        E::ArrayLit(items) => e(ExprKind::Collection(items.iter().map(expr).collect())),
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
        E::DictLit(pairs) => dict_init(
            type_of(x).map(ty).unwrap_or_else(|| {
                TypeRef::Generic(
                    "Dictionary".into(),
                    vec![TypeRef::Named("string".into()), TypeRef::Named("string".into())],
                )
            }),
            pairs,
        ),
        // `address of summer` — in Kiln 2 the method itself, where a pointer is
        // wanted, is its address.
        E::AddressOf(name) => {
            // A variable's address: a record is already held by reference, so
            // the record itself goes where a pointer is wanted.
            if type_of(&ir::Expr::Var(name.clone())).is_some() {
                ident(&camel(name))
            } else {
                ident(&pascal(name))
            }
        }
        // `size of Point` — the laid-out size, known while compiling.
        E::SizeOf(t) => e(ExprKind::Member(Box::new(e(ExprKind::Ident(match ty(*t) {
            TypeRef::Named(n) => n,
            other => print::ty(&other),
        }))), "Size".into())),
        E::ZeroInit => {
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
