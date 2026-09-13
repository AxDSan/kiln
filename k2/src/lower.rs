//! K2 AST → KIR. A first lowerer for the runnable subset.
//!
//! Resolution and desugaring happen here so the KIR the backend sees is plain:
//! names become ids, `Console.WriteLine` becomes `printf`, ternaries and
//! short-circuit `&&`/`||` become `If` into a temp, interpolation becomes a run
//! of prints. Types are resolved to `kiln_kir::TyId`.

use crate::ast;
use crate::lexer::Span as LSpan;
use kiln_kir::build::ModuleBuilder;
use kiln_kir::*;
use std::collections::HashMap;

pub fn lower(p: &ast::Program) -> Result<Module, String> {
    let mut b = ModuleBuilder::new(
        p.namespace.as_deref().unwrap_or("program"),
        ModuleKind::Console,
        Target::X86_64_LINUX,
    );

    // Pass 1: reserve a RecordId + interned type for every declared type.
    let mut type_ids: HashMap<String, RecordId> = HashMap::new();
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if td.kind != ast::TypeKind::StaticClass {
                let rid = RecordId(b.m.records.len() as u32);
                b.m.records.push(RecordDef {
                    id: rid,
                    name: td.name.clone(),
                    fields: Vec::new(),
                    layout: Layout::Managed,
                    equality: match td.kind {
                        ast::TypeKind::Record => Equality::ByValue,
                        _ => Equality::ByRef,
                    },
                });
                let _ = b.m.types.intern(TyKind::Record(rid));
                type_ids.insert(td.name.clone(), rid);
            }
        }
    }

    // Collect enum members as integer constants.
    let mut enums: HashMap<String, HashMap<String, i128>> = HashMap::new();
    for item in &p.items {
        if let ast::Item::Enum(ed) = item {
            let mut members = HashMap::new();
            let mut next = 0i128;
            for (name, val) in &ed.members {
                let v = match val {
                    Some(e) => const_int(e)?,
                    None => next,
                };
                members.insert(name.clone(), v);
                next = v + 1;
            }
            enums.insert(ed.name.clone(), members);
        }
    }

    let mut cx = Cx {
        b,
        type_ids,
        enums,
        consts: HashMap::new(),
        methods: HashMap::new(),
    };

    // Pass 2: fill record fields and compute C layout.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            if td.kind == ast::TypeKind::StaticClass {
                continue;
            }
            let rid = cx.type_ids[&td.name];
            let mut fields = Vec::new();
            for f in td.record_params.iter().chain(td.fields.iter()) {
                let ty = cx.resolve(&f.ty)?;
                fields.push(FieldDef {
                    name: f.name.clone(),
                    ty,
                });
            }
            let (size, align, offsets) = cx.c_layout(&fields);
            cx.b.m.records[rid.0 as usize].fields = fields;
            cx.b.m.records[rid.0 as usize].layout = Layout::C {
                size,
                align,
                offsets,
            };
        }
    }

    // Collect constants (literal-valued) for reference resolution.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            for c in &td.consts {
                let ty = cx.resolve(&c.ty)?;
                cx.consts.insert(c.name.clone(), (ty, c.value.clone()));
                cx.consts
                    .insert(format!("{}.{}", td.name, c.name), (ty, c.value.clone()));
            }
        }
    }

    // Pass 3: declare all methods (symbol + signature) before lowering bodies.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            for m in &td.methods {
                let this = if m.is_static || td.kind == ast::TypeKind::StaticClass {
                    None
                } else {
                    Some(cx.record_ty(&td.name))
                };
                let mut params = Vec::new();
                if let Some(t) = this {
                    params.push(("this", t));
                }
                let owned: Vec<(String, TyId)> = m
                    .params
                    .iter()
                    .map(|p| Ok((p.name.clone(), cx.resolve(&p.ty)?)))
                    .collect::<Result<_, String>>()?;
                for (n, t) in &owned {
                    params.push((n.as_str(), *t));
                }
                let ret = cx.resolve(&m.ret)?;
                let sym = format!("{}_{}", td.name, m.name);
                let fid = cx.b.declare_func(&sym, params, ret);
                let sig = Sig {
                    fid,
                    this: this.is_some(),
                    params: owned.iter().map(|(_, t)| *t).collect(),
                    ret,
                };
                cx.methods
                    .insert(format!("{}.{}", td.name, m.name), sig.clone());
                // also reachable unqualified from within the same type
                cx.methods.entry(m.name.clone()).or_insert(sig);
            }
        }
    }

    // Pass 4: lower method bodies.
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            for m in &td.methods {
                let sig = cx.methods[&format!("{}.{}", td.name, m.name)].clone();
                cx.lower_method(td, m, &sig)?;
            }
        }
    }

    // Top-level statements become Main.
    if !p.top_level.is_empty() {
        let main = cx.b.declare_func("kmain", vec![], TyTable::VOID);
        let mut fl = FnLower::new(&mut cx, main, TyTable::VOID);
        for s in &p.top_level {
            fl.stmt(s)?;
        }
        let body = fl.finish();
        cx.b.set_body(main, body);
        cx.b.set_entry(main);
    } else if let Some(sig) = cx.methods.get("Main").cloned() {
        cx.b.set_entry(sig.fid);
    } else if let Some(sig) = cx.methods.values().find(|s| false_ref(s)).cloned() {
        let _ = sig;
    }

    Ok(cx.b.build())
}

fn false_ref(_: &Sig) -> bool {
    false
}

#[derive(Clone)]
struct Sig {
    fid: FuncId,
    this: bool,
    params: Vec<TyId>,
    ret: TyId,
}

struct Cx {
    b: ModuleBuilder,
    type_ids: HashMap<String, RecordId>,
    enums: HashMap<String, HashMap<String, i128>>,
    consts: HashMap<String, (TyId, ast::Expr)>,
    methods: HashMap<String, Sig>,
}

impl Cx {
    fn record_ty(&mut self, name: &str) -> TyId {
        let rid = self.type_ids[name];
        self.b.m.types.intern(TyKind::Record(rid))
    }

    fn resolve(&mut self, t: &ast::TypeRef) -> Result<TyId, String> {
        Ok(match t {
            ast::TypeRef::Void => TyTable::VOID,
            ast::TypeRef::Named(n) => match n.as_str() {
                "int" => TyTable::I32,
                "uint" => TyTable::U32,
                "long" => TyTable::I64,
                "ulong" => TyTable::U64,
                "short" => TyTable::I16,
                "ushort" => TyTable::U16,
                "byte" => TyTable::U8,
                "sbyte" => TyTable::I8,
                "nint" => TyTable::NINT,
                "nuint" => TyTable::NUINT,
                "float" => TyTable::F32,
                "double" => TyTable::F64,
                "bool" => TyTable::BOOL,
                "char" => TyTable::CHAR,
                "string" => TyTable::STR,
                "Bytes" => TyTable::BYTES,
                "Ptr" => TyTable::PTR,
                other => {
                    if self.type_ids.contains_key(other) {
                        self.record_ty(other)
                    } else {
                        return Err(format!("unknown type `{other}`"));
                    }
                }
            },
            ast::TypeRef::Optional(inner) => {
                let t = self.resolve(inner)?;
                self.b.m.types.intern(TyKind::Optional(t))
            }
            ast::TypeRef::Array(elem) => {
                let e = self.resolve(elem)?;
                self.b.m.types.intern(TyKind::Array(e))
            }
            ast::TypeRef::Generic(n, args) => match n.as_str() {
                "List" => {
                    let e = self.resolve(&args[0])?;
                    self.b.m.types.intern(TyKind::Array(e))
                }
                _ => return Err(format!("generic type `{n}` not yet supported")),
            },
        })
    }

    fn c_layout(&self, fields: &[FieldDef]) -> (i64, i64, Vec<i64>) {
        let mut offset = 0i64;
        let mut align = 1i64;
        let mut offsets = Vec::new();
        for f in fields {
            let sz = self.scalar_size(f.ty);
            offset = round_up(offset, sz);
            offsets.push(offset);
            offset += sz;
            align = align.max(sz);
        }
        (round_up(offset, align.max(1)), align, offsets)
    }

    fn scalar_size(&self, ty: TyId) -> i64 {
        match self.b.m.types.kind(ty) {
            TyKind::Bool | TyKind::I8 | TyKind::U8 => 1,
            TyKind::I16 | TyKind::U16 => 2,
            TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => 4,
            TyKind::I64 | TyKind::U64 | TyKind::F64 => 8,
            TyKind::Nint | TyKind::Nuint => (self.b.m.target.ptr_bits / 8) as i64,
            _ => (self.b.m.target.ptr_bits / 8) as i64,
        }
    }

    fn lower_method(
        &mut self,
        td: &ast::TypeDecl,
        m: &ast::Method,
        sig: &Sig,
    ) -> Result<(), String> {
        let mut fl = FnLower::new(self, sig.fid, sig.ret);
        // bind `this`
        if sig.this {
            fl.scope
                .insert("this".into(), (LocalId(0), fl_self_this_ty(&fl)));
        }
        // parameters are the leading locals
        let base = if sig.this { 1 } else { 0 };
        for (i, p) in m.params.iter().enumerate() {
            let lid = LocalId((base + i) as u32);
            let ty = fl.cx.methods[&format!("{}.{}", td.name, m.name)].params[i];
            fl.scope.insert(p.name.clone(), (lid, ty));
        }
        if let Some(e) = &m.expr_body {
            let (val, _) = fl.expr(e, Some(sig.ret))?;
            fl.push(Stmt::Return(Some(val)));
        } else {
            for s in &m.body {
                fl.stmt(s)?;
            }
        }
        let body = fl.finish();
        self.b.set_body(sig.fid, body);
        Ok(())
    }
}

fn fl_self_this_ty(fl: &FnLower) -> TyId {
    fl.cx.b.m.func(fl.fid).params[0].ty
}

fn round_up(x: i64, align: i64) -> i64 {
    if align <= 1 {
        x
    } else {
        (x + align - 1) / align * align
    }
}

fn const_int(e: &ast::Expr) -> Result<i128, String> {
    match &e.kind {
        ast::ExprKind::Int(v) => Ok(*v),
        ast::ExprKind::Unary(ast::UnOp::Neg, inner) => Ok(-const_int(inner)?),
        _ => Err("enum member must be a constant integer".into()),
    }
}

// ─── function lowering ──────────────────────────────────────────────────────

struct FnLower<'a> {
    cx: &'a mut Cx,
    fid: FuncId,
    ret: TyId,
    scope: HashMap<String, (LocalId, TyId)>,
    blocks: Vec<Vec<Stmt>>,
}

impl<'a> FnLower<'a> {
    fn new(cx: &'a mut Cx, fid: FuncId, ret: TyId) -> FnLower<'a> {
        FnLower {
            cx,
            fid,
            ret,
            scope: HashMap::new(),
            blocks: vec![Vec::new()],
        }
    }

    fn push(&mut self, s: Stmt) {
        self.blocks.last_mut().unwrap().push(s);
    }

    fn finish(mut self) -> Vec<Stmt> {
        self.blocks.pop().unwrap()
    }

    fn new_local(&mut self, name: &str, ty: TyId) -> LocalId {
        let id = self.cx.b.add_local(self.fid, name, ty);
        id
    }

    fn tt(&self) -> &TyTable {
        &self.cx.b.m.types
    }

    // ─── statements ──────────────────────────────────────────────────────

    fn lower_body(&mut self, stmts: &[ast::Stmt]) -> Result<Vec<Stmt>, String> {
        self.blocks.push(Vec::new());
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(self.blocks.pop().unwrap())
    }

    fn stmt(&mut self, s: &ast::Stmt) -> Result<(), String> {
        match &s.kind {
            ast::StmtKind::Local {
                name, ty, value, ..
            } => {
                let hint = match ty {
                    Some(t) => Some(self.cx.resolve(t)?),
                    None => None,
                };
                let (val, vty) = self.expr(value, hint)?;
                let lty = hint.unwrap_or(vty);
                let lid = self.new_local(name, lty);
                self.scope.insert(name.clone(), (lid, lty));
                self.push(Stmt::Let {
                    local: lid,
                    value: val,
                });
            }
            ast::StmtKind::Assign { target, op, value } => {
                let place = self.place(target)?;
                let pty = self.place_ty(target)?;
                let (rhs, _) = self.expr(value, Some(pty))?;
                let value = if *op == ast::AssignOp::Eq {
                    rhs
                } else {
                    let cur = self.expr(target, Some(pty))?.0;
                    let bop = match op {
                        ast::AssignOp::Add => BinOp::Add,
                        ast::AssignOp::Sub => BinOp::Sub,
                        ast::AssignOp::Mul => BinOp::Mul,
                        ast::AssignOp::Div => BinOp::Div,
                        ast::AssignOp::Rem => BinOp::Rem,
                        _ => return Err("unsupported compound assignment".into()),
                    };
                    Expr::Bin(bop, Box::new(cur), Box::new(rhs), pty)
                };
                self.push(Stmt::Assign { place, value });
            }
            ast::StmtKind::Expr(e) => {
                if let Some(()) = self.try_console(e)? {
                    return Ok(());
                }
                let (val, ty) = self.expr(e, None)?;
                // keep the call for its effect; a bare value is dropped
                if ty == TyTable::VOID {
                    self.push(Stmt::Expr(val));
                } else {
                    self.push(Stmt::Expr(val));
                }
            }
            ast::StmtKind::Return(v) => match v {
                None => self.push(Stmt::Return(None)),
                Some(e) => {
                    let (val, _) = self.expr(e, Some(self.ret))?;
                    self.push(Stmt::Return(Some(val)));
                }
            },
            ast::StmtKind::If { cond, then, els } => {
                let (c, _) = self.expr(cond, Some(TyTable::BOOL))?;
                let then_b = self.lower_body(then)?;
                let els_b = self.lower_body(els)?;
                self.push(Stmt::If {
                    cond: c,
                    then: then_b,
                    els: els_b,
                });
            }
            ast::StmtKind::While { cond, body } => {
                // loop { if (!cond) break; body }
                let (c, _) = self.expr(cond, Some(TyTable::BOOL))?;
                let mut inner = vec![Stmt::If {
                    cond: Expr::Not(Box::new(c)),
                    then: vec![Stmt::Break],
                    els: vec![],
                }];
                let body_b = self.lower_body(body)?;
                inner.extend(body_b);
                self.push(Stmt::Loop { body: inner });
            }
            ast::StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init.as_ref() {
                    self.stmt(init)?;
                }
                let cond_expr = match cond {
                    Some(c) => self.expr(c, Some(TyTable::BOOL))?.0,
                    None => Expr::Bool(true),
                };
                let mut inner = vec![Stmt::If {
                    cond: Expr::Not(Box::new(cond_expr)),
                    then: vec![Stmt::Break],
                    els: vec![],
                }];
                inner.extend(self.lower_body(body)?);
                if let Some(step) = step.as_ref() {
                    self.blocks.push(Vec::new());
                    self.stmt(step)?;
                    let step_b = self.blocks.pop().unwrap();
                    inner.extend(step_b);
                }
                self.push(Stmt::Loop { body: inner });
            }
            ast::StmtKind::ForEach { var, coll, body } => {
                self.lower_foreach(var, coll, body)?;
            }
            ast::StmtKind::Break => self.push(Stmt::Break),
            ast::StmtKind::Continue => self.push(Stmt::Continue),
            ast::StmtKind::Block(b) => {
                let body = self.lower_body(b)?;
                for s in body {
                    self.push(s);
                }
            }
            ast::StmtKind::Defer(_) => {
                return Err("`defer` is not yet lowered".into());
            }
        }
        Ok(())
    }

    /// `foreach (i in a..b)` — the only collection form supported yet.
    fn lower_foreach(
        &mut self,
        var: &str,
        coll: &ast::Expr,
        body: &[ast::Stmt],
    ) -> Result<(), String> {
        let ast::ExprKind::Range(lo, hi, inclusive) = &coll.kind else {
            return Err("foreach supports only integer ranges `a..b` for now".into());
        };
        let (lo_v, ity) = self.expr(lo, Some(TyTable::I32))?;
        let (hi_v, _) = self.expr(hi, Some(ity))?;
        let iv = self.new_local(var, ity);
        self.scope.insert(var.to_string(), (iv, ity));
        let hi_l = self.new_local("$end", ity);
        self.push(Stmt::Let {
            local: iv,
            value: lo_v,
        });
        self.push(Stmt::Let {
            local: hi_l,
            value: hi_v,
        });
        let cmp = if *inclusive { BinOp::Le } else { BinOp::Lt };
        let mut inner = vec![Stmt::If {
            cond: Expr::Not(Box::new(Expr::Bin(
                cmp,
                Box::new(Expr::Local(iv)),
                Box::new(Expr::Local(hi_l)),
                ity,
            ))),
            then: vec![Stmt::Break],
            els: vec![],
        }];
        inner.extend(self.lower_body(body)?);
        inner.push(Stmt::Assign {
            place: Place::Local(iv),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(iv)),
                Box::new(Expr::Int(1, ity)),
                ity,
            ),
        });
        self.push(Stmt::Loop { body: inner });
        Ok(())
    }

    // ─── Console.WriteLine / Write ─────────────────────────────────────────

    fn try_console(&mut self, e: &ast::Expr) -> Result<Option<()>, String> {
        let ast::ExprKind::Call(callee, args) = &e.kind else {
            return Ok(None);
        };
        let ast::ExprKind::Member(recv, method) = &callee.kind else {
            return Ok(None);
        };
        let ast::ExprKind::Ident(obj) = &recv.kind else {
            return Ok(None);
        };
        if obj != "Console" || (method != "WriteLine" && method != "Write") {
            return Ok(None);
        }
        let newline = method == "WriteLine";
        if args.is_empty() {
            if newline {
                self.emit_print_str("\n");
            }
            return Ok(Some(()));
        }
        // Interpolation: print each segment, then a newline.
        if let ast::ExprKind::Interp(segs) = &args[0].kind {
            for seg in segs {
                match seg {
                    ast::InterpSeg::Lit(s) => self.emit_print_str(s),
                    ast::InterpSeg::Expr(inner) => {
                        let (v, ty) = self.expr(inner, None)?;
                        self.emit_print_value(v, ty)?;
                    }
                }
            }
            if newline {
                self.emit_print_str("\n");
            }
            return Ok(Some(()));
        }
        let (v, ty) = self.expr(&args[0], None)?;
        if ty == TyTable::STR {
            self.emit_printf(if newline { "%s\n" } else { "%s" }, vec![(v, TyTable::STR)]);
        } else {
            self.emit_print_value(v, ty)?;
            if newline {
                self.emit_print_str("\n");
            }
        }
        Ok(Some(()))
    }

    fn emit_print_str(&mut self, s: &str) {
        self.emit_printf("%s", vec![(Expr::Str(s.to_string()), TyTable::STR)]);
    }

    fn emit_print_value(&mut self, v: Expr, ty: TyId) -> Result<(), String> {
        let (fmt, arg_ty, arg) = match self.tt().kind(ty) {
            TyKind::Str => ("%s", TyTable::STR, v),
            TyKind::F32 | TyKind::F64 => {
                // printf promotes float to double
                let arg = if ty == TyTable::F32 {
                    Expr::Cast {
                        value: Box::new(v),
                        to: TyTable::F64,
                    }
                } else {
                    v
                };
                ("%g", TyTable::F64, arg)
            }
            TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => ("%lld", TyTable::I64, v),
            TyKind::Bool => (
                "%d",
                TyTable::I32,
                Expr::Cast {
                    value: Box::new(v),
                    to: TyTable::I32,
                },
            ),
            TyKind::U8 | TyKind::U16 | TyKind::U32 => {
                ("%u", TyTable::U32, cast_to(v, ty, TyTable::U32))
            }
            _ => ("%d", TyTable::I32, cast_to(v, ty, TyTable::I32)),
        };
        self.emit_printf(fmt, vec![(arg, arg_ty)]);
        Ok(())
    }

    fn emit_printf(&mut self, fmt: &str, extra: Vec<(Expr, TyId)>) {
        // Only the format string is a fixed parameter of `printf`; every value
        // is variadic. Keeping `arg_tys` at just `[STR]` means one uniform
        // `declare i32 @printf(ptr, ...)` however printf is called.
        let mut args = vec![Expr::Str(fmt.to_string())];
        for (e, _) in extra {
            args.push(e);
        }
        self.push(Stmt::Expr(Expr::Call(Box::new(Call::Dll {
            library: "c".into(),
            symbol: "printf".into(),
            conv: CallConv::Cdecl,
            args,
            arg_tys: vec![TyTable::STR],
            ret: TyTable::I32,
            varargs: true,
        }))));
    }

    // ─── expressions ───────────────────────────────────────────────────────

    fn expr(&mut self, e: &ast::Expr, hint: Option<TyId>) -> Result<(Expr, TyId), String> {
        match &e.kind {
            ast::ExprKind::Int(v) => {
                let ty = hint
                    .filter(|t| is_int_ty(self.tt(), *t))
                    .unwrap_or(TyTable::I32);
                Ok((Expr::Int(*v, ty), ty))
            }
            ast::ExprKind::Float(v, f32) => {
                let ty = if *f32 {
                    TyTable::F32
                } else {
                    hint.filter(|t| self.tt().is_float(*t))
                        .unwrap_or(TyTable::F64)
                };
                Ok((Expr::Float(*v, ty), ty))
            }
            ast::ExprKind::Bool(b) => Ok((Expr::Bool(*b), TyTable::BOOL)),
            ast::ExprKind::Str(s) => Ok((Expr::Str(s.clone()), TyTable::STR)),
            ast::ExprKind::Char(c) => Ok((Expr::Int(*c as i128, TyTable::CHAR), TyTable::CHAR)),
            ast::ExprKind::Null => {
                let ty = hint.unwrap_or(TyTable::PTR);
                Ok((Expr::Null(ty), ty))
            }
            ast::ExprKind::Ident(name) => self.ident(name, e.span),
            ast::ExprKind::Member(recv, member) => self.member(recv, member),
            ast::ExprKind::Call(callee, args) => self.call(callee, args),
            ast::ExprKind::Index(base, idx) => {
                let (b, bty) = self.expr(base, None)?;
                let elem = match self.tt().kind(bty) {
                    TyKind::Array(e) => *e,
                    _ => return Err("indexing is only supported on List<T> for now".into()),
                };
                let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                // 1-based → the runtime helper expects 0-based; subtract 1.
                let i0 = Expr::Bin(
                    BinOp::Sub,
                    Box::new(i),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                );
                Ok((Expr::Index(Box::new(b), Box::new(i0)), elem))
            }
            ast::ExprKind::Unary(op, inner) => {
                let (v, ty) = self.expr(inner, hint)?;
                let out = match op {
                    ast::UnOp::Neg => Expr::Neg(Box::new(v), ty),
                    ast::UnOp::Not => Expr::Not(Box::new(v)),
                    ast::UnOp::BitNot => {
                        // ~x == x ^ -1
                        Expr::Bin(BinOp::Xor, Box::new(v), Box::new(Expr::Int(-1, ty)), ty)
                    }
                };
                let rty = if *op == ast::UnOp::Not {
                    TyTable::BOOL
                } else {
                    ty
                };
                Ok((out, rty))
            }
            ast::ExprKind::Binary(op, a, b) => self.binary(*op, a, b, hint),
            ast::ExprKind::Cast(t, inner) => {
                let to = self.cx.resolve(t)?;
                let (v, _) = self.expr(inner, None)?;
                Ok((
                    Expr::Cast {
                        value: Box::new(v),
                        to,
                    },
                    to,
                ))
            }
            ast::ExprKind::New(t, args, inits) => self.new_record(t, args, inits),
            ast::ExprKind::Ternary(c, a, b) => self.ternary(c, a, b, hint),
            ast::ExprKind::NullCoalesce(_, _) => Err("`??` is not yet lowered".into()),
            ast::ExprKind::Try(_) => Err("`?` propagation is not yet lowered".into()),
            ast::ExprKind::Range(_, _, _) => Err("a range is only valid in `foreach`".into()),
            ast::ExprKind::Interp(_) => {
                Err("string interpolation is only supported in Console.WriteLine for now".into())
            }
        }
    }

    fn ident(&mut self, name: &str, _span: LSpan) -> Result<(Expr, TyId), String> {
        if let Some((lid, ty)) = self.scope.get(name).copied() {
            return Ok((Expr::Local(lid), ty));
        }
        if let Some((ty, value)) = self.cx.consts.get(name).cloned() {
            return self.expr(&value, Some(ty));
        }
        // An unqualified name inside an instance method may be a field of `this`.
        if let Some((this_lid, this_ty)) = self.scope.get("this").copied() {
            if let TyKind::Record(rid) = *self.tt().kind(this_ty) {
                let rec = self.cx.b.m.record(rid);
                if let Some(idx) = rec.fields.iter().position(|f| f.name == name) {
                    let fty = rec.fields[idx].ty;
                    return Ok((Expr::Field(Box::new(Expr::Local(this_lid)), idx), fty));
                }
            }
        }
        Err(format!("unknown name `{name}`"))
    }

    fn member(&mut self, recv: &ast::Expr, member: &str) -> Result<(Expr, TyId), String> {
        // Enum member: Type.Member
        if let ast::ExprKind::Ident(obj) = &recv.kind {
            if let Some(members) = self.cx.enums.get(obj) {
                if let Some(v) = members.get(member) {
                    return Ok((Expr::Int(*v, TyTable::I32), TyTable::I32));
                }
            }
            if let Some((ty, value)) = self.cx.consts.get(&format!("{obj}.{member}")).cloned() {
                return self.expr(&value, Some(ty));
            }
        }
        // Field access on a record value, or `.Length` etc. (later).
        let (base, bty) = self.expr(recv, None)?;
        if let TyKind::Record(rid) = *self.tt().kind(bty) {
            let rec = self.cx.b.m.record(rid);
            if let Some(idx) = rec.fields.iter().position(|f| f.name == member) {
                let fty = rec.fields[idx].ty;
                return Ok((Expr::Field(Box::new(base), idx), fty));
            }
        }
        Err(format!("no member `{member}` on this value"))
    }

    fn call(&mut self, callee: &ast::Expr, args: &[ast::Expr]) -> Result<(Expr, TyId), String> {
        // Resolve a method: `Type.Method(..)` (static) or `recv.Method(..)`.
        let (key, this_arg): (String, Option<Expr>) = match &callee.kind {
            ast::ExprKind::Ident(name) => (name.clone(), None),
            ast::ExprKind::Member(recv, name) => {
                if let ast::ExprKind::Ident(obj) = &recv.kind {
                    if self.cx.methods.contains_key(&format!("{obj}.{name}"))
                        && self.is_type_name(obj)
                    {
                        (format!("{obj}.{name}"), None)
                    } else {
                        // instance call
                        let (recv_v, _) = self.expr(recv, None)?;
                        (name.clone(), Some(recv_v))
                    }
                } else {
                    let (recv_v, _) = self.expr(recv, None)?;
                    (name.clone(), Some(recv_v))
                }
            }
            _ => return Err("unsupported call target".into()),
        };
        let sig = self
            .cx
            .methods
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("unknown method `{key}`"))?;
        let mut kargs = Vec::new();
        if let Some(this) = this_arg {
            kargs.push(this);
        }
        for (a, pty) in args.iter().zip(sig.params.iter()) {
            kargs.push(self.expr(a, Some(*pty))?.0);
        }
        Ok((
            Expr::Call(Box::new(Call::Direct {
                func: sig.fid,
                args: kargs,
            })),
            sig.ret,
        ))
    }

    fn is_type_name(&self, name: &str) -> bool {
        self.cx.type_ids.contains_key(name)
            || matches!(
                name,
                "Console" | "Math" | "int" | "long" | "string" | "double"
            )
    }

    fn new_record(
        &mut self,
        t: &ast::TypeRef,
        args: &[ast::Expr],
        inits: &[(String, ast::Expr)],
    ) -> Result<(Expr, TyId), String> {
        let ty = self.cx.resolve(t)?;
        let TyKind::Record(rid) = *self.tt().kind(ty) else {
            return Err("`new` is only supported for records/classes for now".into());
        };
        let field_tys: Vec<(String, TyId)> = self
            .cx
            .b
            .m
            .record(rid)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty))
            .collect();
        let mut values: Vec<Expr> = Vec::with_capacity(field_tys.len());
        if !args.is_empty() {
            // positional
            for (a, (_, fty)) in args.iter().zip(field_tys.iter()) {
                values.push(self.expr(a, Some(*fty))?.0);
            }
        } else {
            // object initialiser: default missing to zero
            for (fname, fty) in &field_tys {
                if let Some((_, e)) = inits.iter().find(|(n, _)| n == fname) {
                    values.push(self.expr(e, Some(*fty))?.0);
                } else {
                    values.push(Expr::Int(0, *fty));
                }
            }
        }
        Ok((Expr::MakeRecord(rid, values), ty))
    }

    fn ternary(
        &mut self,
        c: &ast::Expr,
        a: &ast::Expr,
        b: &ast::Expr,
        hint: Option<TyId>,
    ) -> Result<(Expr, TyId), String> {
        let (cond, _) = self.expr(c, Some(TyTable::BOOL))?;
        let (av, aty) = self.expr(a, hint)?;
        let (bv, _) = self.expr(b, hint.or(Some(aty)))?;
        let t = self.new_local("$tern", aty);
        self.push(Stmt::If {
            cond,
            then: vec![Stmt::Assign {
                place: Place::Local(t),
                value: av,
            }],
            els: vec![Stmt::Assign {
                place: Place::Local(t),
                value: bv,
            }],
        });
        Ok((Expr::Local(t), aty))
    }

    fn binary(
        &mut self,
        op: ast::BinOp,
        a: &ast::Expr,
        b: &ast::Expr,
        hint: Option<TyId>,
    ) -> Result<(Expr, TyId), String> {
        // short-circuit && / ||
        if op == ast::BinOp::And || op == ast::BinOp::Or {
            let (av, _) = self.expr(a, Some(TyTable::BOOL))?;
            let t = self.new_local("$sc", TyTable::BOOL);
            self.push(Stmt::Let {
                local: t,
                value: av,
            });
            let (bv, _) = self.expr(b, Some(TyTable::BOOL))?;
            let assign = Stmt::Assign {
                place: Place::Local(t),
                value: bv,
            };
            if op == ast::BinOp::And {
                self.push(Stmt::If {
                    cond: Expr::Local(t),
                    then: vec![assign],
                    els: vec![],
                });
            } else {
                self.push(Stmt::If {
                    cond: Expr::Not(Box::new(Expr::Local(t))),
                    then: vec![assign],
                    els: vec![],
                });
            }
            return Ok((Expr::Local(t), TyTable::BOOL));
        }

        let kop = match op {
            ast::BinOp::Add => BinOp::Add,
            ast::BinOp::Sub => BinOp::Sub,
            ast::BinOp::Mul => BinOp::Mul,
            ast::BinOp::Div => BinOp::Div,
            ast::BinOp::Rem => BinOp::Rem,
            ast::BinOp::Eq => BinOp::Eq,
            ast::BinOp::Ne => BinOp::Ne,
            ast::BinOp::Lt => BinOp::Lt,
            ast::BinOp::Le => BinOp::Le,
            ast::BinOp::Gt => BinOp::Gt,
            ast::BinOp::Ge => BinOp::Ge,
            ast::BinOp::BitAnd => BinOp::And,
            ast::BinOp::BitOr => BinOp::Or,
            ast::BinOp::BitXor => BinOp::Xor,
            ast::BinOp::Shl => BinOp::Shl,
            ast::BinOp::Shr => BinOp::Shr,
            ast::BinOp::And | ast::BinOp::Or => unreachable!(),
        };
        let is_cmp = matches!(
            op,
            ast::BinOp::Eq
                | ast::BinOp::Ne
                | ast::BinOp::Lt
                | ast::BinOp::Le
                | ast::BinOp::Gt
                | ast::BinOp::Ge
        );
        // Operand type: prefer the hint for arithmetic; infer from operands for
        // comparisons.
        let (av, aty) = self.expr(a, if is_cmp { None } else { hint })?;
        let (bv, bty) = self.expr(b, Some(aty))?;
        let operand_ty = if aty != TyTable::BOOL { aty } else { bty };
        let out = Expr::Bin(kop, Box::new(av), Box::new(bv), operand_ty);
        let rty = if is_cmp { TyTable::BOOL } else { operand_ty };
        Ok((out, rty))
    }

    // ─── places ─────────────────────────────────────────────────────────────

    fn place(&mut self, e: &ast::Expr) -> Result<Place, String> {
        match &e.kind {
            ast::ExprKind::Ident(name) => {
                let (lid, _) = *self
                    .scope
                    .get(name)
                    .ok_or_else(|| format!("cannot assign to unknown `{name}`"))?;
                Ok(Place::Local(lid))
            }
            ast::ExprKind::Member(recv, member) => {
                let (base, bty) = self.expr(recv, None)?;
                if let TyKind::Record(rid) = *self.tt().kind(bty) {
                    let rec = self.cx.b.m.record(rid);
                    if let Some(idx) = rec.fields.iter().position(|f| f.name == *member) {
                        return Ok(Place::Field(Box::new(base), idx));
                    }
                }
                Err(format!("cannot assign to member `{member}`"))
            }
            ast::ExprKind::Index(base, idx) => {
                let (b, _) = self.expr(base, None)?;
                let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                let i0 = Expr::Bin(
                    BinOp::Sub,
                    Box::new(i),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                );
                Ok(Place::Index(Box::new(b), Box::new(i0)))
            }
            _ => Err("invalid assignment target".into()),
        }
    }

    fn place_ty(&mut self, e: &ast::Expr) -> Result<TyId, String> {
        Ok(self.expr(e, None)?.1)
    }
}

fn is_int_ty(tt: &TyTable, t: TyId) -> bool {
    matches!(
        tt.kind(t),
        TyKind::I8
            | TyKind::U8
            | TyKind::I16
            | TyKind::U16
            | TyKind::I32
            | TyKind::U32
            | TyKind::I64
            | TyKind::U64
            | TyKind::Nint
            | TyKind::Nuint
            | TyKind::Char
    )
}

fn cast_to(v: Expr, from: TyId, to: TyId) -> Expr {
    if from == to {
        v
    } else {
        Expr::Cast {
            value: Box::new(v),
            to,
        }
    }
}
