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

/// Field indices of a synthesised `Result` record.
const RESULT_OK: usize = 0;
const RESULT_VALUE: usize = 1;
const RESULT_ERR: usize = 2;

/// Field indices of a synthesised `List` record.
const LIST_LEN: usize = 0;
const LIST_CAP: usize = 1;
const LIST_DATA: usize = 2;

pub fn lower(p: &ast::Program) -> Result<Module, String> {
    let mut b = ModuleBuilder::new(
        p.namespace.as_deref().unwrap_or("program"),
        ModuleKind::Console,
        Target::X86_64_LINUX,
    );

    // Pass 1: reserve a RecordId + interned type for every declared type.
    let mut type_ids: HashMap<String, RecordId> = HashMap::new();
    let mut type_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in &p.items {
        match item {
            ast::Item::Type(td) => {
                type_names.insert(td.name.clone());
            }
            ast::Item::Enum(ed) => {
                type_names.insert(ed.name.clone());
            }
        }
    }
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
        type_names,
        generics: HashMap::new(),
        mono: HashMap::new(),
        tvars: HashMap::new(),
        results: HashMap::new(),
        lists: HashMap::new(),
        pending: Vec::new(),
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
                // A generic method is a template: nothing is emitted until a
                // call site fixes its type arguments.
                if !m.type_params.is_empty() {
                    let t = Template {
                        owner: td.name.clone(),
                        method: m.clone(),
                        this: this.is_some(),
                    };
                    cx.generics
                        .insert(format!("{}.{}", td.name, m.name), t.clone());
                    cx.generics.entry(m.name.clone()).or_insert(t);
                    continue;
                }
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

    // Pass 4: lower method bodies (generic templates are skipped — their
    // instances are lowered from the pending queue below).
    for item in &p.items {
        if let ast::Item::Type(td) = item {
            for m in &td.methods {
                if !m.type_params.is_empty() {
                    continue;
                }
                let sig = cx.methods[&format!("{}.{}", td.name, m.name)].clone();
                cx.lower_method(td, m, &sig)?;
            }
        }
    }
    // Top-level statements become Main.
    if !p.top_level.is_empty() {
        let main = cx.b.declare_func("kmain", vec![], TyTable::VOID);
        let mut fl = FnLower::new(&mut cx, main, TyTable::VOID);
        fl.captured = captured_names(&p.top_level, None);
        for s in &p.top_level {
            fl.stmt(s)?;
        }
        let body = fl.finish();
        cx.b.set_body(main, body);
        cx.b.set_entry(main);
    } else if let Some(sig) = cx.methods.get("Main").cloned() {
        cx.b.set_entry(sig.fid);
    }

    // Every generic instance and lifted lambda queued by *any* of the above —
    // method bodies and top-level code alike — gets its body here. This must
    // run after the last direct lowering, or an instance would be declared and
    // left empty.
    cx.drain_pending()?;

    Ok(cx.b.build())
}

#[derive(Clone)]
struct Sig {
    fid: FuncId,
    this: bool,
    params: Vec<TyId>,
    ret: TyId,
}

/// A generic method awaiting instantiation.
#[derive(Clone)]
struct Template {
    owner: String,
    method: ast::Method,
    this: bool,
}

/// A monomorphic instance whose body still has to be lowered. Body lowering is
/// deferred to the driver so it never runs inside another function's lowering
/// (which already holds the one mutable borrow of `Cx`).
struct Pending {
    fid: FuncId,
    method: ast::Method,
    tvars: HashMap<String, TyId>,
    this: bool,
    /// For a lifted lambda: the environment record reached through parameter 0,
    /// and the captured variables it holds, in field order.
    env: Option<EnvPlan>,
}

/// How a lifted lambda reaches the variables it captured.
#[derive(Clone)]
struct EnvPlan {
    /// Record of `Ptr` fields, one per captured variable.
    rec: RecordId,
    /// `(name, cell record, value type)` per field, in order.
    captures: Vec<(String, RecordId, TyId)>,
}

/// A captured variable: its value lives in a one-field heap record (a "cell"),
/// so the declaring function and every closure over it read and write the same
/// memory — capture by reference, as the spec requires.
#[derive(Clone)]
struct Cell {
    /// An expression yielding the cell pointer in the current function.
    ptr: Expr,
    rec: RecordId,
    ty: TyId,
}

struct Cx {
    b: ModuleBuilder,
    type_ids: HashMap<String, RecordId>,
    /// Every declared type name — records, classes, static classes and enums —
    /// so `Type.Member` can be told from `value.Member`.
    type_names: std::collections::HashSet<String>,
    enums: HashMap<String, HashMap<String, i128>>,
    consts: HashMap<String, (TyId, ast::Expr)>,
    methods: HashMap<String, Sig>,
    /// Generic method templates, by `Type.Name` and by bare `Name`.
    generics: HashMap<String, Template>,
    /// Instantiation cache: (template key, concrete type arguments) → instance.
    mono: HashMap<(String, Vec<TyId>), Sig>,
    /// Type parameters bound while lowering one instance.
    tvars: HashMap<String, TyId>,
    /// `Result<T>` is synthesised per value type: a record `{ok, value, err}`.
    results: HashMap<TyId, RecordId>,
    /// `List<T>` is synthesised per element type: `{len, cap, data}`.
    lists: HashMap<TyId, RecordId>,
    pending: Vec<Pending>,
}

impl Cx {
    fn record_ty(&mut self, name: &str) -> TyId {
        let rid = self.type_ids[name];
        self.b.m.types.intern(TyKind::Record(rid))
    }

    fn resolve(&mut self, t: &ast::TypeRef) -> Result<TyId, String> {
        // A bound type parameter wins: inside a monomorphic instance, `T` is
        // whatever this instantiation bound it to.
        if let ast::TypeRef::Named(n) = t {
            if let Some(&bound) = self.tvars.get(n.as_str()) {
                return Ok(bound);
            }
        }
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
                    let rid = self.list_record(e);
                    self.b.m.types.intern(TyKind::Record(rid))
                }
                // `Func<A, B, R>`: the last argument is the return type.
                "Func" => {
                    let mut tys = Vec::new();
                    for a in args {
                        tys.push(self.resolve(a)?);
                    }
                    let ret = tys.pop().unwrap_or(TyTable::VOID);
                    self.b.m.types.intern(TyKind::Func { params: tys, ret })
                }
                // `Result<T>`: a record {ok, value, err}, synthesised per T.
                "Result" => {
                    let vt = match args.first() {
                        Some(a) => self.resolve(a)?,
                        None => TyTable::I32,
                    };
                    let rid = self.result_record(vt);
                    self.b.m.types.intern(TyKind::Record(rid))
                }
                // `Action<A, B>`: no return.
                "Action" => {
                    let mut tys = Vec::new();
                    for a in args {
                        tys.push(self.resolve(a)?);
                    }
                    self.b.m.types.intern(TyKind::Func {
                        params: tys,
                        ret: TyTable::VOID,
                    })
                }
                _ => return Err(format!("generic type `{n}` not yet supported")),
            },
        })
    }

    /// The record standing for `Result<T>`: `{ok: bool, value: T, err: string}`.
    /// Field indices are fixed so lowering can reach them positionally.
    fn result_record(&mut self, value_ty: TyId) -> RecordId {
        if let Some(r) = self.results.get(&value_ty) {
            return *r;
        }
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$Result{n}"),
            vec![
                ("ok", TyTable::BOOL),
                ("value", value_ty),
                ("err", TyTable::STR),
            ],
            Equality::ByValue,
        );
        self.results.insert(value_ty, rid);
        rid
    }

    /// The record standing for `List<T>`: `{len, cap, data}` where `data` is a
    /// contiguous buffer of `T`.
    fn list_record(&mut self, elem: TyId) -> RecordId {
        if let Some(r) = self.lists.get(&elem) {
            return *r;
        }
        let data_ty = self.b.m.types.intern(TyKind::Array(elem));
        let n = self.b.m.records.len();
        let rid = self.b.c_record(
            &format!("$List{n}"),
            vec![
                ("len", TyTable::I32),
                ("cap", TyTable::I32),
                ("data", data_ty),
            ],
            Equality::ByRef,
        );
        self.lists.insert(elem, rid);
        rid
    }

    /// If `ty` is a synthesised `List`, its record and element type.
    fn as_list(&self, ty: TyId) -> Option<(RecordId, TyId)> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            if self.b.m.record(rid).name.starts_with("$List") {
                let data = self.b.m.record(rid).fields[LIST_DATA].ty;
                if let TyKind::Array(e) = *self.b.m.types.kind(data) {
                    return Some((rid, e));
                }
            }
        }
        None
    }

    /// If `ty` is a synthesised `Result`, its record and value type.
    fn as_result(&self, ty: TyId) -> Option<(RecordId, TyId)> {
        if let TyKind::Record(rid) = *self.b.m.types.kind(ty) {
            if self.b.m.record(rid).name.starts_with("$Result") {
                let vt = self.b.m.record(rid).fields[RESULT_VALUE].ty;
                return Some((rid, vt));
            }
        }
        None
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

    pub(crate) fn scalar_size(&self, ty: TyId) -> i64 {
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
        let _ = td;
        self.lower_into(sig.fid, m, sig.this, None)
    }

    /// Lower a method body into an already-declared function. Parameter types
    /// come from the declared function, so this serves plain methods and
    /// monomorphic instances alike.
    fn lower_into(
        &mut self,
        fid: FuncId,
        m: &ast::Method,
        this: bool,
        env: Option<EnvPlan>,
    ) -> Result<(), String> {
        let ret = self.b.m.func(fid).ret;
        let ptys: Vec<TyId> = self.b.m.func(fid).params.iter().map(|p| p.ty).collect();
        let mut fl = FnLower::new(self, fid, ret);
        // Names this body's lambdas close over; such locals live in cells.
        fl.captured = captured_names(&m.body, m.expr_body.as_ref());
        if this {
            fl.scope.insert("this".into(), (LocalId(0), ptys[0]));
        }
        let base = if this { 1 } else { 0 };
        for (i, p) in m.params.iter().enumerate() {
            fl.scope
                .insert(p.name.clone(), (LocalId((base + i) as u32), ptys[base + i]));
        }
        // A lifted lambda reaches its captures through the environment record
        // in parameter 0: `env.field[i]` is the cell pointer for capture `i`.
        if let Some(plan) = env {
            let env_ty = fl.cx.b.m.types.intern(TyKind::Record(plan.rec));
            let env_val = Expr::Cast {
                value: Box::new(Expr::Local(LocalId(0))),
                to: env_ty,
            };
            for (i, (name, cell_rec, vty)) in plan.captures.iter().enumerate() {
                let cell_ty = fl.cx.b.m.types.intern(TyKind::Record(*cell_rec));
                let ptr = Expr::Cast {
                    value: Box::new(Expr::Field(Box::new(env_val.clone()), i)),
                    to: cell_ty,
                };
                fl.cells.insert(
                    name.clone(),
                    Cell {
                        ptr,
                        rec: *cell_rec,
                        ty: *vty,
                    },
                );
            }
        }
        // A parameter a lambda closes over has to move into a cell, so the
        // closure and the body share one location rather than two copies.
        let captured_params: Vec<(String, LocalId, TyId)> = m
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| fl.captured.contains(&p.name))
            .map(|(i, p)| (p.name.clone(), LocalId((base + i) as u32), ptys[base + i]))
            .collect();
        for (name, lid, ty) in captured_params {
            fl.scope.remove(&name);
            fl.make_cell(&name, ty, Expr::Local(lid))?;
        }
        if let Some(e) = &m.expr_body {
            let (val, _) = fl.expr(e, Some(ret))?;
            fl.push(Stmt::Return(Some(val)));
        } else {
            for s in &m.body {
                fl.stmt(s)?;
            }
        }
        let body = fl.finish();
        self.b.set_body(fid, body);
        Ok(())
    }

    /// Lower every queued monomorphic instance. Lowering one may queue more
    /// (a generic calling another generic), so this runs to a fixed point.
    fn drain_pending(&mut self) -> Result<(), String> {
        while let Some(p) = self.pending.pop() {
            let saved = std::mem::replace(&mut self.tvars, p.tvars);
            let r = self.lower_into(p.fid, &p.method, p.this, p.env);
            self.tvars = saved;
            r?;
        }
        Ok(())
    }
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
    /// Captured variables, by name: their value lives in a heap cell.
    cells: HashMap<String, Cell>,
    /// Names the lambdas in this body close over (an over-approximation: a
    /// local with one of these names is celled whether or not it is really
    /// captured, which is safe and costs one indirection).
    captured: std::collections::HashSet<String>,
    blocks: Vec<Vec<Stmt>>,
    /// Deferred work, one frame per lexical block; each frame holds one group
    /// per `defer` (a single `defer` can lower to several statements). KIR has
    /// no `defer`, so a frame is copied to every edge that leaves its block:
    /// the groups unwind in reverse, the statements inside a group do not.
    defers: Vec<Vec<Vec<Stmt>>>,
    /// `defers.len()` on entry to each enclosing loop body, so `break` and
    /// `continue` know which frames they are leaving.
    loop_frames: Vec<usize>,
    /// The step of each enclosing loop (a `for`'s increment, a `foreach`'s
    /// counter bump). `continue` must run it, or the loop never advances.
    loop_steps: Vec<Vec<Stmt>>,
    /// `T?` locals proven non-null by an enclosing `if`, so reading one yields
    /// the `T` rather than the optional.
    narrowed: std::collections::HashSet<String>,
}

impl<'a> FnLower<'a> {
    fn new(cx: &'a mut Cx, fid: FuncId, ret: TyId) -> FnLower<'a> {
        FnLower {
            cx,
            fid,
            ret,
            scope: HashMap::new(),
            cells: HashMap::new(),
            captured: std::collections::HashSet::new(),
            blocks: vec![Vec::new()],
            defers: vec![Vec::new()],
            loop_frames: Vec::new(),
            loop_steps: Vec::new(),
            narrowed: std::collections::HashSet::new(),
        }
    }

    /// Copy deferred statements from the innermost frame down to `floor`,
    /// innermost first and in reverse declaration order — the unwinding order.
    fn emit_defers_to(&mut self, floor: usize) {
        let mut out = Vec::new();
        for frame in self.defers[floor..].iter().rev() {
            for group in frame.iter().rev() {
                out.extend(group.iter().cloned());
            }
        }
        for s in out {
            self.push(s);
        }
    }

    fn push(&mut self, s: Stmt) {
        self.blocks.last_mut().unwrap().push(s);
    }

    fn finish(mut self) -> Vec<Stmt> {
        self.emit_defers_to(0);
        self.blocks.pop().unwrap()
    }

    /// Move a value into a fresh heap cell and register `name` as captured.
    /// The cell is a one-field record, so the declaring function and every
    /// closure over it read and write the same memory.
    fn make_cell(&mut self, name: &str, ty: TyId, init: Expr) -> Result<(), String> {
        let n = self.cx.b.m.records.len();
        let rec = self
            .cx
            .b
            .c_record(&format!("$cell{n}"), vec![("v", ty)], Equality::ByRef);
        let cell_ty = self.cx.b.m.types.intern(TyKind::Record(rec));
        let lid = self
            .cx
            .b
            .add_local(self.fid, &format!("${name}$cell"), cell_ty);
        self.push(Stmt::Let {
            local: lid,
            value: Expr::MakeRecord(rec, vec![init]),
        });
        self.cells.insert(
            name.to_string(),
            Cell {
                ptr: Expr::Local(lid),
                rec,
                ty,
            },
        );
        Ok(())
    }

    /// Declare a variable, in a cell when a lambda closes over its name.
    fn declare_var(&mut self, name: &str, ty: TyId, init: Expr) -> Result<(), String> {
        if self.captured.contains(name) {
            return self.make_cell(name, ty, init);
        }
        let lid = self.new_local(name, ty);
        self.scope.insert(name.to_string(), (lid, ty));
        self.push(Stmt::Let {
            local: lid,
            value: init,
        });
        Ok(())
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
        self.defers.push(Vec::new());
        for s in stmts {
            self.stmt(s)?;
        }
        // Leaving the block by falling off its end also runs its defers.
        let floor = self.defers.len() - 1;
        self.emit_defers_to(floor);
        self.defers.pop();
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
                self.declare_var(name, lty, val)?;
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
                None => {
                    self.emit_defers_to(0);
                    self.push(Stmt::Return(None))
                }
                Some(e) => {
                    let (val, vty) = self.expr(e, Some(self.ret))?;
                    // `return x;` from a Result-returning method is `Ok(x)`
                    // unless the value already is a Result.
                    let val = match self.cx.as_result(self.ret) {
                        Some((rid, _)) if vty != self.ret => Expr::MakeRecord(
                            rid,
                            vec![Expr::Bool(true), val, Expr::Str(String::new())],
                        ),
                        _ => val,
                    };
                    self.emit_defers_to(0);
                    self.push(Stmt::Return(Some(val)));
                }
            },
            ast::StmtKind::If { cond, then, els } => {
                let (c, _) = self.expr(cond, Some(TyTable::BOOL))?;
                // `if (x != null)` proves `x` present in the then-branch;
                // `if (x == null)` proves it in the else-branch.
                let (narrow_then, narrow_else) = null_test_target(cond);
                let saved = self.narrowed.clone();
                if let Some(n) = &narrow_then {
                    self.narrowed.insert(n.clone());
                }
                let then_b = self.lower_body(then);
                self.narrowed = saved.clone();
                if let Some(n) = &narrow_else {
                    self.narrowed.insert(n.clone());
                }
                let els_b = self.lower_body(els);
                self.narrowed = saved;
                let (then_b, els_b) = (then_b?, els_b?);
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
                self.loop_frames.push(self.defers.len());
                self.loop_steps.push(Vec::new());
                let body_b = self.lower_body(body);
                self.loop_steps.pop();
                self.loop_frames.pop();
                inner.extend(body_b?);
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
                let step_b = match step.as_ref() {
                    Some(step) => {
                        self.blocks.push(Vec::new());
                        self.stmt(step)?;
                        self.blocks.pop().unwrap()
                    }
                    None => Vec::new(),
                };
                self.loop_frames.push(self.defers.len());
                self.loop_steps.push(step_b.clone());
                let body_b = self.lower_body(body);
                self.loop_steps.pop();
                self.loop_frames.pop();
                inner.extend(body_b?);
                inner.extend(step_b);
                self.push(Stmt::Loop { body: inner });
            }
            ast::StmtKind::ForEach { var, coll, body } => {
                self.lower_foreach(var, coll, body)?;
            }
            ast::StmtKind::Break => {
                let floor = self.loop_frames.last().copied().unwrap_or(0);
                self.emit_defers_to(floor);
                self.push(Stmt::Break)
            }
            ast::StmtKind::Continue => {
                let floor = self.loop_frames.last().copied().unwrap_or(0);
                self.emit_defers_to(floor);
                // The step runs on `continue` too — otherwise a counted loop
                // that continues never advances.
                if let Some(step) = self.loop_steps.last().cloned() {
                    for st in step {
                        self.push(st);
                    }
                }
                self.push(Stmt::Continue)
            }
            ast::StmtKind::Block(b) => {
                let body = self.lower_body(b)?;
                for s in body {
                    self.push(s);
                }
            }
            ast::StmtKind::Defer(inner) => {
                self.blocks.push(Vec::new());
                self.stmt(inner)?;
                let lowered = self.blocks.pop().unwrap();
                self.defers.last_mut().unwrap().push(lowered);
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
        // `foreach (x in list)` walks positions 1..Count.
        if !matches!(coll.kind, ast::ExprKind::Range(..)) {
            let (lv, lty) = self.expr(coll, None)?;
            if let Some((_, elem)) = self.cx.as_list(lty) {
                let holder = self.new_local("$each", lty);
                self.push(Stmt::Let {
                    local: holder,
                    value: lv,
                });
                let idx = self.new_local("$i", TyTable::I32);
                self.push(Stmt::Let {
                    local: idx,
                    value: Expr::Int(0, TyTable::I32),
                });
                let item = self.new_local(var, elem);
                self.scope.insert(var.to_string(), (item, elem));
                let len = Expr::Field(Box::new(Expr::Local(holder)), LIST_LEN);
                let mut inner = vec![
                    Stmt::If {
                        cond: Expr::Not(Box::new(Expr::Bin(
                            BinOp::Lt,
                            Box::new(Expr::Local(idx)),
                            Box::new(len),
                            TyTable::I32,
                        ))),
                        then: vec![Stmt::Break],
                        els: vec![],
                    },
                    Stmt::Assign {
                        place: Place::Local(item),
                        value: Expr::Index(
                            Box::new(Expr::Field(Box::new(Expr::Local(holder)), LIST_DATA)),
                            Box::new(Expr::Local(idx)),
                        ),
                    },
                ];
                let step = vec![Stmt::Assign {
                    place: Place::Local(idx),
                    value: Expr::Bin(
                        BinOp::Add,
                        Box::new(Expr::Local(idx)),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    ),
                }];
                self.loop_frames.push(self.defers.len());
                self.loop_steps.push(step.clone());
                let body_b = self.lower_body(body);
                self.loop_steps.pop();
                self.loop_frames.pop();
                inner.extend(body_b?);
                inner.extend(step);
                self.push(Stmt::Loop { body: inner });
                return Ok(());
            }
            return Err("foreach supports integer ranges and List<T>".into());
        }
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
        let step = vec![Stmt::Assign {
            place: Place::Local(iv),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(iv)),
                Box::new(Expr::Int(1, ity)),
                ity,
            ),
        }];
        self.loop_frames.push(self.defers.len());
        self.loop_steps.push(step.clone());
        let body_b = self.lower_body(body);
        self.loop_steps.pop();
        self.loop_frames.pop();
        inner.extend(body_b?);
        inner.extend(step);
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

    /// The printf/snprintf conversion for a value, with any promotion applied.
    fn fmt_arg(&mut self, v: Expr, ty: TyId) -> (&'static str, Expr) {
        match self.tt().kind(ty) {
            TyKind::Str => ("%s", v),
            TyKind::F32 => (
                "%g",
                Expr::Cast {
                    value: Box::new(v),
                    to: TyTable::F64,
                },
            ),
            TyKind::F64 => ("%g", v),
            TyKind::I64 | TyKind::U64 | TyKind::Nint | TyKind::Nuint => ("%lld", v),
            TyKind::Bool => (
                "%d",
                Expr::Cast {
                    value: Box::new(v),
                    to: TyTable::I32,
                },
            ),
            TyKind::U8 | TyKind::U16 | TyKind::U32 => ("%u", cast_to(v, ty, TyTable::U32)),
            _ => ("%d", cast_to(v, ty, TyTable::I32)),
        }
    }

    /// Build a `string` from literal chunks and values, with libc: `snprintf`
    /// measures it, `malloc` allocates, `snprintf` fills. This is what makes
    /// `$"..."` usable as a value and `string + string` work; it is replaced by
    /// the Kiln text runtime when the standard library lands.
    fn build_string(&mut self, parts: Vec<(String, Option<(Expr, TyId)>)>) -> (Expr, TyId) {
        let mut fmt = String::new();
        let mut args: Vec<Expr> = Vec::new();
        for (lit, val) in parts {
            for ch in lit.chars() {
                if ch == '%' {
                    fmt.push_str("%%");
                } else {
                    fmt.push(ch);
                }
            }
            if let Some((v, ty)) = val {
                let (spec, arg) = self.fmt_arg(v, ty);
                fmt.push_str(spec);
                args.push(arg);
            }
        }
        // Values are evaluated once, into locals, then used by both snprintf calls.
        let mut held = Vec::new();
        for a in args {
            let ty = self.infer_arg_ty(&a);
            let l = self.new_local("$fmtarg", ty);
            self.push(Stmt::Let { local: l, value: a });
            held.push(Expr::Local(l));
        }
        let snprintf = |args: Vec<Expr>| {
            Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "snprintf".into(),
                conv: CallConv::Cdecl,
                args,
                arg_tys: vec![TyTable::STR, TyTable::I64, TyTable::STR],
                ret: TyTable::I32,
                varargs: true,
            }))
        };
        // n = snprintf(null, 0, fmt, ...)
        let mut measure = vec![
            Expr::Null(TyTable::STR),
            Expr::Int(0, TyTable::I64),
            Expr::Str(fmt.clone()),
        ];
        measure.extend(held.iter().cloned());
        let n = self.new_local("$len", TyTable::I32);
        self.push(Stmt::Let {
            local: n,
            value: snprintf(measure),
        });
        // buf = malloc(n + 1)
        let size = Expr::Cast {
            value: Box::new(Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(n)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            )),
            to: TyTable::I64,
        };
        let buf = self.new_local("$buf", TyTable::STR);
        self.push(Stmt::Let {
            local: buf,
            value: Expr::Call(Box::new(Call::Dll {
                library: "c".into(),
                symbol: "malloc".into(),
                conv: CallConv::Cdecl,
                args: vec![size],
                arg_tys: vec![TyTable::I64],
                ret: TyTable::STR,
                varargs: false,
            })),
        });
        // snprintf(buf, n + 1, fmt, ...)
        let mut fill = vec![
            Expr::Local(buf),
            Expr::Cast {
                value: Box::new(Expr::Bin(
                    BinOp::Add,
                    Box::new(Expr::Local(n)),
                    Box::new(Expr::Int(1, TyTable::I32)),
                    TyTable::I32,
                )),
                to: TyTable::I64,
            },
            Expr::Str(fmt),
        ];
        fill.extend(held);
        self.push(Stmt::Expr(snprintf(fill)));
        (Expr::Local(buf), TyTable::STR)
    }

    /// The type a already-lowered format argument carries.
    fn infer_arg_ty(&self, e: &Expr) -> TyId {
        match e {
            Expr::Cast { to, .. } => *to,
            Expr::Local(l) => self.cx.b.m.func(self.fid).locals[l.0 as usize].ty,
            Expr::Str(_) => TyTable::STR,
            Expr::Int(_, t) | Expr::Float(_, t) => *t,
            Expr::Bool(_) => TyTable::BOOL,
            Expr::Field(..) | Expr::Call(..) | Expr::Bin(..) => TyTable::I32,
            _ => TyTable::I32,
        }
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

    /// Lower an expression, wrapping a plain `T` when a `T?` is expected.
    fn expr(&mut self, e: &ast::Expr, hint: Option<TyId>) -> Result<(Expr, TyId), String> {
        let (v, ty) = self.expr_raw(e, hint)?;
        if let Some(want) = hint {
            if ty != want {
                if let TyKind::Optional(inner) = *self.tt().kind(want) {
                    if ty == inner {
                        return Ok((Expr::MakeOptional(inner, Some(Box::new(v))), want));
                    }
                }
            }
        }
        Ok((v, ty))
    }

    fn expr_raw(&mut self, e: &ast::Expr, hint: Option<TyId>) -> Result<(Expr, TyId), String> {
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
                if let TyKind::Optional(inner) = *self.tt().kind(ty) {
                    return Ok((Expr::MakeOptional(inner, None), ty));
                }
                Ok((Expr::Null(ty), ty))
            }
            ast::ExprKind::Ident(name) => self.ident(name, e.span),
            ast::ExprKind::Member(recv, member) => self.member(recv, member),
            ast::ExprKind::Call(callee, args) => {
                // `Error("...")` builds a failed Result of the expected type.
                if let ast::ExprKind::Ident(n) = &callee.kind {
                    if n == "Error" && !self.cx.methods.contains_key("Error") {
                        let want = hint
                            .or(Some(self.ret))
                            .and_then(|t| self.cx.as_result(t))
                            .ok_or_else(|| {
                                "`Error(...)` needs a `Result` target — use it in a `return` \
                                 from a Result-returning method, or assign it to a Result"
                                    .to_string()
                            })?;
                        let (rid, vt) = want;
                        let msg = match args.first() {
                            Some(a) => self.expr(a, Some(TyTable::STR))?.0,
                            None => Expr::Str(String::new()),
                        };
                        let ty = self.cx.b.m.types.intern(TyKind::Record(rid));
                        return Ok((
                            Expr::MakeRecord(
                                rid,
                                vec![Expr::Bool(false), zero_of(self.tt(), vt), msg],
                            ),
                            ty,
                        ));
                    }
                }
                self.call(callee, args)
            }
            ast::ExprKind::Index(base, idx) => {
                let (b, bty) = self.expr(base, None)?;
                // `list[i]` reads the buffer; positions are 1-based.
                if let Some((_, elem)) = self.cx.as_list(bty) {
                    let (i, _) = self.expr(idx, Some(TyTable::I32))?;
                    let i0 = Expr::Bin(
                        BinOp::Sub,
                        Box::new(i),
                        Box::new(Expr::Int(1, TyTable::I32)),
                        TyTable::I32,
                    );
                    return Ok((
                        Expr::Index(Box::new(Expr::Field(Box::new(b), LIST_DATA)), Box::new(i0)),
                        elem,
                    ));
                }
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
                let uhint = hint.map(|h| match *self.tt().kind(h) {
                    TyKind::Optional(i) => i,
                    _ => h,
                });
                let (v, ty) = self.expr(inner, uhint)?;
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
            ast::ExprKind::Binary(op, a, b) => {
                // A null comparison on an optional is a presence test.
                if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
                    let (opt, _other) = match (&a.kind, &b.kind) {
                        (ast::ExprKind::Null, _) => (Some(b), true),
                        (_, ast::ExprKind::Null) => (Some(a), true),
                        _ => (None, false),
                    };
                    if let Some(x) = opt {
                        let (v, vty) = self.expr_raw(x, None)?;
                        if let TyKind::Optional(_) = *self.tt().kind(vty) {
                            let has = Expr::OptionalHasValue(Box::new(v));
                            let out = if *op == ast::BinOp::Ne {
                                has
                            } else {
                                Expr::Not(Box::new(has))
                            };
                            return Ok((out, TyTable::BOOL));
                        }
                    }
                }
                self.binary(*op, a, b, hint)
            }
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
            ast::ExprKind::NullCoalesce(a, b) => {
                let (v, vty) = self.expr_raw(a, None)?;
                // `T? ?? fallback`
                if let TyKind::Optional(inner) = *self.tt().kind(vty) {
                    let held = self.new_local("$coalesce", vty);
                    self.push(Stmt::Let {
                        local: held,
                        value: v,
                    });
                    let out = self.new_local("$value", inner);
                    let (fb, _) = self.expr(b, Some(inner))?;
                    self.push(Stmt::If {
                        cond: Expr::OptionalHasValue(Box::new(Expr::Local(held))),
                        then: vec![Stmt::Assign {
                            place: Place::Local(out),
                            value: Expr::OptionalGet(Box::new(Expr::Local(held))),
                        }],
                        els: vec![Stmt::Assign {
                            place: Place::Local(out),
                            value: fb,
                        }],
                    });
                    return Ok((Expr::Local(out), inner));
                }
                let (_, val_ty) = self
                    .cx
                    .as_result(vty)
                    .ok_or_else(|| "`??` applies to a `Result` or a `T?`".to_string())?;
                let held = self.new_local("$coalesce", vty);
                self.push(Stmt::Let {
                    local: held,
                    value: v,
                });
                let out = self.new_local("$value", val_ty);
                let (fb, _) = self.expr(b, Some(val_ty))?;
                self.push(Stmt::If {
                    cond: Expr::Field(Box::new(Expr::Local(held)), RESULT_OK),
                    then: vec![Stmt::Assign {
                        place: Place::Local(out),
                        value: Expr::Field(Box::new(Expr::Local(held)), RESULT_VALUE),
                    }],
                    els: vec![Stmt::Assign {
                        place: Place::Local(out),
                        value: fb,
                    }],
                });
                Ok((Expr::Local(out), val_ty))
            }
            ast::ExprKind::Try(inner) => {
                let (v, vty) = self.expr(inner, None)?;
                let (_, val_ty) = self
                    .cx
                    .as_result(vty)
                    .ok_or_else(|| "`?` can only be applied to a `Result`".to_string())?;
                let (out_rid, out_vt) = self.cx.as_result(self.ret).ok_or_else(|| {
                    "`?` needs the enclosing method to return a `Result`".to_string()
                })?;
                // Hold the result once, then test it.
                let t = self.new_local("$try", vty);
                self.push(Stmt::Let { local: t, value: v });
                let failed = Expr::Not(Box::new(Expr::Field(Box::new(Expr::Local(t)), RESULT_OK)));
                let propagated = Expr::MakeRecord(
                    out_rid,
                    vec![
                        Expr::Bool(false),
                        zero_of(self.tt(), out_vt),
                        Expr::Field(Box::new(Expr::Local(t)), RESULT_ERR),
                    ],
                );
                self.push(Stmt::If {
                    cond: failed,
                    then: vec![Stmt::Return(Some(propagated))],
                    els: vec![],
                });
                Ok((Expr::Field(Box::new(Expr::Local(t)), RESULT_VALUE), val_ty))
            }
            ast::ExprKind::Range(_, _, _) => Err("a range is only valid in `foreach`".into()),
            ast::ExprKind::Interp(segs) => {
                let mut parts: Vec<(String, Option<(Expr, TyId)>)> = Vec::new();
                for seg in segs {
                    match seg {
                        ast::InterpSeg::Lit(l) => parts.push((l.clone(), None)),
                        ast::InterpSeg::Expr(x) => {
                            let (v, t) = self.expr(x, None)?;
                            parts.push((String::new(), Some((v, t))));
                        }
                    }
                }
                Ok(self.build_string(parts))
            }
            ast::ExprKind::Switch(subject, arms) => self.switch(subject, arms, hint),
            ast::ExprKind::Lambda(l) => {
                let want = hint.ok_or_else(|| {
                    "a lambda needs a target type — assign it to a `Func<...>`/`Action<...>` \
                     local or pass it to a parameter of that type"
                        .to_string()
                })?;
                self.lambda(l, want)
            }
        }
    }

    fn ident(&mut self, name: &str, _span: LSpan) -> Result<(Expr, TyId), String> {
        if let Some(c) = self.cells.get(name).cloned() {
            return Ok((Expr::Field(Box::new(c.ptr), 0), c.ty));
        }
        if let Some((lid, ty)) = self.scope.get(name).copied() {
            if let TyKind::Optional(inner) = *self.tt().kind(ty) {
                if self.narrowed.contains(name) {
                    return Ok((Expr::OptionalGet(Box::new(Expr::Local(lid))), inner));
                }
            }
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
        // A list exposes its length.
        if self.cx.as_list(bty).is_some() {
            return match member {
                "Count" => Ok((Expr::Field(Box::new(base), LIST_LEN), TyTable::I32)),
                other => Err(format!("no member `{other}` on a List")),
            };
        }
        // An optional exposes presence and value explicitly.
        if let TyKind::Optional(inner) = *self.tt().kind(bty) {
            return match member {
                "HasValue" => Ok((Expr::OptionalHasValue(Box::new(base)), TyTable::BOOL)),
                "Value" => Ok((Expr::OptionalGet(Box::new(base)), inner)),
                other => Err(format!(
                    "no member `{other}` on a `T?` — test it against null, or use .Value"
                )),
            };
        }
        // A synthesised Result exposes named members rather than raw fields.
        if let Some((_, vt)) = self.cx.as_result(bty) {
            return match member {
                "IsOk" => Ok((Expr::Field(Box::new(base), RESULT_OK), TyTable::BOOL)),
                "IsErr" => Ok((
                    Expr::Not(Box::new(Expr::Field(Box::new(base), RESULT_OK))),
                    TyTable::BOOL,
                )),
                "Value" => Ok((Expr::Field(Box::new(base), RESULT_VALUE), vt)),
                "Error" => Ok((Expr::Field(Box::new(base), RESULT_ERR), TyTable::STR)),
                other => Err(format!("no member `{other}` on a Result")),
            };
        }
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
        // Calling a value of function type: an indirect call through its
        // `{fn, env}` pair.
        if let ast::ExprKind::Ident(name) = &callee.kind {
            if let Some((lid, ty)) = self.scope.get(name).copied() {
                if let TyKind::Func { params, ret } = self.tt().kind(ty).clone() {
                    if args.len() != params.len() {
                        return Err(format!(
                            "`{name}` takes {} argument(s), got {}",
                            params.len(),
                            args.len()
                        ));
                    }
                    let mut kargs = Vec::new();
                    for (a, pty) in args.iter().zip(params.iter()) {
                        kargs.push(self.expr(a, Some(*pty))?.0);
                    }
                    return Ok((
                        Expr::Call(Box::new(Call::Indirect {
                            callee: Box::new(Expr::Local(lid)),
                            args: kargs,
                            sig: ty,
                        })),
                        ret,
                    ));
                }
            }
        }
        // `list.Add(x)` — grow the buffer if it is full, then store.
        if let ast::ExprKind::Member(recv, name) = &callee.kind {
            if name == "Add" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if self.cx.as_list(lty).is_some() {
                    if args.len() != 1 {
                        return Err("List.Add takes one argument".into());
                    }
                    let (_, elem) = self.cx.as_list(lty).unwrap();
                    let (val, _) = self.expr(&args[0], Some(elem))?;
                    let holder = self.new_local("$list", lty);
                    self.push(Stmt::Let {
                        local: holder,
                        value: lv,
                    });
                    self.list_add(holder, lty, val);
                    return Ok((Expr::Int(0, TyTable::VOID), TyTable::VOID));
                }
            }
            // `list.Where(pred)` / `list.Select(f)` — written here rather than
            // in K2 until the standard library exists.
            if name == "Where" || name == "Select" {
                let (lv, lty) = self.expr_raw(recv, None)?;
                if self.cx.as_list(lty).is_some() {
                    return self.list_query(name, lv, lty, args);
                }
            }
        }
        // Resolve a method: `Type.Method(..)` (static) or `recv.Method(..)`.
        let (key, this_arg): (String, Option<Expr>) = match &callee.kind {
            ast::ExprKind::Ident(name) => (name.clone(), None),
            ast::ExprKind::Member(recv, name) => {
                if let ast::ExprKind::Ident(obj) = &recv.kind {
                    let qualified = format!("{obj}.{name}");
                    if (self.cx.methods.contains_key(&qualified)
                        || self.cx.generics.contains_key(&qualified))
                        && self.is_type_name(obj)
                    {
                        (qualified, None)
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
        // A generic method: lower the arguments first (their types are what the
        // type parameters are inferred from), then instantiate.
        if self.cx.generics.contains_key(&key) {
            if this_arg.is_some() {
                return Err(format!(
                    "generic instance method `{key}` is not supported yet — make it static"
                ));
            }
            let mut lowered = Vec::new();
            for a in args {
                lowered.push(self.expr(a, None)?);
            }
            let sig = self.instantiate(&key, &lowered)?;
            let kargs = lowered.into_iter().map(|(e, _)| e).collect();
            return Ok((
                Expr::Call(Box::new(Call::Direct {
                    func: sig.fid,
                    args: kargs,
                })),
                sig.ret,
            ));
        }

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

    /// Append to a list held in `holder`: grow the buffer when it is full, then
    /// store and bump the length.
    fn list_add(&mut self, holder: LocalId, lty: TyId, val: Expr) {
        let (lrid, elem) = self.cx.as_list(lty).expect("a list");
        let l = || Expr::Local(holder);
        let len = || Expr::Field(Box::new(l()), LIST_LEN);
        let cap = || Expr::Field(Box::new(l()), LIST_CAP);
        let esize = self.cx.scalar_size(elem);
        let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
        let newcap = self.new_local("$newcap", TyTable::I32);
        let grow = vec![
            Stmt::If {
                cond: Expr::Bin(
                    BinOp::Eq,
                    Box::new(cap()),
                    Box::new(Expr::Int(0, TyTable::I32)),
                    TyTable::I32,
                ),
                then: vec![Stmt::Assign {
                    place: Place::Local(newcap),
                    value: Expr::Int(4, TyTable::I32),
                }],
                els: vec![Stmt::Assign {
                    place: Place::Local(newcap),
                    value: Expr::Bin(
                        BinOp::Mul,
                        Box::new(cap()),
                        Box::new(Expr::Int(2, TyTable::I32)),
                        TyTable::I32,
                    ),
                }],
            },
            Stmt::Assign {
                place: Place::Field(Box::new(l()), LIST_DATA),
                value: Expr::Call(Box::new(Call::Dll {
                    library: "c".into(),
                    symbol: "realloc".into(),
                    conv: CallConv::Cdecl,
                    args: vec![
                        Expr::Cast {
                            value: Box::new(Expr::Field(Box::new(l()), LIST_DATA)),
                            to: TyTable::PTR,
                        },
                        Expr::Cast {
                            value: Box::new(Expr::Bin(
                                BinOp::Mul,
                                Box::new(Expr::Local(newcap)),
                                Box::new(Expr::Int(esize as i128, TyTable::I32)),
                                TyTable::I32,
                            )),
                            to: TyTable::I64,
                        },
                    ],
                    arg_tys: vec![TyTable::PTR, TyTable::I64],
                    ret: data_ty,
                    varargs: false,
                })),
            },
            Stmt::Assign {
                place: Place::Field(Box::new(l()), LIST_CAP),
                value: Expr::Local(newcap),
            },
        ];
        self.push(Stmt::If {
            cond: Expr::Bin(BinOp::Eq, Box::new(len()), Box::new(cap()), TyTable::I32),
            then: grow,
            els: vec![],
        });
        self.push(Stmt::Assign {
            place: Place::Index(
                Box::new(Expr::Field(Box::new(l()), LIST_DATA)),
                Box::new(len()),
            ),
            value: val,
        });
        self.push(Stmt::Assign {
            place: Place::Field(Box::new(l()), LIST_LEN),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(len()),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
    }

    /// `Where` keeps the elements a predicate accepts; `Select` maps each one.
    /// The result type of a `Select` comes from probing the lambda body.
    fn list_query(
        &mut self,
        which: &str,
        src_val: Expr,
        src_ty: TyId,
        args: &[ast::Expr],
    ) -> Result<(Expr, TyId), String> {
        let (_, elem) = self.cx.as_list(src_ty).expect("a list");
        if args.len() != 1 {
            return Err(format!("List.{which} takes one argument"));
        }
        // The lambda's type: Where is T -> bool; Select is T -> R, and R is
        // found by lowering the body into a scratch block that is discarded.
        let out_elem = if which == "Where" {
            TyTable::BOOL
        } else {
            self.probe_lambda_result(&args[0], elem)?
        };
        let fn_ty = self.cx.b.m.types.intern(TyKind::Func {
            params: vec![elem],
            ret: out_elem,
        });
        let (f, _) = self.expr(&args[0], Some(fn_ty))?;
        let fl = self.new_local("$fn", fn_ty);
        self.push(Stmt::Let {
            local: fl,
            value: f,
        });

        let src = self.new_local("$src", src_ty);
        self.push(Stmt::Let {
            local: src,
            value: src_val,
        });
        let result_elem = if which == "Where" { elem } else { out_elem };
        let out_rid = self.cx.list_record(result_elem);
        let out_ty = self.cx.b.m.types.intern(TyKind::Record(out_rid));
        let out_data_ty = self.cx.b.m.record(out_rid).fields[LIST_DATA].ty;
        let out = self.new_local("$out", out_ty);
        self.push(Stmt::Let {
            local: out,
            value: Expr::MakeRecord(
                out_rid,
                vec![
                    Expr::Int(0, TyTable::I32),
                    Expr::Int(0, TyTable::I32),
                    Expr::Null(out_data_ty),
                ],
            ),
        });

        let i = self.new_local("$qi", TyTable::I32);
        self.push(Stmt::Let {
            local: i,
            value: Expr::Int(0, TyTable::I32),
        });
        let item = self.new_local("$qitem", elem);
        let call = |item_local: LocalId| {
            Expr::Call(Box::new(Call::Indirect {
                callee: Box::new(Expr::Local(fl)),
                args: vec![Expr::Local(item_local)],
                sig: fn_ty,
            }))
        };
        let mut inner = vec![
            Stmt::If {
                cond: Expr::Not(Box::new(Expr::Bin(
                    BinOp::Lt,
                    Box::new(Expr::Local(i)),
                    Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_LEN)),
                    TyTable::I32,
                ))),
                then: vec![Stmt::Break],
                els: vec![],
            },
            Stmt::Assign {
                place: Place::Local(item),
                value: Expr::Index(
                    Box::new(Expr::Field(Box::new(Expr::Local(src)), LIST_DATA)),
                    Box::new(Expr::Local(i)),
                ),
            },
        ];
        // The append is built in a scratch block so it can be nested.
        self.blocks.push(Vec::new());
        if which == "Where" {
            self.list_add(out, out_ty, Expr::Local(item));
            let add = self.blocks.pop().unwrap();
            inner.push(Stmt::If {
                cond: call(item),
                then: add,
                els: vec![],
            });
        } else {
            let mapped = self.new_local("$mapped", out_elem);
            self.push(Stmt::Let {
                local: mapped,
                value: call(item),
            });
            self.list_add(out, out_ty, Expr::Local(mapped));
            let add = self.blocks.pop().unwrap();
            inner.extend(add);
        }
        inner.push(Stmt::Assign {
            place: Place::Local(i),
            value: Expr::Bin(
                BinOp::Add,
                Box::new(Expr::Local(i)),
                Box::new(Expr::Int(1, TyTable::I32)),
                TyTable::I32,
            ),
        });
        self.push(Stmt::Loop { body: inner });
        Ok((Expr::Local(out), out_ty))
    }

    /// Lower a lambda body once into a discarded block just to learn its result
    /// type, so `Select` knows the element type of the list it produces.
    fn probe_lambda_result(&mut self, e: &ast::Expr, param_ty: TyId) -> Result<TyId, String> {
        let ast::ExprKind::Lambda(l) = &e.kind else {
            return Err("List.Select needs a lambda".into());
        };
        let ast::LambdaBody::Expr(body) = &l.body else {
            return Err("List.Select needs an expression lambda".into());
        };
        let saved_scope = self.scope.clone();
        let name = l.params.first().map(|(n, _)| n.clone()).unwrap_or_default();
        let tmp = self.new_local("$probe", param_ty);
        self.scope.insert(name, (tmp, param_ty));
        self.blocks.push(Vec::new());
        let r = self.expr(body, None);
        self.blocks.pop();
        self.scope = saved_scope;
        Ok(r?.1)
    }

    /// A `switch` expression: the subject is held once, then the arms become an
    /// if-chain assigning into one temporary.
    fn switch(
        &mut self,
        subject: &ast::Expr,
        arms: &[ast::SwitchArm],
        hint: Option<TyId>,
    ) -> Result<(Expr, TyId), String> {
        if arms.is_empty() {
            return Err("a `switch` expression needs at least one arm".into());
        }
        let (subj, sty) = self.expr(subject, None)?;
        let held = self.new_local("$switch", sty);
        self.push(Stmt::Let {
            local: held,
            value: subj,
        });
        // The result type comes from the first arm (or the caller's hint).
        let (first_val, rty) = {
            let saved = self.blocks.len();
            self.blocks.push(Vec::new());
            let r = self.expr(&arms[0].value, hint);
            self.blocks.truncate(saved);
            r?
        };
        let _ = first_val;
        let out = self.new_local("$case", rty);

        // Build the chain from the last arm backwards.
        let mut chain: Vec<Stmt> = Vec::new();
        let mut have_default = false;
        for arm in arms.iter().rev() {
            self.blocks.push(Vec::new());
            let val = self.expr(&arm.value, Some(rty));
            let mut body = self.blocks.pop().unwrap();
            let val = val?.0;
            body.push(Stmt::Assign {
                place: Place::Local(out),
                value: val,
            });
            match &arm.pat {
                ast::SwitchPat::Discard => {
                    have_default = true;
                    chain = body;
                }
                ast::SwitchPat::Const(c) => {
                    self.blocks.push(Vec::new());
                    let cv = self.expr(c, Some(sty));
                    let pre = self.blocks.pop().unwrap();
                    let cv = cv?.0;
                    let mut stmts = pre;
                    stmts.push(Stmt::If {
                        cond: Expr::Bin(BinOp::Eq, Box::new(Expr::Local(held)), Box::new(cv), sty),
                        then: body,
                        els: std::mem::take(&mut chain),
                    });
                    chain = stmts;
                }
                ast::SwitchPat::Relational(op, c) => {
                    self.blocks.push(Vec::new());
                    let cv = self.expr(c, Some(sty));
                    let pre = self.blocks.pop().unwrap();
                    let cv = cv?.0;
                    let kop = match op {
                        ast::BinOp::Lt => BinOp::Lt,
                        ast::BinOp::Le => BinOp::Le,
                        ast::BinOp::Gt => BinOp::Gt,
                        ast::BinOp::Ge => BinOp::Ge,
                        _ => return Err("unsupported relational pattern".into()),
                    };
                    let mut stmts = pre;
                    stmts.push(Stmt::If {
                        cond: Expr::Bin(kop, Box::new(Expr::Local(held)), Box::new(cv), sty),
                        then: body,
                        els: std::mem::take(&mut chain),
                    });
                    chain = stmts;
                }
            }
        }
        if !have_default {
            return Err("a `switch` expression needs a `_` arm".into());
        }
        for st in chain {
            self.push(st);
        }
        Ok((Expr::Local(out), rty))
    }

    /// Lift a lambda to its own function and build a closure value.
    ///
    /// The lifted function takes the environment pointer as parameter 0, which
    /// is the KIR closure convention; a non-capturing lambda passes `null` for
    /// it. Capturing lambdas need their captured locals hoisted into an env
    /// record at the point of declaration, which is a later step — until then a
    /// capture is reported rather than silently mis-compiled.
    fn lambda(&mut self, l: &ast::Lambda, want: TyId) -> Result<(Expr, TyId), String> {
        let (want_params, want_ret) = match self.tt().kind(want) {
            TyKind::Func { params, ret } => (params.clone(), *ret),
            _ => {
                return Err("a lambda's target type must be a `Func<...>` or `Action<...>`".into())
            }
        };
        if want_params.len() != l.params.len() {
            return Err(format!(
                "lambda takes {} parameter(s) but its target type expects {}",
                l.params.len(),
                want_params.len()
            ));
        }
        // Which enclosing variables does this lambda close over? Captured
        // locals already live in cells; a plain local here means a form of
        // capture the cell pass does not cover (a loop variable).
        let mut free = Vec::new();
        collect_idents_lambda(l, &mut free);
        let bound: Vec<&str> = l.params.iter().map(|(n, _)| n.as_str()).collect();
        let mut captures: Vec<(String, RecordId, TyId)> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for name in &free {
            if bound.contains(&name.as_str()) || seen.contains(name) {
                continue;
            }
            if let Some(c) = self.cells.get(name).cloned() {
                seen.push(name.clone());
                captures.push((name.clone(), c.rec, c.ty));
            } else if self.scope.contains_key(name) {
                return Err(format!(
                    "this lambda captures `{name}`, which is a loop or pattern variable; \
                     capturing those is not supported yet"
                ));
            }
        }

        // Declare the lifted function: env pointer first, then the parameters.
        let n = self.cx.b.m.funcs.len();
        let sym = format!("{}$lambda{n}", self.cx.b.m.name.replace('.', "_"));
        let mut params: Vec<(&str, TyId)> = vec![("$env", TyTable::PTR)];
        for ((name, _), ty) in l.params.iter().zip(want_params.iter()) {
            params.push((name.as_str(), *ty));
        }
        let fid = self.cx.b.declare_func(&sym, params, want_ret);

        // Queue the body; `this` binds parameter 0, the (unused) env pointer.
        let method = ast::Method {
            vis: ast::Vis::Private,
            is_static: true,
            name: sym.clone(),
            type_params: Vec::new(),
            params: l
                .params
                .iter()
                .map(|(nm, t)| ast::Param {
                    name: nm.clone(),
                    ty: t.clone().unwrap_or(ast::TypeRef::Void),
                    span: Default::default(),
                })
                .collect(),
            ret: ast::TypeRef::Void,
            body: match &l.body {
                ast::LambdaBody::Block(b) => b.clone(),
                ast::LambdaBody::Expr(_) => Vec::new(),
            },
            expr_body: match &l.body {
                ast::LambdaBody::Expr(e) => Some((**e).clone()),
                ast::LambdaBody::Block(_) => None,
            },
            doc: None,
            span: Default::default(),
        };
        // The environment: a record of pointers, one per captured cell. Both
        // sides reach the same cells, so a capture is by reference.
        let (env_expr, env_plan) = if captures.is_empty() {
            (Expr::Null(TyTable::PTR), None)
        } else {
            let rn = self.cx.b.m.records.len();
            let fields: Vec<(&str, TyId)> =
                captures.iter().map(|_| ("cell", TyTable::PTR)).collect();
            let env_rec = self
                .cx
                .b
                .c_record(&format!("$env{rn}"), fields, Equality::ByRef);
            let mut vals = Vec::new();
            for (name, _, _) in &captures {
                let c = self.cells[name].clone();
                vals.push(Expr::Cast {
                    value: Box::new(c.ptr),
                    to: TyTable::PTR,
                });
            }
            (
                Expr::MakeRecord(env_rec, vals),
                Some(EnvPlan {
                    rec: env_rec,
                    captures: captures.clone(),
                }),
            )
        };

        self.cx.pending.push(Pending {
            fid,
            method,
            tvars: self.cx.tvars.clone(),
            this: true,
            env: env_plan,
        });

        Ok((
            Expr::MakeClosure {
                func: fid,
                env: Box::new(env_expr),
            },
            want,
        ))
    }

    /// Monomorphise a generic method for the argument types at this call site.
    ///
    /// Type parameters are inferred by matching each declared parameter type
    /// against the lowered argument's type; the instance is cached, so the same
    /// type arguments produce one function with one mangled symbol.
    fn instantiate(&mut self, key: &str, args: &[(Expr, TyId)]) -> Result<Sig, String> {
        let t = self.cx.generics[key].clone();
        let m = &t.method;
        if args.len() != m.params.len() {
            return Err(format!(
                "`{key}` expects {} argument(s), got {}",
                m.params.len(),
                args.len()
            ));
        }
        // Infer.
        let mut tvars: HashMap<String, TyId> = HashMap::new();
        for (p, (_, aty)) in m.params.iter().zip(args.iter()) {
            unify(&p.ty, *aty, &m.type_params, &self.cx.b.m.types, &mut tvars)?;
        }
        let mut targs = Vec::new();
        for tp in &m.type_params {
            match tvars.get(tp) {
                Some(t) => targs.push(*t),
                None => {
                    return Err(format!(
                        "cannot infer type parameter `{tp}` of `{key}` from the arguments"
                    ))
                }
            }
        }
        let cache_key = (key.to_string(), targs.clone());
        if let Some(sig) = self.cx.mono.get(&cache_key) {
            return Ok(sig.clone());
        }

        // Declare the instance with the type parameters bound.
        let saved = std::mem::replace(&mut self.cx.tvars, tvars.clone());
        let result = (|| -> Result<Sig, String> {
            let ptys: Vec<TyId> = m
                .params
                .iter()
                .map(|p| self.cx.resolve(&p.ty))
                .collect::<Result<_, String>>()?;
            let ret = self.cx.resolve(&m.ret)?;
            let suffix: Vec<String> = targs.iter().map(|t| format!("{}", t.0)).collect();
            let sym = format!("{}_{}${}", t.owner, m.name, suffix.join("_"));
            let params: Vec<(&str, TyId)> = m
                .params
                .iter()
                .zip(ptys.iter())
                .map(|(p, t)| (p.name.as_str(), *t))
                .collect();
            let fid = self.cx.b.declare_func(&sym, params, ret);
            Ok(Sig {
                fid,
                this: false,
                params: ptys,
                ret,
            })
        })();
        self.cx.tvars = saved;
        let sig = result?;

        self.cx.mono.insert(cache_key, sig.clone());
        // The body is lowered by the driver, not here: this call is already
        // inside one function's lowering.
        self.cx.pending.push(Pending {
            fid: sig.fid,
            method: t.method.clone(),
            tvars,
            this: false,
            env: None,
        });
        Ok(sig)
    }

    fn is_type_name(&self, name: &str) -> bool {
        self.cx.type_names.contains(name)
            || self.cx.type_ids.contains_key(name)
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
        // `new List<T>()` starts empty; the buffer is allocated on first Add.
        if self.cx.as_list(ty).is_some() {
            let TyKind::Record(lrid) = *self.tt().kind(ty) else {
                unreachable!()
            };
            let data_ty = self.cx.b.m.record(lrid).fields[LIST_DATA].ty;
            return Ok((
                Expr::MakeRecord(
                    lrid,
                    vec![
                        Expr::Int(0, TyTable::I32),
                        Expr::Int(0, TyTable::I32),
                        Expr::Null(data_ty),
                    ],
                ),
                ty,
            ));
        }
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
        // Arithmetic happens on the value type; a `T?` target wraps the result
        // afterwards, so never let an optional become the operand type.
        let hint = hint.map(|h| match *self.tt().kind(h) {
            TyKind::Optional(inner) => inner,
            _ => h,
        });
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
        // `string + string` builds a new string rather than adding pointers.
        if op == ast::BinOp::Add {
            let probe = self.expr_raw(a, None)?;
            if probe.1 == TyTable::STR {
                let rhs = self.expr(b, Some(TyTable::STR))?;
                return Ok(self.build_string(vec![
                    (String::new(), Some(probe)),
                    (String::new(), Some(rhs)),
                ]));
            }
            // not a string: fall through, re-lowering `a` with the real hint
        }
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
                if let Some(c) = self.cells.get(name).cloned() {
                    return Ok(Place::Field(Box::new(c.ptr), 0));
                }
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

/// Names the lambdas inside a function body close over. An over-approximation:
/// it is every identifier any lambda mentions, so a local sharing a name is
/// celled needlessly — safe, and costing one indirection.
fn captured_names(
    body: &[ast::Stmt],
    expr_body: Option<&ast::Expr>,
) -> std::collections::HashSet<String> {
    let mut lambdas = Vec::new();
    for s in body {
        collect_lambdas_stmt(s, &mut lambdas);
    }
    if let Some(e) = expr_body {
        collect_lambdas_expr(e, &mut lambdas);
    }
    let mut out = std::collections::HashSet::new();
    for l in lambdas {
        let mut free = Vec::new();
        collect_idents_lambda(&l, &mut free);
        let bound: Vec<&str> = l.params.iter().map(|(n, _)| n.as_str()).collect();
        for n in free {
            if !bound.contains(&n.as_str()) {
                out.insert(n);
            }
        }
    }
    out
}

fn collect_lambdas_expr(e: &ast::Expr, out: &mut Vec<ast::Lambda>) {
    use ast::ExprKind as E;
    match &e.kind {
        E::Lambda(l) => {
            out.push(l.clone());
            // a nested lambda's captures matter to this function too
            match &l.body {
                ast::LambdaBody::Expr(x) => collect_lambdas_expr(x, out),
                ast::LambdaBody::Block(b) => {
                    for s in b {
                        collect_lambdas_stmt(s, out);
                    }
                }
            }
        }
        E::Member(b, _) => collect_lambdas_expr(b, out),
        E::Call(c, args) => {
            collect_lambdas_expr(c, out);
            for a in args {
                collect_lambdas_expr(a, out);
            }
        }
        E::Index(a, b) | E::Binary(_, a, b) | E::NullCoalesce(a, b) | E::Range(a, b, _) => {
            collect_lambdas_expr(a, out);
            collect_lambdas_expr(b, out);
        }
        E::Unary(_, a) | E::Cast(_, a) | E::Try(a) => collect_lambdas_expr(a, out),
        E::Ternary(a, b, c) => {
            collect_lambdas_expr(a, out);
            collect_lambdas_expr(b, out);
            collect_lambdas_expr(c, out);
        }
        E::New(_, args, inits) => {
            for a in args {
                collect_lambdas_expr(a, out);
            }
            for (_, v) in inits {
                collect_lambdas_expr(v, out);
            }
        }
        E::Interp(segs) => {
            for sg in segs {
                if let ast::InterpSeg::Expr(x) = sg {
                    collect_lambdas_expr(x, out);
                }
            }
        }
        E::Switch(subj, arms) => {
            collect_lambdas_expr(subj, out);
            for a in arms {
                collect_lambdas_expr(&a.value, out);
            }
        }
        E::Int(_)
        | E::Float(_, _)
        | E::Bool(_)
        | E::Str(_)
        | E::Char(_)
        | E::Null
        | E::Ident(_) => {}
    }
}

fn collect_lambdas_stmt(s: &ast::Stmt, out: &mut Vec<ast::Lambda>) {
    use ast::StmtKind as S;
    match &s.kind {
        S::Local { value, .. } => collect_lambdas_expr(value, out),
        S::Assign { target, value, .. } => {
            collect_lambdas_expr(target, out);
            collect_lambdas_expr(value, out);
        }
        S::Expr(e) => collect_lambdas_expr(e, out),
        S::Return(Some(e)) => collect_lambdas_expr(e, out),
        S::Return(None) | S::Break | S::Continue => {}
        S::If { cond, then, els } => {
            collect_lambdas_expr(cond, out);
            for x in then.iter().chain(els) {
                collect_lambdas_stmt(x, out);
            }
        }
        S::While { cond, body } => {
            collect_lambdas_expr(cond, out);
            for x in body {
                collect_lambdas_stmt(x, out);
            }
        }
        S::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init.as_ref() {
                collect_lambdas_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_lambdas_expr(c, out);
            }
            if let Some(st) = step.as_ref() {
                collect_lambdas_stmt(st, out);
            }
            for x in body {
                collect_lambdas_stmt(x, out);
            }
        }
        S::ForEach { coll, body, .. } => {
            collect_lambdas_expr(coll, out);
            for x in body {
                collect_lambdas_stmt(x, out);
            }
        }
        S::Defer(d) => collect_lambdas_stmt(d, out),
        S::Block(b) => {
            for x in b {
                collect_lambdas_stmt(x, out);
            }
        }
    }
}

/// Every identifier mentioned in a lambda body, so captures can be detected.
fn collect_idents_lambda(l: &ast::Lambda, out: &mut Vec<String>) {
    match &l.body {
        ast::LambdaBody::Expr(e) => collect_idents_expr(e, out),
        ast::LambdaBody::Block(b) => {
            for s in b {
                collect_idents_stmt(s, out);
            }
        }
    }
}

fn collect_idents_expr(e: &ast::Expr, out: &mut Vec<String>) {
    use ast::ExprKind as E;
    match &e.kind {
        E::Ident(n) => out.push(n.clone()),
        E::Member(b, _) => collect_idents_expr(b, out),
        E::Call(c, args) => {
            collect_idents_expr(c, out);
            for a in args {
                collect_idents_expr(a, out);
            }
        }
        E::Index(a, b) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
        }
        E::Unary(_, a) | E::Cast(_, a) | E::Try(a) => collect_idents_expr(a, out),
        E::Binary(_, a, b) | E::NullCoalesce(a, b) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
        }
        E::Range(a, b, _) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
        }
        E::Ternary(a, b, c) => {
            collect_idents_expr(a, out);
            collect_idents_expr(b, out);
            collect_idents_expr(c, out);
        }
        E::New(_, args, inits) => {
            for a in args {
                collect_idents_expr(a, out);
            }
            for (_, v) in inits {
                collect_idents_expr(v, out);
            }
        }
        E::Interp(segs) => {
            for s in segs {
                if let ast::InterpSeg::Expr(x) = s {
                    collect_idents_expr(x, out);
                }
            }
        }
        E::Lambda(inner) => collect_idents_lambda(inner, out),
        E::Switch(subj, arms) => {
            collect_idents_expr(subj, out);
            for a in arms {
                if let ast::SwitchPat::Const(c) | ast::SwitchPat::Relational(_, c) = &a.pat {
                    collect_idents_expr(c, out);
                }
                collect_idents_expr(&a.value, out);
            }
        }
        E::Int(_) | E::Float(_, _) | E::Bool(_) | E::Str(_) | E::Char(_) | E::Null => {}
    }
}

fn collect_idents_stmt(s: &ast::Stmt, out: &mut Vec<String>) {
    use ast::StmtKind as S;
    match &s.kind {
        S::Local { value, .. } => collect_idents_expr(value, out),
        S::Assign { target, value, .. } => {
            collect_idents_expr(target, out);
            collect_idents_expr(value, out);
        }
        S::Expr(e) => collect_idents_expr(e, out),
        S::Return(Some(e)) => collect_idents_expr(e, out),
        S::Return(None) | S::Break | S::Continue => {}
        S::If { cond, then, els } => {
            collect_idents_expr(cond, out);
            for x in then.iter().chain(els) {
                collect_idents_stmt(x, out);
            }
        }
        S::While { cond, body } => {
            collect_idents_expr(cond, out);
            for x in body {
                collect_idents_stmt(x, out);
            }
        }
        S::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init.as_ref() {
                collect_idents_stmt(i, out);
            }
            if let Some(c) = cond {
                collect_idents_expr(c, out);
            }
            if let Some(st) = step.as_ref() {
                collect_idents_stmt(st, out);
            }
            for x in body {
                collect_idents_stmt(x, out);
            }
        }
        S::ForEach { coll, body, .. } => {
            collect_idents_expr(coll, out);
            for x in body {
                collect_idents_stmt(x, out);
            }
        }
        S::Defer(d) => collect_idents_stmt(d, out),
        S::Block(b) => {
            for x in b {
                collect_idents_stmt(x, out);
            }
        }
    }
}

/// Match a declared parameter type against a concrete argument type, binding
/// any type parameter it names. Concrete positions are not checked here — the
/// instance body is type-checked when it is lowered.
fn unify(
    decl: &ast::TypeRef,
    actual: TyId,
    type_params: &[String],
    tt: &TyTable,
    out: &mut HashMap<String, TyId>,
) -> Result<(), String> {
    match decl {
        ast::TypeRef::Named(n) if type_params.iter().any(|p| p == n) => {
            match out.get(n) {
                Some(prev) if *prev != actual => {
                    return Err(format!("conflicting types inferred for `{n}`"))
                }
                _ => {
                    out.insert(n.clone(), actual);
                }
            }
            Ok(())
        }
        ast::TypeRef::Array(_) | ast::TypeRef::Generic(_, _)
            if matches!(tt.kind(actual), TyKind::Array(_)) =>
        {
            let elem = match tt.kind(actual) {
                TyKind::Array(e) => *e,
                _ => unreachable!(),
            };
            let decl_elem = match decl {
                ast::TypeRef::Array(inner) => inner.as_ref(),
                ast::TypeRef::Generic(_, args) if !args.is_empty() => &args[0],
                _ => return Ok(()),
            };
            unify(decl_elem, elem, type_params, tt, out)
        }
        ast::TypeRef::Optional(inner) => {
            if let TyKind::Optional(t) = tt.kind(actual) {
                return unify(inner, *t, type_params, tt, out);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// For `x != null` returns `(Some(x), None)`; for `x == null`, `(None, Some(x))`
/// — the branch in which `x` is known to hold a value.
fn null_test_target(cond: &ast::Expr) -> (Option<String>, Option<String>) {
    if let ast::ExprKind::Binary(op, a, b) = &cond.kind {
        if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
            let name = match (&a.kind, &b.kind) {
                (ast::ExprKind::Ident(n), ast::ExprKind::Null) => Some(n.clone()),
                (ast::ExprKind::Null, ast::ExprKind::Ident(n)) => Some(n.clone()),
                _ => None,
            };
            if let Some(n) = name {
                return if *op == ast::BinOp::Ne {
                    (Some(n), None)
                } else {
                    (None, Some(n))
                };
            }
        }
    }
    (None, None)
}

/// A zero/null value of a type — the unused `value` slot of a failed `Result`.
fn zero_of(tt: &TyTable, ty: TyId) -> Expr {
    match tt.kind(ty) {
        TyKind::F32 | TyKind::F64 => Expr::Float(0.0, ty),
        TyKind::Bool => Expr::Bool(false),
        TyKind::Str => Expr::Str(String::new()),
        k if matches!(
            k,
            TyKind::Ptr
                | TyKind::Bytes
                | TyKind::Record(_)
                | TyKind::Array(_)
                | TyKind::Dict(..)
                | TyKind::Set(_)
        ) =>
        {
            Expr::Null(ty)
        }
        _ => Expr::Int(0, ty),
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
